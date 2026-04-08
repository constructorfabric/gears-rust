//! `MySQL` deadlock detection utility.
//!
//! > "Always be prepared to re-issue a transaction if it fails due to deadlock.
//! > Deadlocks are not dangerous. Just try again."
//! > — [`MySQL` 8.0 Reference Manual, `InnoDB` Deadlocks](https://dev.mysql.com/doc/refman/8.0/en/innodb-deadlocks.html)
//!
//! `InnoDB` detects deadlocks instantly and rolls back one transaction (the victim).
//! SQLSTATE `40001` signals a serialization failure that is always safe to retry.
//! This module provides detection helpers for use by callers that manage their
//! own transaction lifecycle (e.g., the outbox sequencer).

use sea_orm::DbErr;

/// `MySQL` deadlock SQLSTATE code.
const DEADLOCK_SQLSTATE: &str = "40001";

/// Returns `true` if the error is a `MySQL`/`MariaDB` deadlock (SQLSTATE `40001`).
///
/// Always returns `false` for `Postgres` and `SQLite` errors — those engines
/// resolve lock conflicts differently (`SKIP LOCKED`, single-writer).
///
/// Detection is based on the error's string representation containing the
/// SQLSTATE code, which avoids a direct dependency on `sqlx` types.
#[must_use]
pub fn is_deadlock(err: &DbErr) -> bool {
    match err {
        DbErr::Exec(runtime_err) | DbErr::Query(runtime_err) => {
            let msg = runtime_err.to_string();
            msg.contains(DEADLOCK_SQLSTATE)
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn non_deadlock_errors_return_false() {
        assert!(!is_deadlock(&DbErr::Custom("something".into())));
        assert!(!is_deadlock(&DbErr::RecordNotFound("x".into())));
    }
}
