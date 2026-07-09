#![allow(dead_code)]

use std::fmt::Write as _;

use sea_orm::DbBackend;

use super::tables::OutboxTables;

/// Backend-specific SQL dialect for the outbox gear.
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
    pub select_outgoing_chunk: String,
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
    UpdateReturning(String),
    /// `MySQL`: `UPDATE` then `SELECT` as two separate statements.
    UpdateThenSelect { update: String, select: String },
}

// -- Registration queries --

impl Dialect {
    pub fn register_queue_select(self, tables: &OutboxTables) -> String {
        match self {
            Self::Postgres | Self::Sqlite => format!(
                "SELECT id FROM {} WHERE queue = $1 ORDER BY partition ASC",
                tables.partitions()
            ),
            Self::MySql => format!(
                "SELECT id FROM {} WHERE queue = ? ORDER BY `partition` ASC",
                tables.partitions()
            ),
        }
    }

    pub fn register_queue_insert(self, tables: &OutboxTables) -> String {
        match self {
            Self::Postgres => format!(
                "INSERT INTO {} (queue, partition) \
                 VALUES ($1, $2) ON CONFLICT (queue, partition) DO NOTHING",
                tables.partitions()
            ),
            Self::Sqlite => format!(
                "INSERT OR IGNORE INTO {} (queue, partition) VALUES ($1, $2)",
                tables.partitions()
            ),
            Self::MySql => format!(
                "INSERT IGNORE INTO {} (queue, `partition`) VALUES (?, ?)",
                tables.partitions()
            ),
        }
    }
}

// -- Single-row insert queries --

impl Dialect {
    /// Combined CTE: insert body + incoming in a single round-trip.
    /// Returns the incoming row id. Only for backends that support RETURNING.
    pub fn insert_body_and_incoming_cte(self, tables: &OutboxTables) -> Option<String> {
        match self {
            Self::Postgres => Some(format!(
                "WITH b AS (\
                   INSERT INTO {} (payload, payload_type) \
                   VALUES ($1, $2) RETURNING id\
                 ) \
                 INSERT INTO {} (partition_id, body_id) \
                 SELECT $3, id FROM b RETURNING id",
                tables.body(),
                tables.incoming()
            )),
            // SQLite: writable CTEs require 3.35+; the bundled libsqlite3
            // version may be older, so fall back to two separate INSERTs.
            Self::Sqlite | Self::MySql => None,
        }
    }

    pub fn insert_body(self, tables: &OutboxTables) -> String {
        match self {
            Self::Postgres | Self::Sqlite => format!(
                "INSERT INTO {} (payload, payload_type) VALUES ($1, $2) RETURNING id",
                tables.body()
            ),
            Self::MySql => format!(
                "INSERT INTO {} (payload, payload_type) VALUES (?, ?)",
                tables.body()
            ),
        }
    }

    pub fn insert_incoming(self, tables: &OutboxTables) -> String {
        match self {
            Self::Postgres | Self::Sqlite => format!(
                "INSERT INTO {} (partition_id, body_id) VALUES ($1, $2) RETURNING id",
                tables.incoming()
            ),
            Self::MySql => format!(
                "INSERT INTO {} (partition_id, body_id) VALUES (?, ?)",
                tables.incoming()
            ),
        }
    }

    pub(super) fn supports_returning(self) -> bool {
        match self {
            Self::Postgres | Self::Sqlite => true,
            Self::MySql => false,
        }
    }

    /// Returns the `MySQL` query to retrieve the last auto-generated ID.
    pub(super) fn last_insert_id() -> &'static str {
        "SELECT CAST(LAST_INSERT_ID() AS SIGNED) AS id"
    }
}

// -- Batch insert builders --

impl Dialect {
    /// Build a multi-row INSERT for body rows.
    ///
    /// `MySQL` note: consecutive auto-increment IDs are guaranteed by `InnoDB`
    /// for a single multi-row INSERT when `innodb_autoinc_lock_mode` is 0 or 1.
    pub fn build_insert_body_batch(self, tables: &OutboxTables, count: usize) -> String {
        let mut sql = format!(
            "INSERT INTO {} (payload, payload_type) VALUES ",
            tables.body()
        );
        self.append_value_tuples(&mut sql, count, 2);
        if self.supports_returning() {
            sql.push_str(" RETURNING id");
        }
        sql
    }

    pub fn build_insert_incoming_batch(self, tables: &OutboxTables, count: usize) -> String {
        let mut sql = format!(
            "INSERT INTO {} (partition_id, body_id) VALUES ",
            tables.incoming()
        );
        self.append_value_tuples(&mut sql, count, 2);
        if self.supports_returning() {
            sql.push_str(" RETURNING id");
        }
        sql
    }

    /// Build `SELECT id, payload, payload_type, created_at FROM toolkit_outbox_body WHERE id IN (...)`.
    pub fn build_read_body_batch(self, tables: &OutboxTables, count: usize) -> String {
        let mut sql = format!(
            "SELECT id, payload, payload_type, created_at FROM {} WHERE id IN (",
            tables.body()
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

// -- Sequencer queries --

impl Dialect {
    pub fn claim_incoming(self, tables: &OutboxTables, batch_size: u32) -> ClaimSql {
        match self {
            Self::Postgres => ClaimSql {
                select: format!(
                    "SELECT id, body_id \
                     FROM {} \
                     WHERE partition_id = $1 \
                     ORDER BY id \
                     LIMIT {batch_size} \
                     FOR UPDATE SKIP LOCKED",
                    tables.incoming()
                ),
            },
            Self::Sqlite => ClaimSql {
                select: format!(
                    "SELECT id, body_id \
                     FROM {} \
                     WHERE partition_id = $1 \
                     ORDER BY id \
                     LIMIT {batch_size}",
                    tables.incoming()
                ),
            },
            // SKIP LOCKED prevents InnoDB gap-lock deadlocks when
            // multiple sequencers claim from adjacent partitions.
            Self::MySql => ClaimSql {
                select: format!(
                    "SELECT id, body_id \
                     FROM {} \
                     WHERE partition_id = ? \
                     ORDER BY id \
                     LIMIT {batch_size} \
                     FOR UPDATE SKIP LOCKED",
                    tables.incoming()
                ),
            },
        }
    }

    /// Build `DELETE FROM toolkit_outbox_incoming WHERE id IN ($1, $2, ...)`.
    pub fn delete_incoming_batch(self, tables: &OutboxTables, count: usize) -> String {
        let mut sql = format!("DELETE FROM {} WHERE id IN (", tables.incoming());
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

    pub fn allocate_sequences(self, tables: &OutboxTables) -> AllocSql {
        match self {
            Self::Postgres | Self::Sqlite => AllocSql::UpdateReturning(format!(
                "UPDATE {} \
                 SET sequence = sequence + $2 \
                 WHERE id = $1 \
                 RETURNING sequence - $2 AS start_seq",
                tables.partitions()
            )),
            Self::MySql => AllocSql::UpdateThenSelect {
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

    pub fn build_insert_outgoing_batch(self, tables: &OutboxTables, count: usize) -> String {
        let mut sql = format!(
            "INSERT INTO {} (partition_id, body_id, seq) VALUES ",
            tables.outgoing()
        );
        self.append_value_tuples(&mut sql, count, 3);
        sql
    }

    pub fn lock_partition(self, tables: &OutboxTables) -> Option<String> {
        match self {
            Self::Postgres => Some(format!(
                "SELECT id FROM {} WHERE id = $1 FOR UPDATE SKIP LOCKED",
                tables.partitions()
            )),
            Self::MySql => Some(format!(
                "SELECT id FROM {} WHERE id = ? FOR UPDATE SKIP LOCKED",
                tables.partitions()
            )),
            Self::Sqlite => None,
        }
    }

    /// Cold-path discovery: find all partition IDs with pending incoming rows.
    /// Uses the existing `(partition_id, id)` index for an index-only skip scan.
    /// Same SQL for all backends — `DISTINCT` on the leading index column is portable.
    pub fn discover_dirty_partitions(self, tables: &OutboxTables) -> String {
        // Same SQL for all backends — DISTINCT on leading index column is portable.
        match self {
            Self::Postgres | Self::Sqlite | Self::MySql => {
                format!("SELECT DISTINCT partition_id FROM {}", tables.incoming())
            }
        }
    }
}

// -- Processor queries --

impl Dialect {
    pub fn insert_processor_row(self, tables: &OutboxTables) -> String {
        match self {
            Self::Postgres => format!(
                "INSERT INTO {} (partition_id) \
                 VALUES ($1) ON CONFLICT (partition_id) DO NOTHING",
                tables.processor()
            ),
            Self::Sqlite => format!(
                "INSERT OR IGNORE INTO {} (partition_id) VALUES ($1)",
                tables.processor()
            ),
            Self::MySql => format!(
                "INSERT IGNORE INTO {} (partition_id) VALUES (?)",
                tables.processor()
            ),
        }
    }

    pub fn lock_processor(self, tables: &OutboxTables) -> Option<String> {
        match self {
            Self::Postgres => Some(format!(
                "SELECT partition_id, processed_seq, attempts \
                 FROM {} WHERE partition_id = $1 FOR UPDATE SKIP LOCKED",
                tables.processor()
            )),
            Self::MySql => Some(format!(
                "SELECT partition_id, processed_seq, attempts \
                 FROM {} WHERE partition_id = ? FOR UPDATE SKIP LOCKED",
                tables.processor()
            )),
            Self::Sqlite => None,
        }
    }

    pub fn read_outgoing_batch(self, tables: &OutboxTables, batch_size: u32) -> String {
        match self {
            Self::Postgres | Self::Sqlite => format!(
                "SELECT id, body_id, seq \
                 FROM {} \
                 WHERE partition_id = $1 AND seq > $2 \
                 ORDER BY seq \
                 LIMIT {batch_size}",
                tables.outgoing()
            ),
            Self::MySql => format!(
                "SELECT id, body_id, seq \
                 FROM {} \
                 WHERE partition_id = ? AND seq > ? \
                 ORDER BY seq \
                 LIMIT {batch_size}",
                tables.outgoing()
            ),
        }
    }

    pub fn advance_processed_seq(self, tables: &OutboxTables) -> String {
        match self {
            Self::Postgres | Self::Sqlite => format!(
                "UPDATE {} \
                 SET processed_seq = $1, attempts = 0, last_error = NULL \
                 WHERE partition_id = $2",
                tables.processor()
            ),
            Self::MySql => format!(
                "UPDATE {} \
                 SET processed_seq = ?, attempts = 0, last_error = NULL \
                 WHERE partition_id = ?",
                tables.processor()
            ),
        }
    }

    pub fn record_retry(self, tables: &OutboxTables) -> String {
        match self {
            Self::Postgres | Self::Sqlite => format!(
                "UPDATE {} \
                 SET attempts = attempts + 1, last_error = $1 \
                 WHERE partition_id = $2",
                tables.processor()
            ),
            Self::MySql => format!(
                "UPDATE {} \
                 SET attempts = attempts + 1, last_error = ? \
                 WHERE partition_id = ?",
                tables.processor()
            ),
        }
    }

    pub fn insert_dead_letter(self, tables: &OutboxTables) -> String {
        match self {
            Self::Postgres | Self::Sqlite => format!(
                "INSERT INTO {} \
                 (partition_id, seq, payload, payload_type, created_at, last_error, attempts) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7)",
                tables.dead_letters()
            ),
            Self::MySql => format!(
                "INSERT INTO {} \
                 (partition_id, seq, payload, payload_type, created_at, last_error, attempts) \
                 VALUES (?, ?, ?, ?, ?, ?, ?)",
                tables.dead_letters()
            ),
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
    pub fn lease_acquire(self, tables: &OutboxTables) -> String {
        match self {
            Self::Postgres => format!(
                "UPDATE {} \
                 SET locked_by = $1, locked_until = NOW() + $2 * INTERVAL '1 second', \
                     attempts = attempts + 1 \
                 WHERE partition_id = $3 \
                   AND (locked_by IS NULL OR locked_until < NOW()) \
                 RETURNING processed_seq, attempts",
                tables.processor()
            ),
            Self::Sqlite => format!(
                "UPDATE {} \
                 SET locked_by = $1, locked_until = datetime('now', '+' || $2 || ' seconds'), \
                     attempts = attempts + 1 \
                 WHERE partition_id = $3 \
                   AND (locked_by IS NULL OR locked_until < datetime('now')) \
                 RETURNING processed_seq, attempts",
                tables.processor()
            ),
            Self::MySql => format!(
                "UPDATE {} \
                 SET locked_by = ?, locked_until = DATE_ADD(NOW(6), INTERVAL ? SECOND), \
                     attempts = attempts + 1 \
                 WHERE partition_id = ? \
                   AND (locked_by IS NULL OR locked_until < NOW(6))",
                tables.processor()
            ),
        }
    }

    /// Ack with lease guard: advance `processed_seq` only if we still own the lease.
    pub fn lease_ack_advance(self, tables: &OutboxTables) -> String {
        match self {
            Self::Postgres | Self::Sqlite => format!(
                "UPDATE {} \
                 SET processed_seq = $1, attempts = 0, last_error = NULL, \
                     locked_by = NULL, locked_until = NULL \
                 WHERE partition_id = $2 AND locked_by = $3",
                tables.processor()
            ),
            Self::MySql => format!(
                "UPDATE {} \
                 SET processed_seq = ?, attempts = 0, last_error = NULL, \
                     locked_by = NULL, locked_until = NULL \
                 WHERE partition_id = ? AND locked_by = ?",
                tables.processor()
            ),
        }
    }

    /// Record retry with lease guard.
    ///
    /// Does NOT increment `attempts` — already incremented during
    /// [`lease_acquire`](Self::lease_acquire). Just records the error
    /// and releases the lease.
    pub fn lease_record_retry(self, tables: &OutboxTables) -> String {
        match self {
            Self::Postgres | Self::Sqlite => format!(
                "UPDATE {} \
                 SET last_error = $1, locked_by = NULL, locked_until = NULL \
                 WHERE partition_id = $2 AND locked_by = $3",
                tables.processor()
            ),
            Self::MySql => format!(
                "UPDATE {} \
                 SET last_error = ?, locked_by = NULL, locked_until = NULL \
                 WHERE partition_id = ? AND locked_by = ?",
                tables.processor()
            ),
        }
    }

    /// Release a lease without changing state (e.g. on empty partition).
    pub fn lease_release(self, tables: &OutboxTables) -> String {
        match self {
            Self::Postgres | Self::Sqlite => format!(
                "UPDATE {} \
                 SET attempts = 0, locked_by = NULL, locked_until = NULL \
                 WHERE partition_id = $1 AND locked_by = $2",
                tables.processor()
            ),
            Self::MySql => format!(
                "UPDATE {} \
                 SET attempts = 0, locked_by = NULL, locked_until = NULL \
                 WHERE partition_id = ? AND locked_by = ?",
                tables.processor()
            ),
        }
    }

    /// Vacuum: bounded-chunk cleanup.
    ///
    /// Returns SQL to SELECT a bounded chunk of (id, `body_id`) from outgoing.
    /// The caller deletes those rows by ID, then loops while
    /// `deleted == batch_size`.
    pub fn vacuum_cleanup(self, tables: &OutboxTables) -> VacuumSql {
        match self {
            Self::Postgres | Self::Sqlite => VacuumSql {
                select_outgoing_chunk: format!(
                    "SELECT id, body_id FROM {} \
                     WHERE partition_id = $1 AND seq <= $2 \
                     ORDER BY seq LIMIT $3",
                    tables.outgoing()
                ),
            },
            Self::MySql => VacuumSql {
                select_outgoing_chunk: format!(
                    "SELECT id, body_id FROM {} \
                     WHERE partition_id = ? AND seq <= ? \
                     ORDER BY seq LIMIT ?",
                    tables.outgoing()
                ),
            },
        }
    }

    /// Build `DELETE FROM toolkit_outbox_outgoing WHERE id IN ($1, $2, ...)`.
    pub fn build_delete_outgoing_batch(self, tables: &OutboxTables, count: usize) -> String {
        let mut sql = format!("DELETE FROM {} WHERE id IN (", tables.outgoing());
        self.append_in_placeholders(&mut sql, count);
        sql.push(')');
        sql
    }

    /// Build `DELETE FROM toolkit_outbox_body WHERE id IN (...)`.
    pub fn build_delete_body_batch(self, tables: &OutboxTables, count: usize) -> String {
        let mut sql = format!("DELETE FROM {} WHERE id IN (", tables.body());
        self.append_in_placeholders(&mut sql, count);
        sql.push(')');
        sql
    }

    pub fn read_processor(self, tables: &OutboxTables) -> String {
        match self {
            Self::Postgres | Self::Sqlite => format!(
                "SELECT processed_seq, attempts FROM {} WHERE partition_id = $1",
                tables.processor()
            ),
            Self::MySql => format!(
                "SELECT processed_seq, attempts FROM {} WHERE partition_id = ?",
                tables.processor()
            ),
        }
    }
}

// -- Vacuum counter queries --

impl Dialect {
    /// Bump the vacuum counter for a partition (called by processor on ack).
    pub fn bump_vacuum_counter(self, tables: &OutboxTables) -> String {
        match self {
            Self::Postgres | Self::Sqlite => format!(
                "UPDATE {} SET counter = counter + 1 WHERE partition_id = $1",
                tables.vacuum_counter()
            ),
            Self::MySql => format!(
                "UPDATE {} SET counter = counter + 1 WHERE partition_id = ?",
                tables.vacuum_counter()
            ),
        }
    }

    /// Fetch dirty partitions paginated by `partition_id` cursor.
    /// Returns `(partition_id, counter)` for partitions with `counter > 0`.
    pub fn fetch_dirty_partitions(self, tables: &OutboxTables) -> String {
        match self {
            Self::Postgres | Self::Sqlite => format!(
                "SELECT partition_id, counter \
                 FROM {} \
                 WHERE counter > 0 AND partition_id > $1 \
                 ORDER BY partition_id LIMIT $2",
                tables.vacuum_counter()
            ),
            Self::MySql => format!(
                "SELECT partition_id, counter \
                 FROM {} \
                 WHERE counter > 0 AND partition_id > ? \
                 ORDER BY partition_id LIMIT ?",
                tables.vacuum_counter()
            ),
        }
    }

    /// Decrement vacuum counter by snapshot value, floored at 0.
    pub fn decrement_vacuum_counter(self, tables: &OutboxTables) -> String {
        match self {
            Self::Postgres => format!(
                "UPDATE {} \
                 SET counter = GREATEST(counter - $1, 0) \
                 WHERE partition_id = $2",
                tables.vacuum_counter()
            ),
            Self::Sqlite => format!(
                "UPDATE {} \
                 SET counter = MAX(counter - $1, 0) \
                 WHERE partition_id = $2",
                tables.vacuum_counter()
            ),
            Self::MySql => format!(
                "UPDATE {} \
                 SET counter = GREATEST(counter - ?, 0) \
                 WHERE partition_id = ?",
                tables.vacuum_counter()
            ),
        }
    }

    /// Reset vacuum counter to 0. Used by integration tests for state cleanup.
    #[cfg(test)]
    pub fn reset_vacuum_counter(self, tables: &OutboxTables) -> String {
        match self {
            Self::Postgres | Self::Sqlite => format!(
                "UPDATE {} SET counter = 0 WHERE partition_id = $1",
                tables.vacuum_counter()
            ),
            Self::MySql => format!(
                "UPDATE {} SET counter = 0 WHERE partition_id = ?",
                tables.vacuum_counter()
            ),
        }
    }

    /// Insert a vacuum counter row (idempotent, for `register_queue`).
    pub fn insert_vacuum_counter_row(self, tables: &OutboxTables) -> String {
        match self {
            Self::Postgres => format!(
                "INSERT INTO {} (partition_id) \
                 VALUES ($1) ON CONFLICT (partition_id) DO NOTHING",
                tables.vacuum_counter()
            ),
            Self::Sqlite => format!(
                "INSERT OR IGNORE INTO {} (partition_id) VALUES ($1)",
                tables.vacuum_counter()
            ),
            Self::MySql => format!(
                "INSERT IGNORE INTO {} (partition_id) VALUES (?)",
                tables.vacuum_counter()
            ),
        }
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    fn tables() -> OutboxTables {
        OutboxTables::default()
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
        assert!(d.insert_body(&tables()).contains("$1"));
        assert!(d.insert_body(&tables()).contains("$2"));
        assert!(d.insert_body(&tables()).contains("RETURNING"));
    }

    #[test]
    fn mysql_uses_question_placeholders() {
        let d = Dialect::MySql;
        assert!(d.insert_body(&tables()).contains('?'));
        assert!(!d.insert_body(&tables()).contains('$'));
        assert!(!d.insert_body(&tables()).contains("RETURNING"));
    }

    #[test]
    fn supports_returning_correct() {
        assert!(Dialect::Postgres.supports_returning());
        assert!(Dialect::Sqlite.supports_returning());
        assert!(!Dialect::MySql.supports_returning());
    }

    #[test]
    fn lock_partition_correct() {
        assert!(Dialect::Postgres.lock_partition(&tables()).is_some());
        assert!(Dialect::MySql.lock_partition(&tables()).is_some());
        assert!(Dialect::Sqlite.lock_partition(&tables()).is_none());
    }

    #[test]
    fn batch_body_pg_placeholder_format() {
        let sql = Dialect::Postgres.build_insert_body_batch(&tables(), 3);
        assert!(sql.contains("($1, $2), ($3, $4), ($5, $6)"));
        assert!(sql.ends_with("RETURNING id"));
    }

    #[test]
    fn batch_body_mysql_placeholder_format() {
        let sql = Dialect::MySql.build_insert_body_batch(&tables(), 3);
        assert!(sql.contains("(?, ?), (?, ?), (?, ?)"));
        assert!(!sql.contains("RETURNING"));
    }

    #[test]
    fn claim_pg_select_ordered_with_for_update() {
        let claim = Dialect::Postgres.claim_incoming(&tables(), 100);
        assert!(claim.select.contains("ORDER BY id"));
        assert!(claim.select.contains("FOR UPDATE SKIP LOCKED"));
        assert!(claim.select.contains("$1"));
    }

    #[test]
    fn claim_sqlite_select_ordered_no_lock() {
        let claim = Dialect::Sqlite.claim_incoming(&tables(), 100);
        assert!(claim.select.contains("ORDER BY id"));
        assert!(!claim.select.contains("FOR UPDATE"));
    }

    #[test]
    fn claim_mysql_select_ordered_with_for_update() {
        let claim = Dialect::MySql.claim_incoming(&tables(), 100);
        assert!(claim.select.contains("ORDER BY id"));
        assert!(claim.select.contains("FOR UPDATE SKIP LOCKED"));
        assert!(claim.select.contains('?'));
    }

    #[test]
    fn delete_incoming_batch_placeholders() {
        let pg = Dialect::Postgres.delete_incoming_batch(&tables(), 3);
        assert!(pg.contains("$1, $2, $3"));
        assert!(pg.contains("DELETE FROM toolkit_outbox_incoming"));

        let mysql = Dialect::MySql.delete_incoming_batch(&tables(), 3);
        assert!(mysql.contains("?, ?, ?"));
    }

    #[test]
    fn alloc_pg_is_update_returning() {
        let alloc = Dialect::Postgres.allocate_sequences(&tables());
        assert!(matches!(alloc, AllocSql::UpdateReturning(_)));
    }

    #[test]
    fn alloc_mysql_is_update_then_select() {
        let alloc = Dialect::MySql.allocate_sequences(&tables());
        assert!(matches!(alloc, AllocSql::UpdateThenSelect { .. }));
    }

    #[test]
    fn mysql_register_queue_backtick_partition() {
        let d = Dialect::MySql;
        assert!(d.register_queue_select(&tables()).contains("`partition`"));
        assert!(d.register_queue_insert(&tables()).contains("`partition`"));
    }

    // -- Processor dialect tests --

    #[test]
    fn insert_processor_row_pg_uses_on_conflict() {
        let sql = Dialect::Postgres.insert_processor_row(&tables());
        assert!(sql.contains("$1"));
        assert!(sql.contains("ON CONFLICT"));
    }

    #[test]
    fn insert_processor_row_sqlite_uses_or_ignore() {
        let sql = Dialect::Sqlite.insert_processor_row(&tables());
        assert!(sql.contains("INSERT OR IGNORE"));
        assert!(sql.contains("$1"));
    }

    #[test]
    fn insert_processor_row_mysql_uses_insert_ignore() {
        let sql = Dialect::MySql.insert_processor_row(&tables());
        assert!(sql.contains("INSERT IGNORE"));
        assert!(sql.contains('?'));
        assert!(!sql.contains('$'));
    }

    #[test]
    fn lock_processor_correct() {
        assert!(Dialect::Postgres.lock_processor(&tables()).is_some());
        assert!(Dialect::MySql.lock_processor(&tables()).is_some());
        assert!(Dialect::Sqlite.lock_processor(&tables()).is_none());

        let pg = Dialect::Postgres.lock_processor(&tables()).unwrap();
        assert!(pg.contains("FOR UPDATE SKIP LOCKED"));
        assert!(pg.contains("$1"));

        let mysql = Dialect::MySql.lock_processor(&tables()).unwrap();
        assert!(mysql.contains("FOR UPDATE SKIP LOCKED"));
        assert!(mysql.contains('?'));
    }

    #[test]
    fn read_outgoing_batch_uses_limit() {
        let pg = Dialect::Postgres.read_outgoing_batch(&tables(), 50);
        assert!(pg.contains("$1"));
        assert!(pg.contains("$2"));
        assert!(!pg.contains("$3"));
        assert!(pg.contains("seq > $2"));
        assert!(pg.contains("ORDER BY seq"));
        assert!(pg.contains("LIMIT 50"));

        let mysql = Dialect::MySql.read_outgoing_batch(&tables(), 50);
        assert!(mysql.contains('?'));
        assert!(!mysql.contains('$'));
        assert!(mysql.contains("seq > ?"));
        assert!(mysql.contains("LIMIT 50"));
    }

    #[test]
    fn build_read_body_batch_placeholders() {
        let pg = Dialect::Postgres.build_read_body_batch(&tables(), 3);
        assert!(pg.contains("$1, $2, $3"));
        assert!(pg.contains("SELECT id, payload, payload_type, created_at"));

        let mysql = Dialect::MySql.build_read_body_batch(&tables(), 3);
        assert!(mysql.contains("?, ?, ?"));
        assert!(!mysql.contains('$'));
    }

    #[test]
    fn build_delete_body_batch_placeholders() {
        let pg = Dialect::Postgres.build_delete_body_batch(&tables(), 3);
        assert!(pg.contains("$1, $2, $3"));
        assert!(pg.contains("DELETE FROM toolkit_outbox_body"));

        let mysql = Dialect::MySql.build_delete_body_batch(&tables(), 3);
        assert!(mysql.contains("?, ?, ?"));
    }

    #[test]
    fn advance_processed_seq_placeholders() {
        let pg = Dialect::Postgres.advance_processed_seq(&tables());
        assert!(pg.contains("$1"));
        assert!(pg.contains("$2"));
        assert!(pg.contains("attempts = 0"));

        let mysql = Dialect::MySql.advance_processed_seq(&tables());
        assert!(mysql.contains('?'));
        assert!(!mysql.contains('$'));
    }

    #[test]
    fn record_retry_placeholders() {
        let pg = Dialect::Postgres.record_retry(&tables());
        assert!(pg.contains("attempts + 1"));
        assert!(pg.contains("$1"));
        assert!(pg.contains("$2"));

        let mysql = Dialect::MySql.record_retry(&tables());
        assert!(mysql.contains('?'));
    }

    #[test]
    fn insert_dead_letter_placeholders() {
        let pg = Dialect::Postgres.insert_dead_letter(&tables());
        assert!(pg.contains("$1"));
        assert!(pg.contains("$7"));
        assert!(pg.contains("payload"));
        assert!(pg.contains("payload_type"));

        let mysql = Dialect::MySql.insert_dead_letter(&tables());
        assert!(mysql.contains('?'));
        assert!(!mysql.contains('$'));
    }

    // -- Vacuum counter dialect tests --

    #[test]
    fn bump_vacuum_counter_placeholders() {
        let pg = Dialect::Postgres.bump_vacuum_counter(&tables());
        assert!(pg.contains("$1"));
        assert!(pg.contains("toolkit_outbox_vacuum_counter"));
        assert!(pg.contains("counter + 1"));

        let mysql = Dialect::MySql.bump_vacuum_counter(&tables());
        assert!(mysql.contains('?'));
        assert!(!mysql.contains('$'));
    }

    #[test]
    fn fetch_dirty_partitions_placeholders() {
        let pg = Dialect::Postgres.fetch_dirty_partitions(&tables());
        assert!(pg.contains("$1"));
        assert!(pg.contains("$2"));
        assert!(pg.contains("counter > 0"));
        assert!(pg.contains("ORDER BY partition_id"));

        let mysql = Dialect::MySql.fetch_dirty_partitions(&tables());
        assert!(mysql.contains('?'));
        assert!(!mysql.contains('$'));
    }

    #[test]
    fn decrement_vacuum_counter_placeholders() {
        let pg = Dialect::Postgres.decrement_vacuum_counter(&tables());
        assert!(pg.contains("GREATEST"));
        assert!(pg.contains("$1"));
        assert!(pg.contains("$2"));

        let sqlite = Dialect::Sqlite.decrement_vacuum_counter(&tables());
        assert!(sqlite.contains("MAX"));
        assert!(sqlite.contains("$1"));

        let mysql = Dialect::MySql.decrement_vacuum_counter(&tables());
        assert!(mysql.contains("GREATEST"));
        assert!(mysql.contains('?'));
    }

    #[test]
    fn reset_vacuum_counter_placeholders() {
        let pg = Dialect::Postgres.reset_vacuum_counter(&tables());
        assert!(pg.contains("counter = 0"));
        assert!(pg.contains("$1"));

        let mysql = Dialect::MySql.reset_vacuum_counter(&tables());
        assert!(mysql.contains('?'));
    }

    #[test]
    fn insert_vacuum_counter_row_placeholders() {
        let pg = Dialect::Postgres.insert_vacuum_counter_row(&tables());
        assert!(pg.contains("$1"));
        assert!(pg.contains("ON CONFLICT"));

        let sqlite = Dialect::Sqlite.insert_vacuum_counter_row(&tables());
        assert!(sqlite.contains("INSERT OR IGNORE"));

        let mysql = Dialect::MySql.insert_vacuum_counter_row(&tables());
        assert!(mysql.contains("INSERT IGNORE"));
        assert!(mysql.contains('?'));
    }

    #[test]
    fn vacuum_cleanup_placeholders() {
        let pg = Dialect::Postgres.vacuum_cleanup(&tables());
        assert!(pg.select_outgoing_chunk.contains("$1"));
        assert!(pg.select_outgoing_chunk.contains("$2"));
        assert!(pg.select_outgoing_chunk.contains("$3"));

        let mysql = Dialect::MySql.vacuum_cleanup(&tables());
        assert!(mysql.select_outgoing_chunk.contains('?'));
    }
}
