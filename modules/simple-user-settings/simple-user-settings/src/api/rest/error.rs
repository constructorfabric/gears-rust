use modkit::api::canonical_prelude::CanonicalProblemMigrationExt;
use modkit_canonical_errors::{CanonicalError, Problem, resource_error};

use crate::domain::error::DomainError;

#[resource_error("gts.cf.simple_user_settings.settings.user.v1~")]
pub struct UserSettingsError;

impl From<DomainError> for CanonicalError {
    // Flat match on the domain enum is the whole point of this conversion;
    // the structured `tracing::*!` macros count toward cognitive complexity
    // but splitting the arms into helpers would just hide the mapping.
    #[allow(clippy::cognitive_complexity)]
    fn from(e: DomainError) -> Self {
        // The settings resource is keyed off the caller's identity, so the
        // resource_name is always the literal "self" — i.e. the authenticated
        // user's own settings record.
        match e {
            DomainError::NotFound => UserSettingsError::not_found("Settings not found")
                .with_resource("self")
                .create(),
            DomainError::Validation { field, message } => UserSettingsError::invalid_argument()
                .with_field_violation(field, message, "VALIDATION_ERROR")
                .create(),
            DomainError::Forbidden(msg) => {
                tracing::warn!(msg = %msg, "simple-user-settings access forbidden");
                // Mask as not_found so the response does not disclose that the
                // resource exists but is out of scope for the caller.
                UserSettingsError::not_found("Settings not found or not accessible")
                    .with_resource("self")
                    .create()
            }
            DomainError::Internal(msg) => {
                tracing::error!(msg = %msg, "simple-user-settings internal error");
                CanonicalError::internal(msg).create()
            }
            DomainError::Database(db_err) => {
                tracing::error!(error = ?db_err, "simple-user-settings database error");
                CanonicalError::internal(db_err.to_string()).create()
            }
        }
    }
}

// TODO(cpt-cf-errors-component-error-middleware): drop this impl once
// middleware injects trace_id/instance from request context. The
// `From<DomainError> for CanonicalError` impl above is the long-lived
// mapping; this wrapper exists only to keep handler signatures returning
// `Problem` until middleware lands.
impl From<DomainError> for Problem {
    fn from(e: DomainError) -> Self {
        Problem::from(CanonicalError::from(e)).with_temporary_request_context("/")
    }
}
