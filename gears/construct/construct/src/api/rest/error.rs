use toolkit_canonical_errors::{CanonicalError, Http, resource_error};
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::record_intake::Refusal;
use crate::domain::settings_scope::SettingsOutOfScope;

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

/// Logs a subject settings read or write the caller's policy scope leaves out,
/// with its ids as fields and an event of its own, so it is not taken for a
/// policy denial. The ids stay out of the answer.
fn log_out_of_scope(refused: &SettingsOutOfScope) {
    tracing::warn!(
        caller = %refused.caller,
        caller_tenant = %refused.caller_tenant,
        tenant_id = %refused.tenant_id,
        subject_id = %refused.subject_id,
        refusal = ?refused.refusal,
        "construct subject settings out of the policy scope"
    );
}

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
        // A read the policy scope leaves out is refused the same way.
        DomainError::OutOfScope(refused) => {
            log_out_of_scope(&refused);
            SubjectSettingsError::permission_denied()
                .with_reason("ACCESS_DENIED")
                .create()
        }
        // The settings routes refuse no record; a refusal here is mapped like
        // any other error.
        e @ (DomainError::Refused(_)
        | DomainError::Unavailable(_)
        | DomainError::Internal(_)
        | DomainError::Database(_)) => e.into(),
    }
}

/// Errors about a subject's facts.
#[resource_error(gts_id!("cf.construct.subject.facts.v1~"))]
pub struct SubjectFactsError;

/// A manage-facts field longer than its limit, as an invalid-argument problem that names the field.
#[must_use]
pub fn too_long(field: &str, limit: usize) -> CanonicalError {
    SubjectFactsError::invalid_argument()
        .with_field_violation(field, format!("at most {limit} characters"), "too_long")
        .create()
}

/// The REST answer for a domain error on the manage-facts route. A subject the caller may not act for, or whose
/// settings the caller's policy scope leaves out, answers as a subject that does not exist, so the answer reveals
/// nothing about it. The gear's own failures are mapped as everywhere else.
///
/// @cpt-dod:cpt-cf-construct-dod-planner-manage-route:p1
#[must_use]
pub fn for_facts(e: DomainError, subject_id: Uuid) -> CanonicalError {
    match e {
        DomainError::Forbidden(msg) => {
            tracing::warn!(msg = %msg, "construct subject facts access refused");
            SubjectFactsError::not_found("no such subject")
                .with_resource(subject_id.to_string())
                .create()
        }
        DomainError::OutOfScope(refused) => {
            log_out_of_scope(&refused);
            SubjectFactsError::not_found("no such subject")
                .with_resource(subject_id.to_string())
                .create()
        }
        e @ (DomainError::Refused(_)
        | DomainError::Unavailable(_)
        | DomainError::Internal(_)
        | DomainError::Database(_)) => e.into(),
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
            DomainError::OutOfScope(refused) => {
                log_out_of_scope(&refused);
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
