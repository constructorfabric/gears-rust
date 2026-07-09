//! Database contention detection utility.
//!
//! Detects transient lock-contention errors that are safe to retry.
//! The entire transaction must be retried from `BEGIN` — not just the
//! failing statement — because the database has already rolled it back.
//!
//! # Covered engines
//!
//! * **`MySQL` / `MariaDB` / Percona `XtraDB` Cluster** — `InnoDB` deadlock
//!   (SQLSTATE `40001`) and Galera/wsrep certification conflicts that abort
//!   the transaction and require a full retry.
//!   `InnoDB` detects deadlocks instantly and rolls back one transaction.
//!
//!   > "Always be prepared to re-issue a transaction if it fails due to
//!   > deadlock. Deadlocks are not dangerous. Just try again."
//!   > — [MySQL 8.0 Reference Manual, InnoDB Deadlocks](https://dev.mysql.com/doc/refman/8.0/en/innodb-deadlocks.html)
//!
//! * **`PostgreSQL`** — serialization failure (SQLSTATE `40001`) and
//!   deadlock detected (SQLSTATE `40P01`).
//!
//!   > "Applications using this level must be prepared to retry transactions
//!   > due to serialization failures."
//!   > — [PostgreSQL docs, Transaction Isolation](https://www.postgresql.org/docs/current/transaction-iso.html#XACT-SERIALIZABLE)
//!
//! * **`SQLite`** — `SQLITE_BUSY` (code 5) and `SQLITE_BUSY_SNAPSHOT` (code 517).
//!   `SQLite` supports only one writer at a time; concurrent writers receive
//!   `SQLITE_BUSY` when the `busy_timeout` expires, or `SQLITE_BUSY_SNAPSHOT`
//!   immediately when a WAL snapshot cannot be upgraded.
//!   See [Result Codes — SQLITE_BUSY](https://www.sqlite.org/rescode.html#busy).
//!
//! # Backend dispatch
//!
//! The caller must supply the [`DbBackend`] so that pattern matching is scoped
//! to the correct engine, avoiding false positives from shared SQLSTATE codes
//! (e.g., `40001` means different things in `MySQL` vs `PostgreSQL`).
//!
//! This gear provides detection helpers for callers that manage their own
//! transaction lifecycle (e.g., the outbox sequencer).

use sea_orm::{DbBackend, DbErr};

/// `MySQL` deadlock SQLSTATE code.
const MYSQL_DEADLOCK_SQLSTATE: &str = "40001";
const MYSQL_DEADLOCK_MSG: &str = "deadlock";
const MYSQL_WSREP_DEADLOCK_MSG: &str = "wsrep detected deadlock/conflict";
const MYSQL_WSREP_CERTIFICATION_ERROR_MSG: &str = "transaction failed due to certification error";
const MYSQL_WSREP_CANNOT_CERTIFY_MSG: &str = "transaction cannot be certified";
const MYSQL_WSREP_WRITE_SET_CONFLICT_MSG: &str = "write-set conflict";
const MYSQL_WSREP_CERTIFICATION_FAILURE_MSG: &str =
    "transaction rolled back due to certification failure";
const MYSQL_RESTART_MSG: &str = "try restarting transaction";

/// `PostgreSQL` retryable SQLSTATE codes.
const PG_SERIALIZATION_FAILURE: &str = "40001";
const PG_DEADLOCK_DETECTED: &str = "40P01";

/// `PostgreSQL` retryable error MESSAGE fragments. sea-orm/sqlx surface the
/// condition through the error's *message text* on the `Display` path, not the
/// numeric SQLSTATE — so matching the code alone (`40001` / `40P01`) misses real
/// serialization failures and deadlocks (e.g. `"could not serialize access due
/// to concurrent update"` carries no `"40001"` substring). Matching the message
/// is the reliable signal here. Message text is `lc_messages`-dependent, so the
/// numeric-code checks above remain as a locale-independent belt-and-suspenders.
const PG_SERIALIZATION_MSG: &str = "could not serialize access";
const PG_DEADLOCK_MSG: &str = "deadlock detected";

/// `SQLite` error codes for write contention.
///
/// sqlx surfaces these as `"error returned from database: (code: N) database is locked"`.
const SQLITE_BUSY_CODE: &str = "(code: 5)";
const SQLITE_BUSY_SNAPSHOT_CODE: &str = "(code: 517)";
const SQLITE_LOCKED_MSG: &str = "database is locked";

/// Returns `true` if the error is a transient lock-contention error that is
/// safe to retry.
///
/// Covers:
/// * `MySQL` / `MariaDB` deadlock — SQLSTATE `40001`
/// * `PostgreSQL` serialization failure (`40001`) / deadlock (`40P01`)
/// * `SQLite` `SQLITE_BUSY` (code 5) — `busy_timeout` expired
/// * `SQLite` `SQLITE_BUSY_SNAPSHOT` (code 517) — WAL snapshot conflict
///
/// Detection is based on the error's string representation, which avoids a
/// direct dependency on `sqlx` types.
#[must_use]
pub fn is_retryable_contention(backend: DbBackend, err: &DbErr) -> bool {
    match err {
        DbErr::Exec(runtime_err) | DbErr::Query(runtime_err) => {
            let msg = runtime_err.to_string();
            match backend {
                DbBackend::MySql => is_mysql_deadlock(&msg),
                DbBackend::Postgres => is_pg_contention(&msg),
                DbBackend::Sqlite => is_sqlite_busy(&msg),
            }
        }
        _ => false,
    }
}

fn is_mysql_deadlock(msg: &str) -> bool {
    let msg = msg.to_ascii_lowercase();
    msg.contains(MYSQL_DEADLOCK_SQLSTATE)
        || msg.contains(MYSQL_DEADLOCK_MSG)
        || msg.contains(MYSQL_WSREP_DEADLOCK_MSG)
        || msg.contains(MYSQL_WSREP_CERTIFICATION_ERROR_MSG)
        || msg.contains(MYSQL_WSREP_CANNOT_CERTIFY_MSG)
        || msg.contains(MYSQL_WSREP_WRITE_SET_CONFLICT_MSG)
        || msg.contains(MYSQL_WSREP_CERTIFICATION_FAILURE_MSG)
        || msg.contains(MYSQL_RESTART_MSG)
}

fn is_pg_contention(msg: &str) -> bool {
    msg.contains(PG_SERIALIZATION_FAILURE)
        || msg.contains(PG_DEADLOCK_DETECTED)
        || msg.contains(PG_SERIALIZATION_MSG)
        || msg.contains(PG_DEADLOCK_MSG)
}

fn is_sqlite_busy(msg: &str) -> bool {
    (msg.contains(SQLITE_BUSY_CODE) || msg.contains(SQLITE_BUSY_SNAPSHOT_CODE))
        && msg.contains(SQLITE_LOCKED_MSG)
}

#[cfg(test)]
mod tests {
    use sea_orm::RuntimeErr;

    use super::*;

    fn exec_err(msg: &str) -> DbErr {
        DbErr::Exec(RuntimeErr::Internal(msg.to_owned()))
    }

    fn query_err(msg: &str) -> DbErr {
        DbErr::Query(RuntimeErr::Internal(msg.to_owned()))
    }

    // ── MySQL ────────────────────────────────────────────────────────

    #[test]
    fn mysql_deadlock_detected() {
        let err = exec_err("MySqlError { ... SQLSTATE 40001: Deadlock found ... }");
        assert!(is_retryable_contention(DbBackend::MySql, &err));
    }

    #[test]
    fn mysql_wsrep_certification_conflict_detected() {
        let err =
            exec_err("MySqlError: WSREP detected deadlock/conflict and aborted the transaction");
        assert!(is_retryable_contention(DbBackend::MySql, &err));
    }

    #[test]
    fn mysql_wsrep_write_set_conflict_detected() {
        let err = exec_err("MySqlError: WSREP: Transaction failed due to write-set conflict");
        assert!(is_retryable_contention(DbBackend::MySql, &err));
    }

    #[test]
    fn unrelated_mysql_wsrep_message_not_retryable() {
        let err = exec_err(
            "MySqlError: WSREP provider failed certification metadata validation permanently",
        );
        assert!(!is_retryable_contention(DbBackend::MySql, &err));
    }

    #[test]
    fn mysql_restart_transaction_detected() {
        let err = exec_err("MySqlError: Lock wait timeout exceeded; try restarting transaction");
        assert!(is_retryable_contention(DbBackend::MySql, &err));
    }

    // ── PostgreSQL ────────────────────────────────────────────────────

    #[test]
    fn pg_serialization_failure_detected() {
        let err = exec_err("error returned from database: error with SQLSTATE 40001");
        assert!(is_retryable_contention(DbBackend::Postgres, &err));
    }

    #[test]
    fn pg_deadlock_detected() {
        let err = exec_err("error returned from database: error with SQLSTATE 40P01");
        assert!(is_retryable_contention(DbBackend::Postgres, &err));
    }

    #[test]
    fn pg_serialization_failure_by_message_detected() {
        // Real sea-orm/sqlx Display carries the message, not the numeric
        // SQLSTATE — this is the exact text Postgres emits for 40001 and the
        // concurrent-DELETE regression that the numeric-only match missed.
        let err = exec_err(
            "error returned from database: could not serialize access due to concurrent update",
        );
        assert!(is_retryable_contention(DbBackend::Postgres, &err));
    }

    #[test]
    fn pg_deadlock_by_message_detected() {
        let err = exec_err("error returned from database: deadlock detected");
        assert!(is_retryable_contention(DbBackend::Postgres, &err));
    }

    // ── SQLite BUSY (code 5) ─────────────────────────────────────────

    #[test]
    fn sqlite_busy_exec_detected() {
        let err =
            exec_err("Execution Error: error returned from database: (code: 5) database is locked");
        assert!(is_retryable_contention(DbBackend::Sqlite, &err));
    }

    #[test]
    fn sqlite_busy_query_detected() {
        let err =
            query_err("Query Error: error returned from database: (code: 5) database is locked");
        assert!(is_retryable_contention(DbBackend::Sqlite, &err));
    }

    // ── SQLite BUSY_SNAPSHOT (code 517) ──────────────────────────────

    #[test]
    fn sqlite_busy_snapshot_detected() {
        let err = exec_err(
            "Execution Error: error returned from database: (code: 517) database is locked",
        );
        assert!(is_retryable_contention(DbBackend::Sqlite, &err));
    }

    // ── Cross-engine isolation ──────────────────────────────────────

    #[test]
    fn sqlstate_40001_not_retryable_on_sqlite() {
        let err = exec_err("SQLSTATE 40001");
        assert!(!is_retryable_contention(DbBackend::Sqlite, &err));
    }

    #[test]
    fn sqlite_busy_not_retryable_on_mysql() {
        let err =
            exec_err("Execution Error: error returned from database: (code: 5) database is locked");
        assert!(!is_retryable_contention(DbBackend::MySql, &err));
    }

    // ── Negative cases ───────────────────────────────────────────────

    #[test]
    fn sqlite_constraint_not_retryable() {
        let err = exec_err(
            "Execution Error: error returned from database: (code: 19) UNIQUE constraint failed",
        );
        assert!(!is_retryable_contention(DbBackend::Sqlite, &err));
    }

    #[test]
    fn unrelated_errors_not_retryable() {
        assert!(!is_retryable_contention(
            DbBackend::Sqlite,
            &DbErr::Custom("something".into()),
        ));
        assert!(!is_retryable_contention(
            DbBackend::Postgres,
            &DbErr::RecordNotFound("x".into()),
        ));
    }

    #[test]
    fn code_5_without_locked_msg_not_retryable() {
        let err = exec_err("error returned from database: (code: 5) something else");
        assert!(!is_retryable_contention(DbBackend::Sqlite, &err));
    }
}
