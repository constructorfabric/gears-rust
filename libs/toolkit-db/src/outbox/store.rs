use sea_orm::{ConnectionTrait, DbBackend, DbErr, Statement};

use super::dialect::{AllocSql, ClaimSql, Dialect, VacuumSql};
use super::statements::{MySqlIdReservationStatements, OutboxStatements};
use super::tables::OutboxTables;

/// Runtime SQL context for one outbox table family on one database backend.
///
pub(super) struct OutboxStore<'a> {
    statements: &'a OutboxStatements,
}

impl<'a> OutboxStore<'a> {
    pub(super) fn new(statements: &'a OutboxStatements) -> Self {
        Self { statements }
    }

    pub(super) fn backend(&self) -> DbBackend {
        self.statements.backend()
    }

    pub(super) fn dialect(&self) -> Dialect {
        self.statements.dialect()
    }

    pub(super) fn tables(&self) -> &OutboxTables {
        self.statements.tables()
    }

    pub(super) fn register_queue_select(&self) -> &str {
        self.statements.registration().register_queue_select()
    }

    pub(super) fn register_queue_insert(&self) -> &str {
        self.statements.registration().register_queue_insert()
    }

    pub(super) async fn exec_insert_body_batch(
        &self,
        conn: &dyn ConnectionTrait,
        payloads: &[(&[u8], &str)],
    ) -> Result<Vec<i64>, DbErr> {
        if payloads.is_empty() {
            return Ok(Vec::new());
        }
        if self.backend() == DbBackend::MySql {
            let reservation = self
                .statements
                .enqueue()
                .body_id_reservation()
                .ok_or_else(|| DbErr::Custom("missing MySQL body ID reservation SQL".to_owned()))?;
            let ids = self
                .reserve_mysql_ids(conn, reservation, payloads.len(), "body")
                .await?;
            let sql = self.build_insert_body_batch_with_ids(payloads.len());
            let mut values: Vec<sea_orm::Value> = Vec::with_capacity(payloads.len() * 3);
            for (id, &(payload, payload_type)) in ids.iter().zip(payloads) {
                values.push((*id).into());
                values.push(payload.to_vec().into());
                values.push(payload_type.into());
            }
            conn.execute(Statement::from_sql_and_values(self.backend(), &sql, values))
                .await?;
            return Ok(ids);
        }

        let sql = self
            .dialect()
            .build_insert_body_batch(self.tables(), payloads.len());
        let mut values: Vec<sea_orm::Value> = Vec::with_capacity(payloads.len() * 2);
        for &(payload, payload_type) in payloads {
            values.push(payload.to_vec().into());
            values.push(payload_type.into());
        }

        if self.dialect().supports_returning() {
            let rows = conn
                .query_all(Statement::from_sql_and_values(self.backend(), &sql, values))
                .await?;
            rows.iter()
                .map(|r| {
                    r.try_get_by_index(0)
                        .map_err(|e| DbErr::Custom(format!("body id column: {e}")))
                })
                .collect()
        } else {
            conn.execute(Statement::from_sql_and_values(self.backend(), &sql, values))
                .await?;
            let row = conn
                .query_one(Statement::from_string(
                    self.backend(),
                    Dialect::last_insert_id(),
                ))
                .await?
                .ok_or_else(|| {
                    DbErr::Custom("LAST_INSERT_ID() returned no row for body batch".to_owned())
                })?;
            let first_id: i64 = row
                .try_get_by_index(0)
                .map_err(|e| DbErr::Custom(format!("body first_id column: {e}")))?;
            #[allow(clippy::cast_possible_wrap)]
            Ok((0..payloads.len() as i64).map(|i| first_id + i).collect())
        }
    }

    pub(super) async fn exec_insert_incoming_batch(
        &self,
        conn: &dyn ConnectionTrait,
        entries: &[(i64, i64)],
    ) -> Result<Vec<i64>, DbErr> {
        if entries.is_empty() {
            return Ok(Vec::new());
        }
        if self.backend() == DbBackend::MySql {
            let reservation = self
                .statements
                .enqueue()
                .incoming_id_reservation()
                .ok_or_else(|| {
                    DbErr::Custom("missing MySQL incoming ID reservation SQL".to_owned())
                })?;
            let ids = self
                .reserve_mysql_ids(conn, reservation, entries.len(), "incoming")
                .await?;
            let sql = self.build_insert_incoming_batch_with_ids(entries.len());
            let mut values: Vec<sea_orm::Value> = Vec::with_capacity(entries.len() * 3);
            for (id, &(partition_id, body_id)) in ids.iter().zip(entries) {
                values.push((*id).into());
                values.push(partition_id.into());
                values.push(body_id.into());
            }
            conn.execute(Statement::from_sql_and_values(self.backend(), &sql, values))
                .await?;
            return Ok(ids);
        }

        let sql = self
            .dialect()
            .build_insert_incoming_batch(self.tables(), entries.len());
        let mut values: Vec<sea_orm::Value> = Vec::with_capacity(entries.len() * 2);
        for &(partition_id, body_id) in entries {
            values.push(partition_id.into());
            values.push(body_id.into());
        }

        if self.dialect().supports_returning() {
            let rows = conn
                .query_all(Statement::from_sql_and_values(self.backend(), &sql, values))
                .await?;
            rows.iter()
                .map(|r| {
                    r.try_get_by_index(0)
                        .map_err(|e| DbErr::Custom(format!("incoming id column: {e}")))
                })
                .collect()
        } else {
            conn.execute(Statement::from_sql_and_values(self.backend(), &sql, values))
                .await?;
            let row = conn
                .query_one(Statement::from_string(
                    self.backend(),
                    Dialect::last_insert_id(),
                ))
                .await?
                .ok_or_else(|| {
                    DbErr::Custom("LAST_INSERT_ID() returned no row for incoming batch".to_owned())
                })?;
            let first_id: i64 = row
                .try_get_by_index(0)
                .map_err(|e| DbErr::Custom(format!("incoming first_id column: {e}")))?;
            #[allow(clippy::cast_possible_wrap)]
            Ok((0..entries.len() as i64).map(|i| first_id + i).collect())
        }
    }

    /// Execute an INSERT and return the generated `id` column.
    ///
    /// Encapsulates RETURNING (Postgres/SQLite) vs `LAST_INSERT_ID` (`MySQL`).
    async fn exec_insert_returning_id(
        &self,
        conn: &dyn ConnectionTrait,
        sql: &str,
        params: Vec<sea_orm::Value>,
        context: &str,
    ) -> Result<i64, DbErr> {
        if self.dialect().supports_returning() {
            let row = conn
                .query_one(Statement::from_sql_and_values(self.backend(), sql, params))
                .await?
                .ok_or_else(|| {
                    DbErr::Custom(format!("INSERT RETURNING returned no row for {context}"))
                })?;
            row.try_get_by_index(0)
                .map_err(|e| DbErr::Custom(format!("{context} id column: {e}")))
        } else {
            conn.execute(Statement::from_sql_and_values(self.backend(), sql, params))
                .await?;
            let row = conn
                .query_one(Statement::from_string(
                    self.backend(),
                    Dialect::last_insert_id(),
                ))
                .await?
                .ok_or_else(|| {
                    DbErr::Custom(format!("LAST_INSERT_ID() returned no row for {context}"))
                })?;
            row.try_get_by_index(0)
                .map_err(|e| DbErr::Custom(format!("{context} id column: {e}")))
        }
    }

    /// Execute a single body INSERT and return the generated ID.
    async fn exec_insert_body(
        &self,
        conn: &dyn ConnectionTrait,
        payload: Vec<u8>,
        payload_type: &str,
    ) -> Result<i64, DbErr> {
        if self.backend() == DbBackend::MySql {
            let payloads = [(payload.as_slice(), payload_type)];
            let mut ids = self.exec_insert_body_batch(conn, &payloads).await?;
            return ids
                .pop()
                .ok_or_else(|| DbErr::Custom("body insert returned no id".to_owned()));
        }

        let sql = self.statements.enqueue().insert_body();
        self.exec_insert_returning_id(conn, sql, vec![payload.into(), payload_type.into()], "body")
            .await
    }

    /// Execute a single incoming INSERT and return the generated ID.
    async fn exec_insert_incoming(
        &self,
        conn: &dyn ConnectionTrait,
        partition_id: i64,
        body_id: i64,
    ) -> Result<i64, DbErr> {
        if self.backend() == DbBackend::MySql {
            let entries = [(partition_id, body_id)];
            let mut ids = self.exec_insert_incoming_batch(conn, &entries).await?;
            return ids
                .pop()
                .ok_or_else(|| DbErr::Custom("incoming insert returned no id".to_owned()));
        }

        let sql = self.statements.enqueue().insert_incoming();
        self.exec_insert_returning_id(
            conn,
            sql,
            vec![partition_id.into(), body_id.into()],
            "incoming",
        )
        .await
    }

    pub(super) async fn exec_insert_body_and_incoming(
        &self,
        conn: &dyn ConnectionTrait,
        partition_id: i64,
        payload: Vec<u8>,
        payload_type: &str,
    ) -> Result<i64, DbErr> {
        if let Some(cte) = self.statements.enqueue().insert_body_and_incoming_cte() {
            self.exec_insert_returning_id(
                conn,
                cte,
                vec![payload.into(), payload_type.into(), partition_id.into()],
                "incoming",
            )
            .await
        } else {
            // MySQL: two separate round-trips (no CTE INSERT support).
            let body_id = self.exec_insert_body(conn, payload, payload_type).await?;
            self.exec_insert_incoming(conn, partition_id, body_id).await
        }
    }

    pub(super) fn lock_partition(&self) -> Option<&str> {
        self.statements.sequencer().lock_partition()
    }

    pub(super) fn discover_dirty_partitions(&self) -> &str {
        self.statements.sequencer().discover_dirty_partitions()
    }

    pub(super) fn insert_processor_row(&self) -> &str {
        self.statements.processor().insert_processor_row()
    }

    pub(super) fn lock_processor(&self) -> Option<&str> {
        self.statements.processor().lock_processor()
    }

    pub(super) fn read_outgoing_batch(&self, batch_size: u32) -> String {
        self.dialect()
            .read_outgoing_batch(self.tables(), batch_size)
    }

    pub(super) fn build_read_body_batch(&self, count: usize) -> String {
        self.dialect().build_read_body_batch(self.tables(), count)
    }

    pub(super) fn advance_processed_seq(&self) -> &str {
        self.statements.processor().advance_processed_seq()
    }

    pub(super) fn record_retry(&self) -> &str {
        self.statements.processor().record_retry()
    }

    pub(super) fn insert_dead_letter(&self) -> &str {
        self.statements.processor().insert_dead_letter()
    }

    pub(super) fn lease_ack_advance(&self) -> &str {
        self.statements.processor().lease_ack_advance()
    }

    pub(super) fn lease_record_retry(&self) -> &str {
        self.statements.processor().lease_record_retry()
    }

    pub(super) fn lease_release(&self) -> &str {
        self.statements.processor().lease_release()
    }

    pub(super) async fn exec_lease_acquire(
        &self,
        conn: &dyn ConnectionTrait,
        lease_id: &str,
        lease_secs: i64,
        partition_id: i64,
    ) -> Result<Option<(i64, i16)>, DbErr> {
        let lease_acquire = self.statements.processor().lease_acquire();
        if self.dialect().supports_returning() {
            let row = conn
                .query_one(Statement::from_sql_and_values(
                    self.backend(),
                    lease_acquire,
                    [lease_id.into(), lease_secs.into(), partition_id.into()],
                ))
                .await?;
            match row {
                Some(r) => {
                    let processed_seq: i64 = r
                        .try_get_by_index(0)
                        .map_err(|e| DbErr::Custom(format!("processed_seq column: {e}")))?;
                    let attempts: i16 = r
                        .try_get_by_index(1)
                        .map_err(|e| DbErr::Custom(format!("attempts column: {e}")))?;
                    Ok(Some((processed_seq, attempts)))
                }
                None => Ok(None),
            }
        } else {
            let result = conn
                .execute(Statement::from_sql_and_values(
                    self.backend(),
                    lease_acquire,
                    [lease_id.into(), lease_secs.into(), partition_id.into()],
                ))
                .await?;
            if result.rows_affected() == 0 {
                return Ok(None);
            }
            let read_processor = self.read_processor();
            let row = conn
                .query_one(Statement::from_sql_and_values(
                    self.backend(),
                    read_processor,
                    [partition_id.into()],
                ))
                .await?;
            match row {
                Some(r) => {
                    let processed_seq: i64 = r
                        .try_get_by_index(0)
                        .map_err(|e| DbErr::Custom(format!("processed_seq column: {e}")))?;
                    let attempts: i16 = r
                        .try_get_by_index(1)
                        .map_err(|e| DbErr::Custom(format!("attempts column: {e}")))?;
                    Ok(Some((processed_seq, attempts)))
                }
                None => Ok(None),
            }
        }
    }

    pub(super) fn claim_incoming(&self, batch_size: u32) -> ClaimSql {
        self.dialect().claim_incoming(self.tables(), batch_size)
    }

    pub(super) fn delete_incoming_batch(&self, count: usize) -> String {
        self.dialect().delete_incoming_batch(self.tables(), count)
    }

    pub(super) fn allocate_sequences(&self) -> &AllocSql {
        self.statements.sequencer().allocate_sequences()
    }

    pub(super) fn build_insert_outgoing_batch(&self, count: usize) -> String {
        self.dialect()
            .build_insert_outgoing_batch(self.tables(), count)
    }

    pub(super) fn vacuum_cleanup(&self) -> &VacuumSql {
        self.statements.vacuum().cleanup()
    }

    pub(super) fn build_delete_outgoing_batch(&self, count: usize) -> String {
        self.dialect()
            .build_delete_outgoing_batch(self.tables(), count)
    }

    pub(super) fn build_delete_body_batch(&self, count: usize) -> String {
        self.dialect().build_delete_body_batch(self.tables(), count)
    }

    pub(super) fn read_processor(&self) -> &str {
        self.statements.processor().read_processor()
    }

    pub(super) fn bump_vacuum_counter(&self) -> &str {
        self.statements.vacuum().bump_counter()
    }

    pub(super) fn fetch_dirty_partitions(&self) -> &str {
        self.statements.vacuum().fetch_dirty_partitions()
    }

    pub(super) fn decrement_vacuum_counter(&self) -> &str {
        self.statements.vacuum().decrement_counter()
    }

    #[cfg(test)]
    pub(super) fn reset_vacuum_counter(&self) -> &str {
        self.statements.vacuum().reset_counter()
    }

    pub(super) fn insert_vacuum_counter_row(&self) -> &str {
        self.statements.registration().insert_vacuum_counter_row()
    }

    pub(super) fn dead_letter_select_columns(&self) -> &str {
        self.statements.dead_letters().select_columns()
    }

    pub(super) fn dead_letter_count_base(&self) -> &str {
        self.statements.dead_letters().count_base()
    }

    pub(super) fn dead_letter_id_select_base(&self) -> &str {
        self.statements.dead_letters().id_select_base()
    }

    async fn reserve_mysql_ids(
        &self,
        conn: &dyn ConnectionTrait,
        reservation: &MySqlIdReservationStatements,
        count: usize,
        context: &str,
    ) -> Result<Vec<i64>, DbErr> {
        let count = i64::try_from(count)
            .map_err(|e| DbErr::Custom(format!("{context} batch too large: {e}")))?;
        let row = conn
            .query_one(Statement::from_string(
                self.backend(),
                reservation.select_next_id_for_update(),
            ))
            .await?
            .ok_or_else(|| {
                DbErr::Custom(format!(
                    "MySQL {context} ID sequence table returned no singleton row"
                ))
            })?;
        let first_id: i64 = row
            .try_get_by_index(0)
            .map_err(|e| DbErr::Custom(format!("{context} next_id column: {e}")))?;

        conn.execute(Statement::from_sql_and_values(
            self.backend(),
            reservation.advance_next_id(),
            [count.into()],
        ))
        .await?;

        Ok((0..count).map(|offset| first_id + offset).collect())
    }

    fn build_insert_body_batch_with_ids(&self, count: usize) -> String {
        let mut sql = format!(
            "INSERT INTO {} (id, payload, payload_type) VALUES ",
            self.tables().body()
        );
        append_mysql_value_tuples(&mut sql, count, 3);
        sql
    }

    fn build_insert_incoming_batch_with_ids(&self, count: usize) -> String {
        let mut sql = format!(
            "INSERT INTO {} (id, partition_id, body_id) VALUES ",
            self.tables().incoming()
        );
        append_mysql_value_tuples(&mut sql, count, 3);
        sql
    }
}

fn append_mysql_value_tuples(sql: &mut String, row_count: usize, cols: usize) {
    for row in 0..row_count {
        if row > 0 {
            sql.push_str(", ");
        }
        sql.push('(');
        for col in 0..cols {
            if col > 0 {
                sql.push_str(", ");
            }
            sql.push('?');
        }
        sql.push(')');
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use sea_orm::DbBackend;

    use super::*;
    use crate::outbox::tables::OutboxTables;

    fn mysql_store() -> OutboxStore<'static> {
        let tables = OutboxTables::default();
        let statements = OutboxStatements::new(DbBackend::MySql, &tables);
        let leaked = Box::leak(Box::new(statements));
        OutboxStore::new(leaked)
    }

    #[test]
    fn mysql_explicit_body_insert_includes_ids() {
        let store = mysql_store();
        let sql = store.build_insert_body_batch_with_ids(2);

        assert_eq!(
            sql,
            "INSERT INTO toolkit_outbox_body (id, payload, payload_type) VALUES (?, ?, ?), (?, ?, ?)"
        );
    }

    #[test]
    fn mysql_explicit_incoming_insert_includes_ids() {
        let store = mysql_store();
        let sql = store.build_insert_incoming_batch_with_ids(2);

        assert_eq!(
            sql,
            "INSERT INTO toolkit_outbox_incoming (id, partition_id, body_id) VALUES (?, ?, ?), (?, ?, ?)"
        );
    }
}
