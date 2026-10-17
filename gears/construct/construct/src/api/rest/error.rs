use toolkit_canonical_errors::{CanonicalError, resource_error};

use crate::domain::error::DomainError;

#[resource_error(gts_id!("cf.construct.foundation.note.v1~"))]
pub struct FoundationNoteError;

/// Errors about a subject's settings.
#[resource_error(gts_id!("cf.construct.subject.settings.v1~"))]
pub struct SubjectSettingsError;

/// The REST answer for a domain error on the subject's settings routes. The
/// errors about the request itself name the settings resource; the gear's own
/// failures are mapped as everywhere else. Every variant is listed, so a new
/// one has to be placed here.
///
/// @cpt-dod:cpt-cf-construct-dod-subject-settings-routes:p1
#[must_use]
pub fn for_settings(e: DomainError) -> CanonicalError {
    match e {
        // @cpt-begin:cpt-cf-construct-algo-subject-settings-owner-only:p1:inst-owner-refuse-return
        DomainError::Forbidden(msg) => {
            tracing::warn!(msg = %msg, "construct subject settings access forbidden");
            SubjectSettingsError::permission_denied()
                .with_reason("ACCESS_DENIED")
                .create()
        }
        // @cpt-end:cpt-cf-construct-algo-subject-settings-owner-only:p1:inst-owner-refuse-return
        DomainError::NotFound => SubjectSettingsError::not_found("Subject settings not found")
            .with_resource("subject_settings")
            .create(),
        DomainError::Validation { field, message } => SubjectSettingsError::invalid_argument()
            .with_field_violation(field, message, "VALIDATION_ERROR")
            .create(),
        e @ (DomainError::Unavailable(_) | DomainError::Internal(_) | DomainError::Database(_)) => {
            e.into()
        }
    }
}

/// @cpt-dod:cpt-cf-construct-dod-gear-foundation-error-mapping:p1
impl From<DomainError> for CanonicalError {
    #[expect(
        clippy::cognitive_complexity,
        reason = "flat match on the domain enum; the tracing macros count toward the complexity"
    )]
    fn from(e: DomainError) -> Self {
        match e {
            // @cpt-begin:cpt-cf-construct-algo-gear-foundation-map-errors:p1:inst-map-not-found
            // @cpt-begin:cpt-cf-construct-algo-gear-foundation-map-errors:p1:inst-map-not-found-return
            DomainError::NotFound => FoundationNoteError::not_found("Note not found")
                .with_resource("note")
                .create(),
            // @cpt-end:cpt-cf-construct-algo-gear-foundation-map-errors:p1:inst-map-not-found-return
            // @cpt-end:cpt-cf-construct-algo-gear-foundation-map-errors:p1:inst-map-not-found
            // @cpt-begin:cpt-cf-construct-algo-gear-foundation-map-errors:p1:inst-map-validation
            // @cpt-begin:cpt-cf-construct-algo-gear-foundation-map-errors:p1:inst-map-validation-return
            DomainError::Validation { field, message } => FoundationNoteError::invalid_argument()
                .with_field_violation(field, message, "VALIDATION_ERROR")
                .create(),
            // @cpt-end:cpt-cf-construct-algo-gear-foundation-map-errors:p1:inst-map-validation-return
            // @cpt-end:cpt-cf-construct-algo-gear-foundation-map-errors:p1:inst-map-validation
            // @cpt-begin:cpt-cf-construct-algo-gear-foundation-map-errors:p1:inst-map-forbidden
            // @cpt-begin:cpt-cf-construct-algo-gear-foundation-map-errors:p1:inst-map-forbidden-return
            DomainError::Forbidden(msg) => {
                tracing::warn!(msg = %msg, "construct access forbidden");
                FoundationNoteError::permission_denied()
                    .with_reason("ACCESS_DENIED")
                    .create()
            }
            // @cpt-end:cpt-cf-construct-algo-gear-foundation-map-errors:p1:inst-map-forbidden-return
            // @cpt-end:cpt-cf-construct-algo-gear-foundation-map-errors:p1:inst-map-forbidden
            // @cpt-begin:cpt-cf-construct-algo-gear-foundation-map-errors:p1:inst-map-unavailable
            // @cpt-begin:cpt-cf-construct-algo-gear-foundation-map-errors:p1:inst-map-unavailable-return
            DomainError::Unavailable(msg) => {
                tracing::warn!(msg = %msg, "construct dependency unavailable");
                CanonicalError::service_unavailable().create()
            }
            // @cpt-end:cpt-cf-construct-algo-gear-foundation-map-errors:p1:inst-map-unavailable-return
            // @cpt-end:cpt-cf-construct-algo-gear-foundation-map-errors:p1:inst-map-unavailable
            // @cpt-begin:cpt-cf-construct-algo-gear-foundation-map-errors:p1:inst-map-internal
            // @cpt-begin:cpt-cf-construct-algo-gear-foundation-map-errors:p1:inst-map-internal-return
            DomainError::Internal(msg) => {
                tracing::error!(msg = %msg, "construct internal error");
                CanonicalError::internal(msg).create()
            }
            DomainError::Database(db_err) => {
                tracing::error!(error = ?db_err, "construct database error");
                CanonicalError::internal(db_err.to_string()).create()
            }
        }
        // @cpt-end:cpt-cf-construct-algo-gear-foundation-map-errors:p1:inst-map-internal-return
        // @cpt-end:cpt-cf-construct-algo-gear-foundation-map-errors:p1:inst-map-internal
    }
}
