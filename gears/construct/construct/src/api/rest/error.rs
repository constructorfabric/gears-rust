use toolkit_canonical_errors::{CanonicalError, resource_error};

use crate::domain::error::DomainError;

#[resource_error(gts_id!("cf.construct.foundation.note.v1~"))]
pub struct FoundationNoteError;

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
