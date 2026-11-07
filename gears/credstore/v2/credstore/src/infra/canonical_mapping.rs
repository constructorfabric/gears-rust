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

/// The database is unreachable: no connection could be acquired, or the
/// connection broke (typed `DbErr` signals only, never driver text).
fn is_db_unavailable(err: &DbErr) -> bool {
    matches!(err, DbErr::ConnectionAcquire(_) | DbErr::Conn(_))
}

/// The class of a [`DbErr`] as a fixed name. Logged instead of the error
/// text: like in toolkit-db, the driver text can carry column values; the
/// full error stays in the `cause`.
fn db_err_kind(err: &DbErr) -> &'static str {
    match err {
        DbErr::ConnectionAcquire(_) => "connection_acquire",
        DbErr::Conn(_) => "conn",
        DbErr::Exec(_) => "exec",
        DbErr::Query(_) => "query",
        DbErr::RecordNotInserted => "record_not_inserted",
        DbErr::RecordNotUpdated => "record_not_updated",
        DbErr::RecordNotFound(_) => "record_not_found",
        DbErr::Custom(_) => "custom",
        DbErr::Type(_) => "type",
        DbErr::Json(_) => "json",
        DbErr::Migration(_) => "migration",
        _ => "other",
    }
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
    // DESIGN requires 503 when `PostgreSQL` is unavailable. The `DbErr` stays
    // as the cause; the transaction retry only looks at
    // `is_retryable_contention`, so it does not retry on this.
    if is_db_unavailable(&db_err) {
        return DomainError::ServiceUnavailable {
            detail: "database unavailable".to_owned(),
            retry_after: None,
            cause: Some(Box::new(db_err)),
        };
    }
    // Every CHECK in the schema guards data the code validates or produces
    // before the write, so a firing CHECK is a broken server-side invariant:
    // an internal error like any other unexpected database failure.
    warn!(target: "credstore.db", kind = db_err_kind(&db_err), "database error");
    DomainError::Internal {
        diagnostic: "database error".to_owned(),
        cause: Some(Box::new(db_err)),
    }
}

impl From<DbError> for DomainError {
    fn from(err: DbError) -> Self {
        match err {
            DbError::Sea(db) => classify_db_err_to_domain(db),
            // A system-level IO failure (socket reset and the like): the
            // database is unavailable, as for `DbErr::Conn`.
            io @ DbError::Io(_) => Self::ServiceUnavailable {
                detail: "database unavailable".to_owned(),
                retry_after: None,
                cause: Some(Box::new(io)),
            },
            other => Self::Internal {
                diagnostic: "database error".to_owned(),
                cause: Some(Box::new(other)),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use sea_orm::{ConnAcquireErr, DbErr, RuntimeErr};
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
            classify_db_err_to_domain(custom("unexpected")),
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

    #[test]
    fn classify_maps_connection_acquire_to_service_unavailable() {
        assert!(matches!(
            classify_db_err_to_domain(DbErr::ConnectionAcquire(ConnAcquireErr::Timeout)),
            DomainError::ServiceUnavailable { cause: Some(_), .. }
        ));
    }

    #[test]
    fn classify_maps_conn_to_service_unavailable() {
        assert!(matches!(
            classify_db_err_to_domain(DbErr::Conn(RuntimeErr::Internal("down".to_owned()))),
            DomainError::ServiceUnavailable { cause: Some(_), .. }
        ));
    }

    #[test]
    fn from_dberr_io_is_service_unavailable() {
        let io = std::io::Error::new(std::io::ErrorKind::ConnectionReset, "reset");
        let mapped: DomainError = DbError::Io(io).into();
        assert!(matches!(
            mapped,
            DomainError::ServiceUnavailable { cause: Some(_), .. }
        ));
    }

    #[test]
    fn classify_maps_serialization_failure_to_service_unavailable() {
        // SQLSTATE 40001 as `PostgreSQL` reports it.
        let err = DbErr::Exec(RuntimeErr::Internal(
            "error returned from database: error with SQLSTATE 40001".to_owned(),
        ));
        assert!(matches!(
            classify_db_err_to_domain(err),
            DomainError::ServiceUnavailable { cause: Some(_), .. }
        ));
    }
}
