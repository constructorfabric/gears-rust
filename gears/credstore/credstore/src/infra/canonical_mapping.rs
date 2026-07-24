//! `DbErr` → [`DomainError`] classification ladder.

use sea_orm::DbErr;
use toolkit_db::DbError;
use tracing::warn;

use crate::domain::error::DomainError;
use crate::infra::error_conv::{is_check_violation, is_serialization_failure, is_unique_violation};

/// Classify a raw [`DbErr`] into a typed [`DomainError`].
#[allow(clippy::needless_pass_by_value)]
pub(crate) fn classify_db_err_to_domain(db_err: DbErr) -> DomainError {
    if is_unique_violation(&db_err) {
        return DomainError::Conflict;
    }
    if is_serialization_failure(&db_err) {
        warn!(
            target: "credstore.db",
            error = %db_err,
            "serialization conflict (retry-exhausted)"
        );
        return DomainError::ServiceUnavailable {
            detail: "serialization conflict; retry budget exhausted".to_owned(),
            retry_after: None,
            cause: None,
        };
    }
    // Every CHECK in the schema (reference length, sharing/status domains, the
    // fingerprint fence pairing) guards data the code validates or produces
    // before the write — `reference` is rejected at both the SDK and REST
    // boundaries, the rest never comes from user input. A firing CHECK is
    // therefore a broken server-side invariant, not a bad secret reference:
    // it maps to `Internal` like any other unexpected DB failure.
    let diagnostic = if is_check_violation(&db_err) {
        "database CHECK constraint violated"
    } else {
        "database error"
    };
    warn!(target: "credstore.db", error = %db_err, "{diagnostic}");
    DomainError::Internal {
        diagnostic: diagnostic.to_owned(),
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
}
