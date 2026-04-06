use super::*;

impl Dialect {
    pub fn reset_vacuum_counter(self) -> &'static str {
        match self {
            Self::Postgres | Self::Sqlite => {
                "UPDATE modkit_outbox_vacuum_counter \
                 SET counter = 0 WHERE partition_id = $1"
            }
            Self::MySql => {
                "UPDATE modkit_outbox_vacuum_counter \
                 SET counter = 0 WHERE partition_id = ?"
            }
        }
    }
}

#[test]
fn dialect_from_dbbackend() {
    assert_eq!(Dialect::from(DbBackend::Postgres), Dialect::Postgres);
    assert_eq!(Dialect::from(DbBackend::Sqlite), Dialect::Sqlite);
    assert_eq!(Dialect::from(DbBackend::MySql), Dialect::MySql);
}

#[test]
fn postgres_uses_dollar_placeholders() {
    let d = Dialect::Postgres;
    assert!(d.insert_body().contains("$1"));
    assert!(d.insert_body().contains("$2"));
    assert!(d.insert_body().contains("RETURNING"));
}

#[test]
fn mysql_uses_question_placeholders() {
    let d = Dialect::MySql;
    assert!(d.insert_body().contains('?'));
    assert!(!d.insert_body().contains('$'));
    assert!(!d.insert_body().contains("RETURNING"));
}

#[test]
fn supports_returning_correct() {
    assert!(Dialect::Postgres.supports_returning());
    assert!(Dialect::Sqlite.supports_returning());
    assert!(!Dialect::MySql.supports_returning());
}

#[test]
fn lock_partition_correct() {
    assert!(Dialect::Postgres.lock_partition().is_some());
    assert!(Dialect::MySql.lock_partition().is_some());
    assert!(Dialect::Sqlite.lock_partition().is_none());
}

#[test]
fn batch_body_pg_placeholder_format() {
    let sql = Dialect::Postgres.build_insert_body_batch(3);
    assert!(sql.contains("($1, $2), ($3, $4), ($5, $6)"));
    assert!(sql.ends_with("RETURNING id"));
}

#[test]
fn batch_body_mysql_placeholder_format() {
    let sql = Dialect::MySql.build_insert_body_batch(3);
    assert!(sql.contains("(?, ?), (?, ?), (?, ?)"));
    assert!(!sql.contains("RETURNING"));
}

#[test]
fn claim_pg_select_ordered_with_for_update() {
    let claim = Dialect::Postgres.claim_incoming(100);
    assert!(claim.select.contains("ORDER BY id"));
    assert!(claim.select.contains("FOR UPDATE SKIP LOCKED"));
    assert!(claim.select.contains("$1"));
}

#[test]
fn claim_sqlite_select_ordered_no_lock() {
    let claim = Dialect::Sqlite.claim_incoming(100);
    assert!(claim.select.contains("ORDER BY id"));
    assert!(!claim.select.contains("FOR UPDATE"));
}

#[test]
fn claim_mysql_select_ordered_with_for_update() {
    let claim = Dialect::MySql.claim_incoming(100);
    assert!(claim.select.contains("ORDER BY id"));
    assert!(claim.select.contains("FOR UPDATE SKIP LOCKED"));
    assert!(claim.select.contains('?'));
}

#[test]
fn delete_incoming_batch_placeholders() {
    let pg = Dialect::Postgres.delete_incoming_batch(3);
    assert!(pg.contains("$1, $2, $3"));
    assert!(pg.contains("DELETE FROM modkit_outbox_incoming"));

    let mysql = Dialect::MySql.delete_incoming_batch(3);
    assert!(mysql.contains("?, ?, ?"));
}

#[test]
fn alloc_pg_is_update_returning() {
    let alloc = Dialect::Postgres.allocate_sequences();
    assert!(matches!(alloc, AllocSql::UpdateReturning(_)));
}

#[test]
fn alloc_mysql_is_update_then_select() {
    let alloc = Dialect::MySql.allocate_sequences();
    assert!(matches!(alloc, AllocSql::UpdateThenSelect { .. }));
}

#[test]
fn mysql_register_queue_backtick_partition() {
    let d = Dialect::MySql;
    assert!(d.register_queue_select().contains("`partition`"));
    assert!(d.register_queue_insert().contains("`partition`"));
}

#[test]
fn insert_processor_row_pg_uses_on_conflict() {
    let sql = Dialect::Postgres.insert_processor_row();
    assert!(sql.contains("$1"));
    assert!(sql.contains("ON CONFLICT"));
}

#[test]
fn insert_processor_row_sqlite_uses_or_ignore() {
    let sql = Dialect::Sqlite.insert_processor_row();
    assert!(sql.contains("INSERT OR IGNORE"));
    assert!(sql.contains("$1"));
}

#[test]
fn insert_processor_row_mysql_uses_insert_ignore() {
    let sql = Dialect::MySql.insert_processor_row();
    assert!(sql.contains("INSERT IGNORE"));
    assert!(sql.contains('?'));
    assert!(!sql.contains('$'));
}

#[test]
fn lock_processor_correct() {
    assert!(Dialect::Postgres.lock_processor().is_some());
    assert!(Dialect::MySql.lock_processor().is_some());
    assert!(Dialect::Sqlite.lock_processor().is_none());

    let pg = Dialect::Postgres.lock_processor().unwrap();
    assert!(pg.contains("FOR UPDATE SKIP LOCKED"));
    assert!(pg.contains("$1"));

    let mysql = Dialect::MySql.lock_processor().unwrap();
    assert!(mysql.contains("FOR UPDATE SKIP LOCKED"));
    assert!(mysql.contains('?'));
}

#[test]
fn read_outgoing_batch_uses_limit() {
    let pg = Dialect::Postgres.read_outgoing_batch(50);
    assert!(pg.contains("$1"));
    assert!(pg.contains("$2"));
    assert!(!pg.contains("$3"));
    assert!(pg.contains("seq > $2"));
    assert!(pg.contains("ORDER BY seq"));
    assert!(pg.contains("LIMIT 50"));

    let mysql = Dialect::MySql.read_outgoing_batch(50);
    assert!(mysql.contains('?'));
    assert!(!mysql.contains('$'));
    assert!(mysql.contains("seq > ?"));
    assert!(mysql.contains("LIMIT 50"));
}

#[test]
fn build_read_body_batch_placeholders() {
    let pg = Dialect::Postgres.build_read_body_batch(3);
    assert!(pg.contains("$1, $2, $3"));
    assert!(pg.contains("SELECT id, payload, payload_type, created_at"));

    let mysql = Dialect::MySql.build_read_body_batch(3);
    assert!(mysql.contains("?, ?, ?"));
    assert!(!mysql.contains('$'));
}

#[test]
fn build_delete_body_batch_placeholders() {
    let pg = Dialect::Postgres.build_delete_body_batch(3);
    assert!(pg.contains("$1, $2, $3"));
    assert!(pg.contains("DELETE FROM modkit_outbox_body"));

    let mysql = Dialect::MySql.build_delete_body_batch(3);
    assert!(mysql.contains("?, ?, ?"));
}

#[test]
fn advance_processed_seq_placeholders() {
    let pg = Dialect::Postgres.advance_processed_seq();
    assert!(pg.contains("$1"));
    assert!(pg.contains("$2"));
    assert!(pg.contains("attempts = 0"));

    let mysql = Dialect::MySql.advance_processed_seq();
    assert!(mysql.contains('?'));
    assert!(!mysql.contains('$'));
}

#[test]
fn record_retry_placeholders() {
    let pg = Dialect::Postgres.record_retry();
    assert!(pg.contains("attempts + 1"));
    assert!(pg.contains("$1"));
    assert!(pg.contains("$2"));

    let mysql = Dialect::MySql.record_retry();
    assert!(mysql.contains('?'));
}

#[test]
fn insert_dead_letter_placeholders() {
    let pg = Dialect::Postgres.insert_dead_letter();
    assert!(pg.contains("$1"));
    assert!(pg.contains("$7"));
    assert!(pg.contains("payload"));
    assert!(pg.contains("payload_type"));

    let mysql = Dialect::MySql.insert_dead_letter();
    assert!(mysql.contains('?'));
    assert!(!mysql.contains('$'));
}

#[test]
fn bump_vacuum_counter_placeholders() {
    let pg = Dialect::Postgres.bump_vacuum_counter();
    assert!(pg.contains("$1"));
    assert!(pg.contains("modkit_outbox_vacuum_counter"));
    assert!(pg.contains("counter + 1"));

    let mysql = Dialect::MySql.bump_vacuum_counter();
    assert!(mysql.contains('?'));
    assert!(!mysql.contains('$'));
}

#[test]
fn fetch_dirty_partitions_placeholders() {
    let pg = Dialect::Postgres.fetch_dirty_partitions();
    assert!(pg.contains("$1"));
    assert!(pg.contains("$2"));
    assert!(pg.contains("counter > 0"));
    assert!(pg.contains("ORDER BY partition_id"));

    let mysql = Dialect::MySql.fetch_dirty_partitions();
    assert!(mysql.contains('?'));
    assert!(!mysql.contains('$'));
}

#[test]
fn decrement_vacuum_counter_placeholders() {
    let pg = Dialect::Postgres.decrement_vacuum_counter();
    assert!(pg.contains("GREATEST"));
    assert!(pg.contains("$1"));
    assert!(pg.contains("$2"));

    let sqlite = Dialect::Sqlite.decrement_vacuum_counter();
    assert!(sqlite.contains("MAX"));
    assert!(sqlite.contains("$1"));

    let mysql = Dialect::MySql.decrement_vacuum_counter();
    assert!(mysql.contains("GREATEST"));
    assert!(mysql.contains('?'));
}

#[test]
fn reset_vacuum_counter_placeholders() {
    let pg = Dialect::Postgres.reset_vacuum_counter();
    assert!(pg.contains("counter = 0"));
    assert!(pg.contains("$1"));

    let mysql = Dialect::MySql.reset_vacuum_counter();
    assert!(mysql.contains('?'));
}

#[test]
fn insert_vacuum_counter_row_placeholders() {
    let pg = Dialect::Postgres.insert_vacuum_counter_row();
    assert!(pg.contains("$1"));
    assert!(pg.contains("ON CONFLICT"));

    let sqlite = Dialect::Sqlite.insert_vacuum_counter_row();
    assert!(sqlite.contains("INSERT OR IGNORE"));

    let mysql = Dialect::MySql.insert_vacuum_counter_row();
    assert!(mysql.contains("INSERT IGNORE"));
    assert!(mysql.contains('?'));
}

#[test]
fn vacuum_cleanup_placeholders() {
    let pg = Dialect::Postgres.vacuum_cleanup();
    assert!(pg.select_outgoing_chunk.contains("$1"));
    assert!(pg.select_outgoing_chunk.contains("$2"));
    assert!(pg.select_outgoing_chunk.contains("$3"));

    let mysql = Dialect::MySql.vacuum_cleanup();
    assert!(mysql.select_outgoing_chunk.contains('?'));
}
