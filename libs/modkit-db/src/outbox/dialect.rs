use std::fmt::Write as _;

use sea_orm::{ConnectionTrait, DbBackend, DbErr, Statement};

/// Backend-specific SQL dialect for the outbox module.
///
/// Centralizes all DML differences between `Postgres`, `SQLite`, and `MySQL`
/// so that `core.rs` and `sequencer.rs` contain zero `match backend` blocks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dialect {
    Postgres,
    /// `SQLite`: single-process only. No row-level locking — `lock_partition()`
    /// and `lock_processor()` return `None`. Do not run multiple outbox
    /// instances against the same `SQLite` database.
    Sqlite,
    /// `MySQL` 8.0+ required. Uses `FOR UPDATE SKIP LOCKED` for partition
    /// locking and sequencer claims, which is not available in `MySQL` 5.7
    /// or earlier.
    MySql,
}

impl From<DbBackend> for Dialect {
    fn from(backend: DbBackend) -> Self {
        match backend {
            DbBackend::Postgres => Self::Postgres,
            DbBackend::Sqlite => Self::Sqlite,
            DbBackend::MySql => Self::MySql,
        }
    }
}

/// SQL for the vacuum's bounded-chunk cleanup operation.
///
/// Strategy: SELECT a bounded chunk of (id, `body_id`) from outgoing, then
/// DELETE those outgoing rows by ID, then DELETE body rows by ID.
/// The caller loops while `deleted == batch_size` (more work likely).
pub struct VacuumSql {
    /// SELECT id, `body_id` with LIMIT for bounded chunk deletion.
    /// Parameters: `partition_id`, `processed_seq`, limit.
    pub select_outgoing_chunk: &'static str,
}

/// SQL for the sequencer's claim-incoming operation.
///
/// All backends use SELECT-then-DELETE to guarantee FIFO ordering:
/// the SELECT returns rows ordered by `id`, and the sequencer assigns
/// sequences in that order before deleting.
pub struct ClaimSql {
    /// SELECT query that returns `id, body_id` ordered by `id`.
    /// Pg/MySQL append `FOR UPDATE`; `SQLite` omits it (no row locking).
    pub select: String,
}

/// SQL for the sequencer's sequence-allocation operation.
pub enum AllocSql {
    /// `Pg`/`SQLite`: single `UPDATE ... RETURNING` statement.
    UpdateReturning(&'static str),
    /// `MySQL`: `UPDATE` then `SELECT` as two separate statements.
    UpdateThenSelect {
        update: &'static str,
        select: &'static str,
    },
}

// -- Registration queries --

impl Dialect {
    pub fn register_queue_select(self) -> &'static str {
        match self {
            Self::Postgres | Self::Sqlite => {
                "SELECT id FROM modkit_outbox_partitions \
                 WHERE queue = $1 ORDER BY partition ASC"
            }
            Self::MySql => {
                "SELECT id FROM modkit_outbox_partitions \
                 WHERE queue = ? ORDER BY `partition` ASC"
            }
        }
    }

    pub fn register_queue_insert(self) -> &'static str {
        match self {
            Self::Postgres => {
                "INSERT INTO modkit_outbox_partitions (queue, partition) \
                 VALUES ($1, $2) ON CONFLICT (queue, partition) DO NOTHING"
            }
            Self::Sqlite => {
                "INSERT OR IGNORE INTO modkit_outbox_partitions (queue, partition) \
                 VALUES ($1, $2)"
            }
            Self::MySql => {
                "INSERT IGNORE INTO modkit_outbox_partitions (queue, `partition`) \
                 VALUES (?, ?)"
            }
        }
    }
}

// -- Single-row insert queries --

impl Dialect {
    /// Combined CTE: insert body + incoming in a single round-trip.
    /// Returns the incoming row id. Only for backends that support RETURNING.
    pub fn insert_body_and_incoming_cte(self) -> Option<&'static str> {
        match self {
            Self::Postgres => Some(
                "WITH b AS (\
                   INSERT INTO modkit_outbox_body (payload, payload_type) \
                   VALUES ($1, $2) RETURNING id\
                 ) \
                 INSERT INTO modkit_outbox_incoming (partition_id, body_id) \
                 SELECT $3, id FROM b RETURNING id",
            ),
            // SQLite: writable CTEs require 3.35+; the bundled libsqlite3
            // version may be older, so fall back to two separate INSERTs.
            Self::Sqlite | Self::MySql => None,
        }
    }

    pub fn insert_body(self) -> &'static str {
        match self {
            Self::Postgres | Self::Sqlite => {
                "INSERT INTO modkit_outbox_body (payload, payload_type) \
                 VALUES ($1, $2) RETURNING id"
            }
            Self::MySql => {
                "INSERT INTO modkit_outbox_body (payload, payload_type) \
                 VALUES (?, ?)"
            }
        }
    }

    pub fn insert_incoming(self) -> &'static str {
        match self {
            Self::Postgres | Self::Sqlite => {
                "INSERT INTO modkit_outbox_incoming (partition_id, body_id) \
                 VALUES ($1, $2) RETURNING id"
            }
            Self::MySql => {
                "INSERT INTO modkit_outbox_incoming (partition_id, body_id) \
                 VALUES (?, ?)"
            }
        }
    }

    fn supports_returning(self) -> bool {
        match self {
            Self::Postgres | Self::Sqlite => true,
            Self::MySql => false,
        }
    }

    /// Returns the `MySQL` query to retrieve the last auto-generated ID.
    fn last_insert_id() -> &'static str {
        "SELECT CAST(LAST_INSERT_ID() AS SIGNED) AS id"
    }
}

// -- Batch insert builders --

impl Dialect {
    /// Build a multi-row INSERT for body rows.
    ///
    /// `MySQL` note: consecutive auto-increment IDs are guaranteed by `InnoDB`
    /// for a single multi-row INSERT when `innodb_autoinc_lock_mode` is 0 or 1.
    pub fn build_insert_body_batch(self, count: usize) -> String {
        let mut sql =
            String::from("INSERT INTO modkit_outbox_body (payload, payload_type) VALUES ");
        self.append_value_tuples(&mut sql, count, 2);
        if self.supports_returning() {
            sql.push_str(" RETURNING id");
        }
        sql
    }

    pub fn build_insert_incoming_batch(self, count: usize) -> String {
        let mut sql =
            String::from("INSERT INTO modkit_outbox_incoming (partition_id, body_id) VALUES ");
        self.append_value_tuples(&mut sql, count, 2);
        if self.supports_returning() {
            sql.push_str(" RETURNING id");
        }
        sql
    }

    /// Build `SELECT id, payload, payload_type, created_at FROM modkit_outbox_body WHERE id IN (...)`.
    pub fn build_read_body_batch(self, count: usize) -> String {
        let mut sql = String::from(
            "SELECT id, payload, payload_type, created_at FROM modkit_outbox_body WHERE id IN (",
        );
        self.append_in_placeholders(&mut sql, count);
        sql.push(')');
        sql
    }

    /// Append `$1, $2, ...` or `?, ?, ...` placeholders for an IN clause.
    fn append_in_placeholders(self, sql: &mut String, count: usize) {
        for i in 0..count {
            if i > 0 {
                sql.push_str(", ");
            }
            match self {
                Self::Postgres | Self::Sqlite => {
                    #[allow(clippy::let_underscore_must_use)]
                    let _ = write!(sql, "${}", i + 1);
                }
                Self::MySql => {
                    sql.push('?');
                }
            }
        }
    }

    /// Append `(p1, p2), (p3, p4), ...` with correct placeholder style.
    fn append_value_tuples(self, sql: &mut String, row_count: usize, cols: usize) {
        for i in 0..row_count {
            if i > 0 {
                sql.push_str(", ");
            }
            sql.push('(');
            for c in 0..cols {
                if c > 0 {
                    sql.push_str(", ");
                }
                match self {
                    Self::Postgres | Self::Sqlite => {
                        let idx = i * cols + c + 1;
                        // Writing to a String is infallible.
                        #[allow(clippy::let_underscore_must_use)]
                        let _ = write!(sql, "${idx}");
                    }
                    Self::MySql => {
                        sql.push('?');
                    }
                }
            }
            sql.push(')');
        }
    }
}

// -- Insert execution helpers --
//
// Encapsulate the RETURNING vs LAST_INSERT_ID branching so callers
// never need to check `supports_returning()`.

impl Dialect {
    /// Execute a multi-row body INSERT and return generated IDs.
    pub async fn exec_insert_body_batch(
        self,
        conn: &dyn ConnectionTrait,
        backend: DbBackend,
        payloads: &[(&[u8], &str)],
    ) -> Result<Vec<i64>, DbErr> {
        if payloads.is_empty() {
            return Ok(Vec::new());
        }
        let sql = self.build_insert_body_batch(payloads.len());
        let mut values: Vec<sea_orm::Value> = Vec::with_capacity(payloads.len() * 2);
        for &(payload, payload_type) in payloads {
            values.push(payload.to_vec().into());
            values.push(payload_type.into());
        }

        if self.supports_returning() {
            let rows = conn
                .query_all(Statement::from_sql_and_values(backend, &sql, values))
                .await?;
            rows.iter()
                .map(|r| {
                    r.try_get_by_index(0)
                        .map_err(|e| DbErr::Custom(format!("body id column: {e}")))
                })
                .collect()
        } else {
            conn.execute(Statement::from_sql_and_values(backend, &sql, values))
                .await?;
            let row = conn
                .query_one(Statement::from_string(backend, Self::last_insert_id()))
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

    /// Execute a multi-row incoming INSERT and return generated IDs.
    pub async fn exec_insert_incoming_batch(
        self,
        conn: &dyn ConnectionTrait,
        backend: DbBackend,
        entries: &[(i64, i64)],
    ) -> Result<Vec<i64>, DbErr> {
        if entries.is_empty() {
            return Ok(Vec::new());
        }
        let sql = self.build_insert_incoming_batch(entries.len());
        let mut values: Vec<sea_orm::Value> = Vec::with_capacity(entries.len() * 2);
        for &(partition_id, body_id) in entries {
            values.push(partition_id.into());
            values.push(body_id.into());
        }

        if self.supports_returning() {
            let rows = conn
                .query_all(Statement::from_sql_and_values(backend, &sql, values))
                .await?;
            rows.iter()
                .map(|r| {
                    r.try_get_by_index(0)
                        .map_err(|e| DbErr::Custom(format!("incoming id column: {e}")))
                })
                .collect()
        } else {
            conn.execute(Statement::from_sql_and_values(backend, &sql, values))
                .await?;
            let row = conn
                .query_one(Statement::from_string(backend, Self::last_insert_id()))
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
        self,
        conn: &dyn ConnectionTrait,
        backend: DbBackend,
        sql: &str,
        params: Vec<sea_orm::Value>,
        context: &str,
    ) -> Result<i64, DbErr> {
        if self.supports_returning() {
            let row = conn
                .query_one(Statement::from_sql_and_values(backend, sql, params))
                .await?
                .ok_or_else(|| {
                    DbErr::Custom(format!("INSERT RETURNING returned no row for {context}"))
                })?;
            row.try_get_by_index(0)
                .map_err(|e| DbErr::Custom(format!("{context} id column: {e}")))
        } else {
            conn.execute(Statement::from_sql_and_values(backend, sql, params))
                .await?;
            let row = conn
                .query_one(Statement::from_string(backend, Self::last_insert_id()))
                .await?
                .ok_or_else(|| {
                    DbErr::Custom(format!("LAST_INSERT_ID() returned no row for {context}"))
                })?;
            row.try_get_by_index(0)
                .map_err(|e| DbErr::Custom(format!("{context} id column: {e}")))
        }
    }

    /// Execute a single body INSERT and return the generated ID.
    pub async fn exec_insert_body(
        self,
        conn: &dyn ConnectionTrait,
        backend: DbBackend,
        payload: Vec<u8>,
        payload_type: &str,
    ) -> Result<i64, DbErr> {
        self.exec_insert_returning_id(
            conn,
            backend,
            self.insert_body(),
            vec![payload.into(), payload_type.into()],
            "body",
        )
        .await
    }

    /// Execute a single incoming INSERT and return the generated ID.
    pub async fn exec_insert_incoming(
        self,
        conn: &dyn ConnectionTrait,
        backend: DbBackend,
        partition_id: i64,
        body_id: i64,
    ) -> Result<i64, DbErr> {
        self.exec_insert_returning_id(
            conn,
            backend,
            self.insert_incoming(),
            vec![partition_id.into(), body_id.into()],
            "incoming",
        )
        .await
    }

    /// Execute combined CTE: insert body + incoming in one round-trip.
    /// Falls back to two separate inserts on `MySQL`.
    pub async fn exec_insert_body_and_incoming(
        self,
        conn: &dyn ConnectionTrait,
        backend: DbBackend,
        partition_id: i64,
        payload: Vec<u8>,
        payload_type: &str,
    ) -> Result<i64, DbErr> {
        if let Some(cte) = self.insert_body_and_incoming_cte() {
            self.exec_insert_returning_id(
                conn,
                backend,
                cte,
                vec![payload.into(), payload_type.into(), partition_id.into()],
                "incoming",
            )
            .await
        } else {
            // MySQL: two separate round-trips (no CTE INSERT support)
            let body_id = self
                .exec_insert_body(conn, backend, payload, payload_type)
                .await?;
            self.exec_insert_incoming(conn, backend, partition_id, body_id)
                .await
        }
    }

    /// Execute the lease-acquire UPDATE and return `(processed_seq, attempts)` if
    /// the lease was obtained.
    ///
    /// Encapsulates the `RETURNING` vs UPDATE-then-SELECT branching for `MySQL`.
    pub async fn exec_lease_acquire(
        self,
        conn: &dyn ConnectionTrait,
        backend: DbBackend,
        lease_id: &str,
        lease_secs: i64,
        partition_id: i64,
    ) -> Result<Option<(i64, i16)>, DbErr> {
        if self.supports_returning() {
            let row = conn
                .query_one(Statement::from_sql_and_values(
                    backend,
                    self.lease_acquire(),
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
                    backend,
                    self.lease_acquire(),
                    [lease_id.into(), lease_secs.into(), partition_id.into()],
                ))
                .await?;
            if result.rows_affected() == 0 {
                return Ok(None);
            }
            let row = conn
                .query_one(Statement::from_sql_and_values(
                    backend,
                    self.read_processor(),
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
}

// -- Sequencer queries --

impl Dialect {
    pub fn claim_incoming(self, batch_size: u32) -> ClaimSql {
        match self {
            Self::Postgres => ClaimSql {
                select: format!(
                    "SELECT id, body_id \
                     FROM modkit_outbox_incoming \
                     WHERE partition_id = $1 \
                     ORDER BY id \
                     LIMIT {batch_size} \
                     FOR UPDATE SKIP LOCKED"
                ),
            },
            Self::Sqlite => ClaimSql {
                select: format!(
                    "SELECT id, body_id \
                     FROM modkit_outbox_incoming \
                     WHERE partition_id = $1 \
                     ORDER BY id \
                     LIMIT {batch_size}"
                ),
            },
            // SKIP LOCKED prevents InnoDB gap-lock deadlocks when
            // multiple sequencers claim from adjacent partitions.
            Self::MySql => ClaimSql {
                select: format!(
                    "SELECT id, body_id \
                     FROM modkit_outbox_incoming \
                     WHERE partition_id = ? \
                     ORDER BY id \
                     LIMIT {batch_size} \
                     FOR UPDATE SKIP LOCKED"
                ),
            },
        }
    }

    /// Build `DELETE FROM modkit_outbox_incoming WHERE id IN ($1, $2, ...)`.
    pub fn delete_incoming_batch(self, count: usize) -> String {
        let mut sql = String::from("DELETE FROM modkit_outbox_incoming WHERE id IN (");
        for i in 0..count {
            if i > 0 {
                sql.push_str(", ");
            }
            match self {
                Self::Postgres | Self::Sqlite => {
                    // Writing to a String is infallible.
                    #[allow(clippy::let_underscore_must_use)]
                    let _ = write!(sql, "${}", i + 1);
                }
                Self::MySql => {
                    sql.push('?');
                }
            }
        }
        sql.push(')');
        sql
    }

    pub fn allocate_sequences(self) -> AllocSql {
        match self {
            Self::Postgres | Self::Sqlite => AllocSql::UpdateReturning(
                "UPDATE modkit_outbox_partitions \
                 SET sequence = sequence + $2 \
                 WHERE id = $1 \
                 RETURNING sequence - $2 AS start_seq",
            ),
            Self::MySql => AllocSql::UpdateThenSelect {
                update: "UPDATE modkit_outbox_partitions \
                         SET sequence = sequence + ? WHERE id = ?",
                select: "SELECT sequence - ? AS start_seq \
                         FROM modkit_outbox_partitions WHERE id = ?",
            },
        }
    }

    pub fn build_insert_outgoing_batch(self, count: usize) -> String {
        let mut sql =
            String::from("INSERT INTO modkit_outbox_outgoing (partition_id, body_id, seq) VALUES ");
        self.append_value_tuples(&mut sql, count, 3);
        sql
    }

    pub fn lock_partition(self) -> Option<&'static str> {
        match self {
            Self::Postgres => Some(
                "SELECT id FROM modkit_outbox_partitions \
                 WHERE id = $1 FOR UPDATE SKIP LOCKED",
            ),
            Self::MySql => Some(
                "SELECT id FROM modkit_outbox_partitions \
                 WHERE id = ? FOR UPDATE SKIP LOCKED",
            ),
            Self::Sqlite => None,
        }
    }

    /// Cold-path discovery: find all partition IDs with pending incoming rows.
    /// Uses the existing `(partition_id, id)` index for an index-only skip scan.
    /// Same SQL for all backends — `DISTINCT` on the leading index column is portable.
    pub fn discover_dirty_partitions(self) -> &'static str {
        // Same SQL for all backends — DISTINCT on leading index column is portable.
        match self {
            Self::Postgres | Self::Sqlite | Self::MySql => {
                "SELECT DISTINCT partition_id FROM modkit_outbox_incoming"
            }
        }
    }
}

// -- Processor queries --

impl Dialect {
    pub fn insert_processor_row(self) -> &'static str {
        match self {
            Self::Postgres => {
                "INSERT INTO modkit_outbox_processor (partition_id) \
                 VALUES ($1) ON CONFLICT (partition_id) DO NOTHING"
            }
            Self::Sqlite => {
                "INSERT OR IGNORE INTO modkit_outbox_processor (partition_id) \
                 VALUES ($1)"
            }
            Self::MySql => {
                "INSERT IGNORE INTO modkit_outbox_processor (partition_id) \
                 VALUES (?)"
            }
        }
    }

    pub fn lock_processor(self) -> Option<&'static str> {
        match self {
            Self::Postgres => Some(
                "SELECT partition_id, processed_seq, attempts \
                 FROM modkit_outbox_processor \
                 WHERE partition_id = $1 FOR UPDATE SKIP LOCKED",
            ),
            Self::MySql => Some(
                "SELECT partition_id, processed_seq, attempts \
                 FROM modkit_outbox_processor \
                 WHERE partition_id = ? FOR UPDATE SKIP LOCKED",
            ),
            Self::Sqlite => None,
        }
    }

    pub fn read_outgoing_batch(self, batch_size: u32) -> String {
        match self {
            Self::Postgres | Self::Sqlite => format!(
                "SELECT id, body_id, seq \
                 FROM modkit_outbox_outgoing \
                 WHERE partition_id = $1 AND seq > $2 \
                 ORDER BY seq \
                 LIMIT {batch_size}"
            ),
            Self::MySql => format!(
                "SELECT id, body_id, seq \
                 FROM modkit_outbox_outgoing \
                 WHERE partition_id = ? AND seq > ? \
                 ORDER BY seq \
                 LIMIT {batch_size}"
            ),
        }
    }

    pub fn advance_processed_seq(self) -> &'static str {
        match self {
            Self::Postgres | Self::Sqlite => {
                "UPDATE modkit_outbox_processor \
                 SET processed_seq = $1, attempts = 0, last_error = NULL \
                 WHERE partition_id = $2"
            }
            Self::MySql => {
                "UPDATE modkit_outbox_processor \
                 SET processed_seq = ?, attempts = 0, last_error = NULL \
                 WHERE partition_id = ?"
            }
        }
    }

    pub fn record_retry(self) -> &'static str {
        match self {
            Self::Postgres | Self::Sqlite => {
                "UPDATE modkit_outbox_processor \
                 SET attempts = attempts + 1, last_error = $1 \
                 WHERE partition_id = $2"
            }
            Self::MySql => {
                "UPDATE modkit_outbox_processor \
                 SET attempts = attempts + 1, last_error = ? \
                 WHERE partition_id = ?"
            }
        }
    }

    pub fn insert_dead_letter(self) -> &'static str {
        match self {
            Self::Postgres | Self::Sqlite => {
                "INSERT INTO modkit_outbox_dead_letters \
                 (partition_id, seq, payload, payload_type, created_at, last_error, attempts) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7)"
            }
            Self::MySql => {
                "INSERT INTO modkit_outbox_dead_letters \
                 (partition_id, seq, payload, payload_type, created_at, last_error, attempts) \
                 VALUES (?, ?, ?, ?, ?, ?, ?)"
            }
        }
    }

    /// Acquire a lease on the processor row for leased mode.
    ///
    /// Atomically increments `attempts` so that a pod crash leaves a trace —
    /// the next pod will see a non-zero attempt count even though the previous
    /// processing cycle never reached the ack phase.
    ///
    /// Returns `processed_seq` and `attempts` (post-increment).
    /// Callers subtract 1 to recover the pre-increment value for the handler.
    pub fn lease_acquire(self) -> &'static str {
        match self {
            Self::Postgres => {
                "UPDATE modkit_outbox_processor \
                 SET locked_by = $1, locked_until = NOW() + $2 * INTERVAL '1 second', \
                     attempts = attempts + 1 \
                 WHERE partition_id = $3 \
                   AND (locked_by IS NULL OR locked_until < NOW()) \
                 RETURNING processed_seq, attempts"
            }
            Self::Sqlite => {
                "UPDATE modkit_outbox_processor \
                 SET locked_by = $1, locked_until = datetime('now', '+' || $2 || ' seconds'), \
                     attempts = attempts + 1 \
                 WHERE partition_id = $3 \
                   AND (locked_by IS NULL OR locked_until < datetime('now')) \
                 RETURNING processed_seq, attempts"
            }
            Self::MySql => {
                "UPDATE modkit_outbox_processor \
                 SET locked_by = ?, locked_until = DATE_ADD(NOW(6), INTERVAL ? SECOND), \
                     attempts = attempts + 1 \
                 WHERE partition_id = ? \
                   AND (locked_by IS NULL OR locked_until < NOW(6))"
            }
        }
    }

    /// Ack with lease guard: advance `processed_seq` only if we still own the lease.
    pub fn lease_ack_advance(self) -> &'static str {
        match self {
            Self::Postgres | Self::Sqlite => {
                "UPDATE modkit_outbox_processor \
                 SET processed_seq = $1, attempts = 0, last_error = NULL, \
                     locked_by = NULL, locked_until = NULL \
                 WHERE partition_id = $2 AND locked_by = $3"
            }
            Self::MySql => {
                "UPDATE modkit_outbox_processor \
                 SET processed_seq = ?, attempts = 0, last_error = NULL, \
                     locked_by = NULL, locked_until = NULL \
                 WHERE partition_id = ? AND locked_by = ?"
            }
        }
    }

    /// Record retry with lease guard.
    ///
    /// Does NOT increment `attempts` — already incremented during
    /// [`lease_acquire`](Self::lease_acquire). Just records the error
    /// and releases the lease.
    pub fn lease_record_retry(self) -> &'static str {
        match self {
            Self::Postgres | Self::Sqlite => {
                "UPDATE modkit_outbox_processor \
                 SET last_error = $1, \
                     locked_by = NULL, locked_until = NULL \
                 WHERE partition_id = $2 AND locked_by = $3"
            }
            Self::MySql => {
                "UPDATE modkit_outbox_processor \
                 SET last_error = ?, \
                     locked_by = NULL, locked_until = NULL \
                 WHERE partition_id = ? AND locked_by = ?"
            }
        }
    }

    /// Release a lease without changing state (e.g. on empty partition).
    pub fn lease_release(self) -> &'static str {
        match self {
            Self::Postgres | Self::Sqlite => {
                "UPDATE modkit_outbox_processor \
                 SET attempts = 0, locked_by = NULL, locked_until = NULL \
                 WHERE partition_id = $1 AND locked_by = $2"
            }
            Self::MySql => {
                "UPDATE modkit_outbox_processor \
                 SET attempts = 0, locked_by = NULL, locked_until = NULL \
                 WHERE partition_id = ? AND locked_by = ?"
            }
        }
    }

    /// Vacuum: bounded-chunk cleanup.
    ///
    /// Returns SQL to SELECT a bounded chunk of (id, `body_id`) from outgoing.
    /// The caller deletes those rows by ID, then loops while
    /// `deleted == batch_size`.
    pub fn vacuum_cleanup(self) -> VacuumSql {
        match self {
            Self::Postgres | Self::Sqlite => VacuumSql {
                select_outgoing_chunk: "SELECT id, body_id FROM modkit_outbox_outgoing \
                                        WHERE partition_id = $1 AND seq <= $2 \
                                        ORDER BY seq LIMIT $3",
            },
            Self::MySql => VacuumSql {
                select_outgoing_chunk: "SELECT id, body_id FROM modkit_outbox_outgoing \
                                        WHERE partition_id = ? AND seq <= ? \
                                        ORDER BY seq LIMIT ?",
            },
        }
    }

    /// Build `DELETE FROM modkit_outbox_outgoing WHERE id IN ($1, $2, ...)`.
    pub fn build_delete_outgoing_batch(self, count: usize) -> String {
        let mut sql = String::from("DELETE FROM modkit_outbox_outgoing WHERE id IN (");
        self.append_in_placeholders(&mut sql, count);
        sql.push(')');
        sql
    }

    /// Build `DELETE FROM modkit_outbox_body WHERE id IN (...)`.
    pub fn build_delete_body_batch(self, count: usize) -> String {
        let mut sql = String::from("DELETE FROM modkit_outbox_body WHERE id IN (");
        self.append_in_placeholders(&mut sql, count);
        sql.push(')');
        sql
    }

    pub fn read_processor(self) -> &'static str {
        match self {
            Self::Postgres | Self::Sqlite => {
                "SELECT processed_seq, attempts \
                 FROM modkit_outbox_processor WHERE partition_id = $1"
            }
            Self::MySql => {
                "SELECT processed_seq, attempts \
                 FROM modkit_outbox_processor WHERE partition_id = ?"
            }
        }
    }
}

// -- Vacuum counter queries --

impl Dialect {
    /// Bump the vacuum counter for a partition (called by processor on ack).
    pub fn bump_vacuum_counter(self) -> &'static str {
        match self {
            Self::Postgres | Self::Sqlite => {
                "UPDATE modkit_outbox_vacuum_counter \
                 SET counter = counter + 1 WHERE partition_id = $1"
            }
            Self::MySql => {
                "UPDATE modkit_outbox_vacuum_counter \
                 SET counter = counter + 1 WHERE partition_id = ?"
            }
        }
    }

    /// Fetch dirty partitions paginated by `partition_id` cursor.
    /// Returns `(partition_id, counter)` for partitions with `counter > 0`.
    pub fn fetch_dirty_partitions(self) -> &'static str {
        match self {
            Self::Postgres | Self::Sqlite => {
                "SELECT partition_id, counter \
                 FROM modkit_outbox_vacuum_counter \
                 WHERE counter > 0 AND partition_id > $1 \
                 ORDER BY partition_id LIMIT $2"
            }
            Self::MySql => {
                "SELECT partition_id, counter \
                 FROM modkit_outbox_vacuum_counter \
                 WHERE counter > 0 AND partition_id > ? \
                 ORDER BY partition_id LIMIT ?"
            }
        }
    }

    /// Decrement vacuum counter by snapshot value, floored at 0.
    pub fn decrement_vacuum_counter(self) -> &'static str {
        match self {
            Self::Postgres => {
                "UPDATE modkit_outbox_vacuum_counter \
                 SET counter = GREATEST(counter - $1, 0) \
                 WHERE partition_id = $2"
            }
            Self::Sqlite => {
                "UPDATE modkit_outbox_vacuum_counter \
                 SET counter = MAX(counter - $1, 0) \
                 WHERE partition_id = $2"
            }
            Self::MySql => {
                "UPDATE modkit_outbox_vacuum_counter \
                 SET counter = GREATEST(counter - ?, 0) \
                 WHERE partition_id = ?"
            }
        }
    }

    /// Insert a vacuum counter row (idempotent, for `register_queue`).
    pub fn insert_vacuum_counter_row(self) -> &'static str {
        match self {
            Self::Postgres => {
                "INSERT INTO modkit_outbox_vacuum_counter (partition_id) \
                 VALUES ($1) ON CONFLICT (partition_id) DO NOTHING"
            }
            Self::Sqlite => {
                "INSERT OR IGNORE INTO modkit_outbox_vacuum_counter (partition_id) \
                 VALUES ($1)"
            }
            Self::MySql => {
                "INSERT IGNORE INTO modkit_outbox_vacuum_counter (partition_id) \
                 VALUES (?)"
            }
        }
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "dialect_tests.rs"]
mod tests;
