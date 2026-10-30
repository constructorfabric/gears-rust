// Updated: 2026-10-06 by Constructor Tech
//! `DbErr` → [`DomainError`] classification ladder.

use sea_orm::{DbBackend, DbErr};
use toolkit_db::DbError;
use toolkit_db::contention::is_retryable_contention;
use toolkit_db::secure::is_unique_violation;
use tracing::warn;

use crate::domain::error::DomainError;

/// A definite rollback the transaction retry may run again: a `PostgreSQL`
/// serialization failure or deadlock, or a busy `SQLite` (the two supported
/// backends).
fn is_serialization_failure(err: &DbErr) -> bool {
    is_retryable_contention(DbBackend::Postgres, err)
        || is_retryable_contention(DbBackend::Sqlite, err)
}

/// Classify a raw [`DbErr`] into a typed [`DomainError`].
pub(super) fn classify_db_err_to_domain(db_err: DbErr) -> DomainError {
    if is_unique_violation(&db_err) {
        return DomainError::Conflict;
    }
    if is_serialization_failure(&db_err) {
        // The `DbErr` stays as the cause: the transaction retry
        // (`SecretRepoImpl::run_tx`) reads it back to decide whether to run
        // the body again.
        return DomainError::ServiceUnavailable {
            detail: "serialization conflict".to_owned(),
            retry_after: None,
            cause: Some(Box::new(db_err)),
        };
    }
    // Every CHECK in the schema guards data the code validates or produces
    // before the write, so a firing CHECK is a broken server-side invariant:
    // an internal error like any other unexpected database failure.
    warn!(target: "credstore.db", error = %db_err, "database error");
    DomainError::Internal {
        diagnostic: "database error".to_owned(),
        cause: Some(Box::new(db_err)),
    }
}

impl From<DbError> for DomainError {
    fn from(err: DbError) -> Self {
        match err {
            DbError::Sea(db) => classify_db_err_to_domain(db),
            other => Self::Internal {
                diagnostic: "database error".to_owned(),
                cause: Some(Box::new(other)),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use sea_orm::DbErr;
    use toolkit_db::DbError;

    use super::classify_db_err_to_domain;
    use crate::domain::error::DomainError;

    fn custom(msg: &str) -> DbErr {
        DbErr::Custom(msg.to_owned())
    }

    #[test]
    fn classify_maps_check_violation_to_internal() {
        assert!(matches!(
            classify_db_err_to_domain(custom("CHECK constraint failed")),
            DomainError::Internal { cause: Some(_), .. }
        ));
    }

    #[test]
    fn classify_maps_unclassified_to_internal() {
        assert!(matches!(
            classify_db_err_to_domain(custom("connection refused")),
            DomainError::Internal { .. }
        ));
    }

    #[test]
    fn from_dberr_sea_delegates_to_classifier() {
        let mapped: DomainError = DbError::Sea(custom("CHECK constraint failed")).into();
        assert!(matches!(
            mapped,
            DomainError::Internal { cause: Some(_), .. }
        ));
    }

    #[test]
    fn from_dberr_non_sea_is_internal_with_cause() {
        let mapped: DomainError = DbError::InvalidConfig("bad dsn".to_owned()).into();
        assert!(matches!(
            mapped,
            DomainError::Internal { cause: Some(_), .. }
        ));
    }

    #[test]
    fn opaque_error_is_neither_unique_nor_serialization() {
        let err = custom("opaque");
        assert!(!super::is_serialization_failure(&err));
        assert!(matches!(
            classify_db_err_to_domain(err),
            DomainError::Internal { .. }
        ));
    }

    #[test]
    fn classify_maps_unique_violation_to_conflict() {
        // `toolkit_db`'s check also reads the message when the driver gave no
        // SQLSTATE, as a wrapped error does.
        assert!(matches!(
            classify_db_err_to_domain(custom(
                "duplicate key value violates unique constraint \"credstore_secrets_pkey\""
            )),
            DomainError::Conflict
        ));
    }
}
