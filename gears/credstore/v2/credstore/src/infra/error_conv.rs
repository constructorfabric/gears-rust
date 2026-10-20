//! DB error classification helpers for the boundary mapping in
//! [`crate::infra::canonical_mapping`].

use sea_orm::{DbBackend, DbErr};
use toolkit_db::contention::is_retryable_contention;

/// Backend-agnostic adapter. Probes `PostgreSQL` and `SQLite` (the two
/// supported backends); `MySQL` is unsupported.
pub(crate) fn is_serialization_failure(err: &DbErr) -> bool {
    is_retryable_contention(DbBackend::Postgres, err)
        || is_retryable_contention(DbBackend::Sqlite, err)
}

/// Returns `true` iff `err` represents a `CHECK` constraint violation on
/// either supported backend.
pub(crate) fn is_check_violation(err: &DbErr) -> bool {
    let msg = err.to_string().to_lowercase();
    msg.contains("check constraint")
        || msg.contains("check_violation")
        || msg.contains("sqlite_constraint_check")
        || contains_anchored_pg_check_sqlstate(&msg)
        || (msg.contains("sqlite") && contains_anchored_sqlite_check_code(&msg))
}

/// Returns `true` iff `err` is a unique-constraint violation.
pub(crate) fn is_unique_violation(err: &DbErr) -> bool {
    matches!(
        err.sql_err(),
        Some(sea_orm::SqlErr::UniqueConstraintViolation(_))
    )
}

fn contains_anchored_pg_check_sqlstate(msg: &str) -> bool {
    msg.contains("sqlstate 23514")
        || msg.contains("sqlstate: 23514")
        || msg.contains("sqlstate=23514")
        || msg.contains("code 23514")
        || msg.contains("code: 23514")
        || msg.contains("(23514)")
        || msg.contains("(23514:")
        || msg.starts_with("23514:")
        || msg.contains(" 23514:")
}

fn contains_anchored_sqlite_check_code(msg: &str) -> bool {
    msg.contains("code 275")
        || msg.contains("code: 275")
        || msg.contains("(275)")
        || msg.contains("(275:")
        || msg.starts_with("275:")
        || msg.contains(" 275:")
}

#[cfg(test)]
mod tests {
    use sea_orm::DbErr;

    use super::{is_check_violation, is_serialization_failure, is_unique_violation};

    fn custom(msg: &str) -> DbErr {
        DbErr::Custom(msg.to_owned())
    }

    #[test]
    fn check_violation_detected_by_message_and_codes() {
        assert!(is_check_violation(&custom(
            "CHECK constraint failed: secrets"
        )));
        assert!(is_check_violation(&custom(
            "error: check_violation on table"
        )));
        assert!(is_check_violation(&custom(
            "SQLITE_CONSTRAINT_CHECK: bad row"
        )));
        // Anchored PostgreSQL SQLSTATE 23514.
        assert!(is_check_violation(&custom("db error (SQLSTATE 23514)")));
        // Anchored SQLite extended result code 275, gated on "sqlite".
        assert!(is_check_violation(&custom("sqlite failure (275)")));
    }

    #[test]
    fn check_violation_ignores_unrelated_errors() {
        assert!(!is_check_violation(&custom("connection reset by peer")));
        // Bare 275 without the sqlite marker must not match.
        assert!(!is_check_violation(&custom("affected 275 rows")));
    }

    #[test]
    fn unique_and_serialization_predicates_reject_opaque_errors() {
        // A free-form Custom error carries no SqlErr / backend contention code.
        let err = custom("opaque");
        assert!(!is_unique_violation(&err));
        assert!(!is_serialization_failure(&err));
    }
}
