//! How a `DomainError` reaches the caller: the HTTP-facing mapping lives in
//! the API layer, so `domain/` stays free of status codes and transport
//! vocabulary.

use toolkit_canonical_errors::{CanonicalError, resource_error};

use crate::domain::error::DomainError;

#[resource_error(gts_id!("cf.core.github_mirror.repository.v1~"))]
pub struct RepositoryError;

/// What a caller is told about an internal failure. The cause is logged, not
/// returned: the messages name upstream GitHub paths and storage internals.
const INTERNAL_DETAIL: &str = "The mirror could not complete this request";

impl From<DomainError> for CanonicalError {
    // Flat match on the domain enum is the whole point of this conversion;
    // the structured `tracing::*!` macros count toward cognitive complexity
    // but splitting the arms into helpers would just hide the mapping.
    #[allow(clippy::cognitive_complexity)]
    fn from(e: DomainError) -> Self {
        match e {
            DomainError::NotFound => RepositoryError::not_found("Repo not found")
                .with_resource("repository")
                .create(),
            DomainError::Validation { field, message } => RepositoryError::invalid_argument()
                .with_field_violation(field, message, "VALIDATION_ERROR")
                .create(),
            DomainError::AccessLost(msg) => {
                tracing::warn!(msg = %msg, "github-mirror upstream access lost");
                RepositoryError::not_found("Repo not found or not accessible")
                    .with_resource("repository")
                    .create()
            }
            DomainError::Conflict(msg) => RepositoryError::already_exists(msg)
                .with_resource("repository")
                .create(),
            DomainError::Forbidden(msg) => {
                tracing::warn!(msg = %msg, "github-mirror access forbidden");
                RepositoryError::not_found("Repo not found or not accessible")
                    .with_resource("repository")
                    .create()
            }
            // Both arms keep the detail in the log and hand the caller a
            // fixed message: an internal failure's text names upstream
            // GitHub paths and storage internals, and a repository that is
            // private to one tenant should not be inferable from another
            // tenant's error body.
            DomainError::Internal(msg) => {
                tracing::error!(msg = %msg, "github-mirror internal error");
                CanonicalError::internal(INTERNAL_DETAIL).create()
            }
            DomainError::Database(db_err) => {
                tracing::error!(error = ?db_err, "github-mirror database error");
                CanonicalError::internal(INTERNAL_DETAIL).create()
            }
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn status_of(e: DomainError) -> u16 {
        CanonicalError::from(e).status_code()
    }

    #[test]
    fn every_domain_error_maps_to_its_canonical_status() {
        assert_eq!(status_of(DomainError::NotFound), 404);
        assert_eq!(
            status_of(DomainError::Validation {
                field: "owner".to_owned(),
                message: "empty".to_owned(),
            }),
            400
        );
        // Both "caller lacks rights" and "the mirror's own upstream access
        // is gone" must read as 404: a 403 would confirm the repo exists.
        assert_eq!(status_of(DomainError::forbidden("no scope")), 404);
        assert_eq!(
            status_of(DomainError::AccessLost("token revoked".to_owned())),
            404
        );
        assert_eq!(
            status_of(DomainError::Conflict("sync already running".to_owned())),
            409
        );
        assert_eq!(status_of(DomainError::internal("boom")), 500);
        assert_eq!(
            status_of(DomainError::Database(toolkit_db::DbError::InvalidConfig(
                "bad dsn".to_owned()
            ))),
            500
        );
    }
}
