#![allow(dead_code)]

use sea_orm::DbBackend;

use super::dialect::{AllocSql, Dialect, VacuumSql};
use super::tables::OutboxTables;

pub(super) struct OutboxStatements {
    backend: DbBackend,
    dialect: Dialect,
    tables: OutboxTables,
    registration: RegistrationStatements,
    enqueue: EnqueueStatements,
    sequencer: SequencerStatements,
    processor: ProcessorStatements,
    vacuum: VacuumStatements,
    dead_letters: DeadLetterStatements,
}

pub(super) struct RegistrationStatements {
    register_queue_select: String,
    register_queue_insert: String,
    insert_vacuum_counter_row: String,
}

pub(super) struct EnqueueStatements {
    insert_body_and_incoming_cte: Option<String>,
    insert_body: String,
    insert_incoming: String,
    body_id_reservation: Option<MySqlIdReservationStatements>,
    incoming_id_reservation: Option<MySqlIdReservationStatements>,
}

pub(super) struct MySqlIdReservationStatements {
    select_next_id_for_update: String,
    advance_next_id: String,
}

pub(super) struct SequencerStatements {
    allocate_sequences: AllocSql,
    lock_partition: Option<String>,
    discover_dirty_partitions: String,
}

pub(super) struct ProcessorStatements {
    insert_processor_row: String,
    lock_processor: Option<String>,
    advance_processed_seq: String,
    record_retry: String,
    insert_dead_letter: String,
    lease_acquire: String,
    lease_ack_advance: String,
    lease_record_retry: String,
    lease_release: String,
    read_processor: String,
}

pub(super) struct VacuumStatements {
    cleanup: VacuumSql,
    bump_counter: String,
    fetch_dirty_partitions: String,
    decrement_counter: String,
    #[cfg(test)]
    reset_counter: String,
}

pub(super) struct DeadLetterStatements {
    select_columns: String,
    count_base: String,
    id_select_base: String,
}

impl OutboxStatements {
    pub(super) fn new(backend: DbBackend, tables: &OutboxTables) -> Self {
        let dialect = Dialect::from(backend);
        Self {
            backend,
            dialect,
            tables: tables.clone(),
            registration: RegistrationStatements::new(dialect, tables),
            enqueue: EnqueueStatements::new(dialect, tables),
            sequencer: SequencerStatements::new(dialect, tables),
            processor: ProcessorStatements::new(dialect, tables),
            vacuum: VacuumStatements::new(dialect, tables),
            dead_letters: DeadLetterStatements::new(tables),
        }
    }

    pub(super) fn backend(&self) -> DbBackend {
        self.backend
    }

    pub(super) fn dialect(&self) -> Dialect {
        self.dialect
    }

    pub(super) fn tables(&self) -> &OutboxTables {
        &self.tables
    }

    pub(super) fn registration(&self) -> &RegistrationStatements {
        &self.registration
    }

    pub(super) fn enqueue(&self) -> &EnqueueStatements {
        &self.enqueue
    }

    pub(super) fn sequencer(&self) -> &SequencerStatements {
        &self.sequencer
    }

    pub(super) fn processor(&self) -> &ProcessorStatements {
        &self.processor
    }

    pub(super) fn vacuum(&self) -> &VacuumStatements {
        &self.vacuum
    }

    pub(super) fn dead_letters(&self) -> &DeadLetterStatements {
        &self.dead_letters
    }
}

impl RegistrationStatements {
    fn new(dialect: Dialect, tables: &OutboxTables) -> Self {
        Self {
            register_queue_select: register_queue_select(dialect, tables),
            register_queue_insert: register_queue_insert(dialect, tables),
            insert_vacuum_counter_row: insert_vacuum_counter_row(dialect, tables),
        }
    }

    pub(super) fn register_queue_select(&self) -> &str {
        &self.register_queue_select
    }

    pub(super) fn register_queue_insert(&self) -> &str {
        &self.register_queue_insert
    }

    pub(super) fn insert_vacuum_counter_row(&self) -> &str {
        &self.insert_vacuum_counter_row
    }
}

impl EnqueueStatements {
    fn new(dialect: Dialect, tables: &OutboxTables) -> Self {
        Self {
            insert_body_and_incoming_cte: insert_body_and_incoming_cte(dialect, tables),
            insert_body: insert_body(dialect, tables),
            insert_incoming: insert_incoming(dialect, tables),
            body_id_reservation: mysql_id_reservation(dialect, tables.body_id_sequence()),
            incoming_id_reservation: mysql_id_reservation(dialect, tables.incoming_id_sequence()),
        }
    }

    pub(super) fn insert_body_and_incoming_cte(&self) -> Option<&str> {
        self.insert_body_and_incoming_cte.as_deref()
    }

    pub(super) fn insert_body(&self) -> &str {
        &self.insert_body
    }

    pub(super) fn insert_incoming(&self) -> &str {
        &self.insert_incoming
    }

    pub(super) fn body_id_reservation(&self) -> Option<&MySqlIdReservationStatements> {
        self.body_id_reservation.as_ref()
    }

    pub(super) fn incoming_id_reservation(&self) -> Option<&MySqlIdReservationStatements> {
        self.incoming_id_reservation.as_ref()
    }
}

impl MySqlIdReservationStatements {
    fn new(table: &str) -> Self {
        Self {
            select_next_id_for_update: format!(
                "SELECT next_id FROM {table} WHERE slot = 1 FOR UPDATE"
            ),
            advance_next_id: format!("UPDATE {table} SET next_id = next_id + ? WHERE slot = 1"),
        }
    }

    pub(super) fn select_next_id_for_update(&self) -> &str {
        &self.select_next_id_for_update
    }

    pub(super) fn advance_next_id(&self) -> &str {
        &self.advance_next_id
    }
}

impl SequencerStatements {
    fn new(dialect: Dialect, tables: &OutboxTables) -> Self {
        Self {
            allocate_sequences: allocate_sequences(dialect, tables),
            lock_partition: lock_partition(dialect, tables),
            discover_dirty_partitions: discover_dirty_partitions(tables),
        }
    }

    pub(super) fn allocate_sequences(&self) -> &AllocSql {
        &self.allocate_sequences
    }

    pub(super) fn lock_partition(&self) -> Option<&str> {
        self.lock_partition.as_deref()
    }

    pub(super) fn discover_dirty_partitions(&self) -> &str {
        &self.discover_dirty_partitions
    }
}

impl ProcessorStatements {
    fn new(dialect: Dialect, tables: &OutboxTables) -> Self {
        Self {
            insert_processor_row: insert_processor_row(dialect, tables),
            lock_processor: lock_processor(dialect, tables),
            advance_processed_seq: advance_processed_seq(dialect, tables),
            record_retry: record_retry(dialect, tables),
            insert_dead_letter: insert_dead_letter(dialect, tables),
            lease_acquire: lease_acquire(dialect, tables),
            lease_ack_advance: lease_ack_advance(dialect, tables),
            lease_record_retry: lease_record_retry(dialect, tables),
            lease_release: lease_release(dialect, tables),
            read_processor: read_processor(dialect, tables),
        }
    }

    pub(super) fn insert_processor_row(&self) -> &str {
        &self.insert_processor_row
    }

    pub(super) fn lock_processor(&self) -> Option<&str> {
        self.lock_processor.as_deref()
    }

    pub(super) fn advance_processed_seq(&self) -> &str {
        &self.advance_processed_seq
    }

    pub(super) fn record_retry(&self) -> &str {
        &self.record_retry
    }

    pub(super) fn insert_dead_letter(&self) -> &str {
        &self.insert_dead_letter
    }

    pub(super) fn lease_acquire(&self) -> &str {
        &self.lease_acquire
    }

    pub(super) fn lease_ack_advance(&self) -> &str {
        &self.lease_ack_advance
    }

    pub(super) fn lease_record_retry(&self) -> &str {
        &self.lease_record_retry
    }

    pub(super) fn lease_release(&self) -> &str {
        &self.lease_release
    }

    pub(super) fn read_processor(&self) -> &str {
        &self.read_processor
    }
}

impl VacuumStatements {
    fn new(dialect: Dialect, tables: &OutboxTables) -> Self {
        Self {
            cleanup: vacuum_cleanup(dialect, tables),
            bump_counter: bump_vacuum_counter(dialect, tables),
            fetch_dirty_partitions: fetch_vacuum_dirty_partitions(dialect, tables),
            decrement_counter: decrement_vacuum_counter(dialect, tables),
            #[cfg(test)]
            reset_counter: reset_vacuum_counter(dialect, tables),
        }
    }

    pub(super) fn cleanup(&self) -> &VacuumSql {
        &self.cleanup
    }

    pub(super) fn bump_counter(&self) -> &str {
        &self.bump_counter
    }

    pub(super) fn fetch_dirty_partitions(&self) -> &str {
        &self.fetch_dirty_partitions
    }

    pub(super) fn decrement_counter(&self) -> &str {
        &self.decrement_counter
    }

    #[cfg(test)]
    pub(super) fn reset_counter(&self) -> &str {
        &self.reset_counter
    }
}

impl DeadLetterStatements {
    fn new(tables: &OutboxTables) -> Self {
        Self {
            select_columns: format!(
                "SELECT d.id, d.partition_id, d.seq, d.payload, d.payload_type, \
                 d.created_at, d.failed_at, d.last_error, d.attempts, d.status, \
                 d.completed_at, d.deadline FROM {} d",
                tables.dead_letters()
            ),
            count_base: format!("SELECT COUNT(*) AS cnt FROM {} d", tables.dead_letters()),
            id_select_base: format!("SELECT d.id FROM {} d", tables.dead_letters()),
        }
    }

    pub(super) fn select_columns(&self) -> &str {
        &self.select_columns
    }

    pub(super) fn count_base(&self) -> &str {
        &self.count_base
    }

    pub(super) fn id_select_base(&self) -> &str {
        &self.id_select_base
    }
}

fn register_queue_select(dialect: Dialect, tables: &OutboxTables) -> String {
    match dialect {
        Dialect::Postgres | Dialect::Sqlite => format!(
            "SELECT id FROM {} WHERE queue = $1 ORDER BY partition ASC",
            tables.partitions()
        ),
        Dialect::MySql => format!(
            "SELECT id FROM {} WHERE queue = ? ORDER BY `partition` ASC",
            tables.partitions()
        ),
    }
}

fn register_queue_insert(dialect: Dialect, tables: &OutboxTables) -> String {
    match dialect {
        Dialect::Postgres => format!(
            "INSERT INTO {} (queue, partition) \
             VALUES ($1, $2) ON CONFLICT (queue, partition) DO NOTHING",
            tables.partitions()
        ),
        Dialect::Sqlite => format!(
            "INSERT OR IGNORE INTO {} (queue, partition) VALUES ($1, $2)",
            tables.partitions()
        ),
        Dialect::MySql => format!(
            "INSERT IGNORE INTO {} (queue, `partition`) VALUES (?, ?)",
            tables.partitions()
        ),
    }
}

fn insert_body_and_incoming_cte(dialect: Dialect, tables: &OutboxTables) -> Option<String> {
    match dialect {
        Dialect::Postgres => Some(format!(
            "WITH b AS (\
               INSERT INTO {} (payload, payload_type) \
               VALUES ($1, $2) RETURNING id\
             ) \
             INSERT INTO {} (partition_id, body_id) \
             SELECT $3, id FROM b RETURNING id",
            tables.body(),
            tables.incoming()
        )),
        Dialect::Sqlite | Dialect::MySql => None,
    }
}

fn insert_body(dialect: Dialect, tables: &OutboxTables) -> String {
    match dialect {
        Dialect::Postgres | Dialect::Sqlite => format!(
            "INSERT INTO {} (payload, payload_type) VALUES ($1, $2) RETURNING id",
            tables.body()
        ),
        Dialect::MySql => format!(
            "INSERT INTO {} (payload, payload_type) VALUES (?, ?)",
            tables.body()
        ),
    }
}

fn insert_incoming(dialect: Dialect, tables: &OutboxTables) -> String {
    match dialect {
        Dialect::Postgres | Dialect::Sqlite => format!(
            "INSERT INTO {} (partition_id, body_id) VALUES ($1, $2) RETURNING id",
            tables.incoming()
        ),
        Dialect::MySql => format!(
            "INSERT INTO {} (partition_id, body_id) VALUES (?, ?)",
            tables.incoming()
        ),
    }
}

fn mysql_id_reservation(
    dialect: Dialect,
    sequence_table: &str,
) -> Option<MySqlIdReservationStatements> {
    match dialect {
        Dialect::MySql => Some(MySqlIdReservationStatements::new(sequence_table)),
        Dialect::Postgres | Dialect::Sqlite => None,
    }
}

fn allocate_sequences(dialect: Dialect, tables: &OutboxTables) -> AllocSql {
    match dialect {
        Dialect::Postgres | Dialect::Sqlite => AllocSql::UpdateReturning(format!(
            "UPDATE {} \
             SET sequence = sequence + $2 \
             WHERE id = $1 \
             RETURNING sequence - $2 AS start_seq",
            tables.partitions()
        )),
        Dialect::MySql => AllocSql::UpdateThenSelect {
            update: format!(
                "UPDATE {} SET sequence = sequence + ? WHERE id = ?",
                tables.partitions()
            ),
            select: format!(
                "SELECT sequence - ? AS start_seq FROM {} WHERE id = ?",
                tables.partitions()
            ),
        },
    }
}

fn lock_partition(dialect: Dialect, tables: &OutboxTables) -> Option<String> {
    match dialect {
        Dialect::Postgres => Some(format!(
            "SELECT id FROM {} WHERE id = $1 FOR UPDATE SKIP LOCKED",
            tables.partitions()
        )),
        Dialect::MySql => Some(format!(
            "SELECT id FROM {} WHERE id = ? FOR UPDATE SKIP LOCKED",
            tables.partitions()
        )),
        Dialect::Sqlite => None,
    }
}

fn discover_dirty_partitions(tables: &OutboxTables) -> String {
    format!("SELECT DISTINCT partition_id FROM {}", tables.incoming())
}

fn insert_processor_row(dialect: Dialect, tables: &OutboxTables) -> String {
    match dialect {
        Dialect::Postgres => format!(
            "INSERT INTO {} (partition_id) \
             VALUES ($1) ON CONFLICT (partition_id) DO NOTHING",
            tables.processor()
        ),
        Dialect::Sqlite => format!(
            "INSERT OR IGNORE INTO {} (partition_id) VALUES ($1)",
            tables.processor()
        ),
        Dialect::MySql => format!(
            "INSERT IGNORE INTO {} (partition_id) VALUES (?)",
            tables.processor()
        ),
    }
}

fn lock_processor(dialect: Dialect, tables: &OutboxTables) -> Option<String> {
    match dialect {
        Dialect::Postgres => Some(format!(
            "SELECT partition_id, processed_seq, attempts \
             FROM {} WHERE partition_id = $1 FOR UPDATE SKIP LOCKED",
            tables.processor()
        )),
        Dialect::MySql => Some(format!(
            "SELECT partition_id, processed_seq, attempts \
             FROM {} WHERE partition_id = ? FOR UPDATE SKIP LOCKED",
            tables.processor()
        )),
        Dialect::Sqlite => None,
    }
}

fn advance_processed_seq(dialect: Dialect, tables: &OutboxTables) -> String {
    match dialect {
        Dialect::Postgres | Dialect::Sqlite => format!(
            "UPDATE {} \
             SET processed_seq = $1, attempts = 0, last_error = NULL \
             WHERE partition_id = $2",
            tables.processor()
        ),
        Dialect::MySql => format!(
            "UPDATE {} \
             SET processed_seq = ?, attempts = 0, last_error = NULL \
             WHERE partition_id = ?",
            tables.processor()
        ),
    }
}

fn record_retry(dialect: Dialect, tables: &OutboxTables) -> String {
    match dialect {
        Dialect::Postgres | Dialect::Sqlite => format!(
            "UPDATE {} \
             SET attempts = attempts + 1, last_error = $1 \
             WHERE partition_id = $2",
            tables.processor()
        ),
        Dialect::MySql => format!(
            "UPDATE {} \
             SET attempts = attempts + 1, last_error = ? \
             WHERE partition_id = ?",
            tables.processor()
        ),
    }
}

fn insert_dead_letter(dialect: Dialect, tables: &OutboxTables) -> String {
    match dialect {
        Dialect::Postgres | Dialect::Sqlite => format!(
            "INSERT INTO {} \
             (partition_id, seq, payload, payload_type, created_at, last_error, attempts) \
             VALUES ($1, $2, $3, $4, $5, $6, $7)",
            tables.dead_letters()
        ),
        Dialect::MySql => format!(
            "INSERT INTO {} \
             (partition_id, seq, payload, payload_type, created_at, last_error, attempts) \
             VALUES (?, ?, ?, ?, ?, ?, ?)",
            tables.dead_letters()
        ),
    }
}

fn lease_acquire(dialect: Dialect, tables: &OutboxTables) -> String {
    match dialect {
        Dialect::Postgres => format!(
            "UPDATE {} \
             SET locked_by = $1, locked_until = NOW() + $2 * INTERVAL '1 second', \
                 attempts = attempts + 1 \
             WHERE partition_id = $3 \
               AND (locked_by IS NULL OR locked_until < NOW()) \
             RETURNING processed_seq, attempts",
            tables.processor()
        ),
        Dialect::Sqlite => format!(
            "UPDATE {} \
             SET locked_by = $1, locked_until = datetime('now', '+' || $2 || ' seconds'), \
                 attempts = attempts + 1 \
             WHERE partition_id = $3 \
               AND (locked_by IS NULL OR locked_until < datetime('now')) \
             RETURNING processed_seq, attempts",
            tables.processor()
        ),
        Dialect::MySql => format!(
            "UPDATE {} \
             SET locked_by = ?, locked_until = DATE_ADD(NOW(6), INTERVAL ? SECOND), \
                 attempts = attempts + 1 \
             WHERE partition_id = ? \
               AND (locked_by IS NULL OR locked_until < NOW(6))",
            tables.processor()
        ),
    }
}

fn lease_ack_advance(dialect: Dialect, tables: &OutboxTables) -> String {
    match dialect {
        Dialect::Postgres | Dialect::Sqlite => format!(
            "UPDATE {} \
             SET processed_seq = $1, attempts = 0, last_error = NULL, \
                 locked_by = NULL, locked_until = NULL \
             WHERE partition_id = $2 AND locked_by = $3",
            tables.processor()
        ),
        Dialect::MySql => format!(
            "UPDATE {} \
             SET processed_seq = ?, attempts = 0, last_error = NULL, \
                 locked_by = NULL, locked_until = NULL \
             WHERE partition_id = ? AND locked_by = ?",
            tables.processor()
        ),
    }
}

fn lease_record_retry(dialect: Dialect, tables: &OutboxTables) -> String {
    match dialect {
        Dialect::Postgres | Dialect::Sqlite => format!(
            "UPDATE {} \
             SET last_error = $1, locked_by = NULL, locked_until = NULL \
             WHERE partition_id = $2 AND locked_by = $3",
            tables.processor()
        ),
        Dialect::MySql => format!(
            "UPDATE {} \
             SET last_error = ?, locked_by = NULL, locked_until = NULL \
             WHERE partition_id = ? AND locked_by = ?",
            tables.processor()
        ),
    }
}

fn lease_release(dialect: Dialect, tables: &OutboxTables) -> String {
    match dialect {
        Dialect::Postgres | Dialect::Sqlite => format!(
            "UPDATE {} \
             SET attempts = 0, locked_by = NULL, locked_until = NULL \
             WHERE partition_id = $1 AND locked_by = $2",
            tables.processor()
        ),
        Dialect::MySql => format!(
            "UPDATE {} \
             SET attempts = 0, locked_by = NULL, locked_until = NULL \
             WHERE partition_id = ? AND locked_by = ?",
            tables.processor()
        ),
    }
}

fn read_processor(dialect: Dialect, tables: &OutboxTables) -> String {
    match dialect {
        Dialect::Postgres | Dialect::Sqlite => format!(
            "SELECT processed_seq, attempts FROM {} WHERE partition_id = $1",
            tables.processor()
        ),
        Dialect::MySql => format!(
            "SELECT processed_seq, attempts FROM {} WHERE partition_id = ?",
            tables.processor()
        ),
    }
}

fn vacuum_cleanup(dialect: Dialect, tables: &OutboxTables) -> VacuumSql {
    match dialect {
        Dialect::Postgres | Dialect::Sqlite => VacuumSql {
            select_outgoing_chunk: format!(
                "SELECT id, body_id FROM {} \
                 WHERE partition_id = $1 AND seq <= $2 \
                 ORDER BY seq LIMIT $3",
                tables.outgoing()
            ),
        },
        Dialect::MySql => VacuumSql {
            select_outgoing_chunk: format!(
                "SELECT id, body_id FROM {} \
                 WHERE partition_id = ? AND seq <= ? \
                 ORDER BY seq LIMIT ?",
                tables.outgoing()
            ),
        },
    }
}

fn bump_vacuum_counter(dialect: Dialect, tables: &OutboxTables) -> String {
    match dialect {
        Dialect::Postgres | Dialect::Sqlite => format!(
            "UPDATE {} SET counter = counter + 1 WHERE partition_id = $1",
            tables.vacuum_counter()
        ),
        Dialect::MySql => format!(
            "UPDATE {} SET counter = counter + 1 WHERE partition_id = ?",
            tables.vacuum_counter()
        ),
    }
}

fn fetch_vacuum_dirty_partitions(dialect: Dialect, tables: &OutboxTables) -> String {
    match dialect {
        Dialect::Postgres | Dialect::Sqlite => format!(
            "SELECT partition_id, counter \
             FROM {} \
             WHERE counter > 0 AND partition_id > $1 \
             ORDER BY partition_id LIMIT $2",
            tables.vacuum_counter()
        ),
        Dialect::MySql => format!(
            "SELECT partition_id, counter \
             FROM {} \
             WHERE counter > 0 AND partition_id > ? \
             ORDER BY partition_id LIMIT ?",
            tables.vacuum_counter()
        ),
    }
}

fn decrement_vacuum_counter(dialect: Dialect, tables: &OutboxTables) -> String {
    match dialect {
        Dialect::Postgres => format!(
            "UPDATE {} \
             SET counter = GREATEST(counter - $1, 0) \
             WHERE partition_id = $2",
            tables.vacuum_counter()
        ),
        Dialect::Sqlite => format!(
            "UPDATE {} \
             SET counter = MAX(counter - $1, 0) \
             WHERE partition_id = $2",
            tables.vacuum_counter()
        ),
        Dialect::MySql => format!(
            "UPDATE {} \
             SET counter = GREATEST(counter - ?, 0) \
             WHERE partition_id = ?",
            tables.vacuum_counter()
        ),
    }
}

#[cfg(test)]
fn reset_vacuum_counter(dialect: Dialect, tables: &OutboxTables) -> String {
    match dialect {
        Dialect::Postgres | Dialect::Sqlite => format!(
            "UPDATE {} SET counter = 0 WHERE partition_id = $1",
            tables.vacuum_counter()
        ),
        Dialect::MySql => format!(
            "UPDATE {} SET counter = 0 WHERE partition_id = ?",
            tables.vacuum_counter()
        ),
    }
}

fn insert_vacuum_counter_row(dialect: Dialect, tables: &OutboxTables) -> String {
    match dialect {
        Dialect::Postgres => format!(
            "INSERT INTO {} (partition_id) \
             VALUES ($1) ON CONFLICT (partition_id) DO NOTHING",
            tables.vacuum_counter()
        ),
        Dialect::Sqlite => format!(
            "INSERT OR IGNORE INTO {} (partition_id) VALUES ($1)",
            tables.vacuum_counter()
        ),
        Dialect::MySql => format!(
            "INSERT IGNORE INTO {} (partition_id) VALUES (?)",
            tables.vacuum_counter()
        ),
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    fn statements(backend: DbBackend) -> OutboxStatements {
        OutboxStatements::new(backend, &OutboxTables::default())
    }

    #[test]
    fn postgres_uses_dollar_placeholders() {
        let statements = statements(DbBackend::Postgres);

        assert!(statements.enqueue().insert_body().contains("$1"));
        assert!(statements.enqueue().insert_body().contains("$2"));
        assert!(statements.enqueue().insert_body().contains("RETURNING id"));
        assert!(
            statements
                .registration()
                .register_queue_insert()
                .contains("ON CONFLICT")
        );
        assert!(statements.processor().lease_acquire().contains("$3"));
    }

    #[test]
    fn sqlite_omits_lock_statements() {
        let statements = statements(DbBackend::Sqlite);

        assert!(statements.sequencer().lock_partition().is_none());
        assert!(statements.processor().lock_processor().is_none());
        assert!(statements.enqueue().insert_body().contains("RETURNING id"));
    }

    #[test]
    fn mysql_uses_question_placeholders_and_lock_statements() {
        let statements = statements(DbBackend::MySql);

        assert!(statements.enqueue().insert_body().contains('?'));
        assert!(!statements.enqueue().insert_body().contains('$'));
        assert!(
            statements
                .enqueue()
                .insert_body_and_incoming_cte()
                .is_none()
        );
        assert!(
            statements
                .registration()
                .register_queue_select()
                .contains("`partition`")
        );
        assert!(
            statements
                .sequencer()
                .lock_partition()
                .expect("mysql lock partition")
                .contains("FOR UPDATE SKIP LOCKED")
        );
        assert!(
            statements
                .processor()
                .lock_processor()
                .expect("mysql lock processor")
                .contains("FOR UPDATE SKIP LOCKED")
        );
        assert!(
            statements
                .enqueue()
                .body_id_reservation()
                .expect("body id reservation")
                .select_next_id_for_update()
                .contains("toolkit_outbox_body_id_sequence")
        );
        assert!(
            statements
                .enqueue()
                .incoming_id_reservation()
                .expect("incoming id reservation")
                .advance_next_id()
                .contains("toolkit_outbox_incoming_id_sequence")
        );
    }

    #[test]
    fn statements_use_custom_prefix_without_replacing_inside_names() {
        let tables = OutboxTables::new("toolkit_outbox_body").expect("valid prefix");
        let statements = OutboxStatements::new(DbBackend::Postgres, &tables);

        assert!(
            statements
                .enqueue()
                .insert_body()
                .contains("toolkit_outbox_body_body")
        );
        assert!(
            statements
                .enqueue()
                .insert_incoming()
                .contains("toolkit_outbox_body_incoming")
        );
        assert!(
            statements
                .processor()
                .insert_dead_letter()
                .contains("toolkit_outbox_body_dead_letters")
        );
        assert!(
            !statements
                .processor()
                .insert_dead_letter()
                .contains("toolkit_outbox_body_body_dead_letters")
        );
        assert!(
            statements
                .dead_letters()
                .count_base()
                .contains("toolkit_outbox_body_dead_letters")
        );
    }

    #[test]
    fn allocation_and_vacuum_groups_keep_backend_syntax() {
        let pg = statements(DbBackend::Postgres);
        match pg.sequencer().allocate_sequences() {
            AllocSql::UpdateReturning(sql) => {
                assert!(sql.contains("$1"));
                assert!(sql.contains("$2"));
                assert!(sql.contains("RETURNING sequence - $2"));
            }
            AllocSql::UpdateThenSelect { .. } => panic!("postgres should use returning"),
        }
        assert!(
            pg.vacuum()
                .cleanup()
                .select_outgoing_chunk
                .contains("LIMIT $3")
        );

        let mysql = statements(DbBackend::MySql);
        match mysql.sequencer().allocate_sequences() {
            AllocSql::UpdateReturning(_) => panic!("mysql should use update then select"),
            AllocSql::UpdateThenSelect { update, select } => {
                assert!(update.contains('?'));
                assert!(select.contains('?'));
                assert!(!update.contains('$'));
                assert!(!select.contains('$'));
            }
        }
        assert!(
            mysql
                .vacuum()
                .cleanup()
                .select_outgoing_chunk
                .contains("LIMIT ?")
        );
    }

    #[test]
    fn registration_processor_vacuum_and_dead_letter_groups_are_present() {
        let statements = statements(DbBackend::Postgres);

        assert!(
            statements
                .registration()
                .register_queue_select()
                .contains("toolkit_outbox_partitions")
        );
        assert!(
            statements
                .registration()
                .insert_vacuum_counter_row()
                .contains("toolkit_outbox_vacuum_counter")
        );
        assert!(
            statements
                .processor()
                .read_processor()
                .contains("toolkit_outbox_processor")
        );
        assert!(
            statements
                .vacuum()
                .fetch_dirty_partitions()
                .contains("toolkit_outbox_vacuum_counter")
        );
        assert!(
            statements
                .dead_letters()
                .select_columns()
                .contains("toolkit_outbox_dead_letters")
        );
    }
}
