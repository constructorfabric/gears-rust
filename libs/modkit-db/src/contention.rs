//! Database contention detection utility.
//!
//! Detects transient lock-contention errors that are safe to retry.
//! The entire transaction must be retried from `BEGIN` — not just the
//! failing statement — because the database has already rolled it back.
//!
//! # Covered engines
//!
//! * **`MySQL` / `MariaDB`** — `InnoDB` deadlock (SQLSTATE `40001`).
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
//! This module provides detection helpers for callers that manage their own
//! transaction lifecycle (e.g., the outbox sequencer).

use sea_orm::{DbBackend, DbErr};

/// `MySQL` deadlock SQLSTATE code.
const MYSQL_DEADLOCK_SQLSTATE: &str = "40001";

/// `PostgreSQL` retryable SQLSTATE codes.
const PG_SERIALIZATION_FAILURE: &str = "40001";
const PG_DEADLOCK_DETECTED: &str = "40P01";

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
    msg.contains(MYSQL_DEADLOCK_SQLSTATE)
}

fn is_pg_contention(msg: &str) -> bool {
    msg.contains(PG_SERIALIZATION_FAILURE) || msg.contains(PG_DEADLOCK_DETECTED)
}

fn is_sqlite_busy(msg: &str) -> bool {
    (msg.contains(SQLITE_BUSY_CODE) || msg.contains(SQLITE_BUSY_SNAPSHOT_CODE))
        && msg.contains(SQLITE_LOCKED_MSG)
}

#[cfg(test)]
#[path = "contention_tests.rs"]
mod tests;
