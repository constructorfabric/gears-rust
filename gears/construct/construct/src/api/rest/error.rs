use toolkit_canonical_errors::{CanonicalError, Http, resource_error};

use crate::domain::error::DomainError;
use crate::domain::record_intake::Refusal;

/// Errors about connector records. The resource type is the record base type.
#[resource_error(gts_id!("cf.connectors.core.record.v1~"))]
pub struct RecordError;

/// A refused record as an invalid-argument problem. The field violation names
/// the place in the record, the broken rule and the reason code; the resource
/// names the record's type. The refusal is logged here, once, at info level:
/// it is the connector's error, not the gear's.
/// On the REST route (`rest`) a refusal is `422`.
fn refused(refusal: Refusal, rest: bool) -> CanonicalError {
    // @cpt-begin:cpt-cf-construct-flow-record-intake-submit:p1:inst-submit-refused
    let place = refusal.place.to_string();
    let type_id = refusal.type_id.unwrap_or_else(|| "record".to_owned());
    tracing::info!(
        reason = refusal.reason.code(),
        record_type = %type_id,
        place = %place,
        rule = %refusal.rule,
        "record refused"
    );
    let error = RecordError::invalid_argument()
        .with_field_violation(place, refusal.rule, refusal.reason.code())
        .with_resource(type_id);
    if rest {
        error.with_override(Http::status_code(422)).create()
    } else {
        error.create()
    }
    // @cpt-end:cpt-cf-construct-flow-record-intake-submit:p1:inst-submit-refused
}

/// The REST answer for a domain error. A refused record is `422`, a status
/// only the REST route carries; the shared conversion below stays
/// transport-agnostic for the in-process client.
///
/// @cpt-dod:cpt-cf-construct-dod-record-intake-route:p1
#[must_use]
pub fn for_rest(e: DomainError) -> CanonicalError {
    match e {
        DomainError::Refused(refusal) => refused(refusal, true),
        other => other.into(),
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
            DomainError::Refused(refusal) => refused(refusal, false),
            // @cpt-begin:cpt-cf-construct-algo-gear-foundation-map-errors:p1:inst-map-forbidden
            // @cpt-begin:cpt-cf-construct-algo-gear-foundation-map-errors:p1:inst-map-forbidden-return
            DomainError::Forbidden(msg) => {
                tracing::warn!(msg = %msg, "construct access forbidden");
                RecordError::permission_denied()
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
