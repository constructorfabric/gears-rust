use toolkit_canonical_errors::{CanonicalError, Http, resource_error};
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::pagination::QueryError;
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

/// Errors about a subject's profile.
#[resource_error(gts_id!("cf.construct.subject.profile.v1~"))]
pub struct ProfileError;

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
#[expect(
    clippy::cognitive_complexity,
    reason = "flat match on the domain enum; the tracing macros count toward the complexity"
)]
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
        // Answered on the settings resource, not as a profile's.
        DomainError::NotVisible(msg) => {
            tracing::warn!(msg = %msg, "construct subject settings not visible");
            SubjectSettingsError::not_found("no such subject")
                .with_resource("subject")
                .create()
        }
        DomainError::NotConfigured(msg) => {
            tracing::error!(msg = %msg, "construct is not configured for this call");
            SubjectSettingsError::unimplemented("this deployment does not provide this call")
                .create()
        }
        // The settings routes refuse no record; a refusal here is mapped like
        // any other error.
        e @ (DomainError::Refused(_)
        | DomainError::Unavailable(_)
        | DomainError::NoSuchFact(_)
        | DomainError::InvalidQuery(_)
        | DomainError::Internal(_)
        | DomainError::ProfileFull(_)
        | DomainError::NoSuchReviewRequest(_)
        | DomainError::ReviewConflict(_)
        | DomainError::InvalidValue { .. }
        | DomainError::NotStored(_)
        | DomainError::Database(_)) => e.into(),
    }
}

/// Errors about a subject's facts.
#[resource_error(gts_id!("cf.construct.subject.fact.v1~"))]
pub struct SubjectFactsError;

/// A fact the subject does not have, answered like a subject that does not
/// exist.
#[must_use]
pub fn no_such_fact(subject_id: Uuid) -> CanonicalError {
    SubjectFactsError::not_found("no such fact")
        .with_resource(subject_id.to_string())
        .create()
}

/// A manage-facts field longer than its limit, as an invalid-argument problem that names the field.
#[must_use]
pub fn too_long(field: &str, limit: usize) -> CanonicalError {
    SubjectFactsError::invalid_argument()
        .with_field_violation(field, format!("at most {limit} characters"), "too_long")
        .create()
}

/// A subject's text that breaks its rule, as an invalid-argument problem
/// that names the field.
#[must_use]
pub fn bad_text(field: &str, rule: crate::domain::review_queue::TextRule) -> CanonicalError {
    SubjectFactsError::invalid_argument()
        .with_field_violation(field, rule.description(), "invalid_text")
        .create()
}

/// The REST answer for a domain error on the export. The data protection
/// officer is told a refusal as such, 403, since the export is not the
/// subject's own data; a subject without data in the tenant exports empty
/// and never reaches here.
///
/// @cpt-dod:cpt-cf-construct-dod-subject-control-export:p1
#[must_use]
pub fn for_export(e: DomainError) -> CanonicalError {
    match e {
        DomainError::Forbidden(msg) => {
            tracing::warn!(msg = %msg, "construct subject export refused");
            SubjectFactsError::permission_denied()
                .with_reason("ACCESS_DENIED")
                .create()
        }
        DomainError::OutOfScope(refused) => {
            log_out_of_scope(&refused);
            SubjectFactsError::permission_denied()
                .with_reason("ACCESS_DENIED")
                .create()
        }
        e @ (DomainError::Refused(_)
        | DomainError::Unavailable(_)
        | DomainError::NotConfigured(_)
        | DomainError::NoSuchFact(_)
        | DomainError::InvalidQuery(_)
        | DomainError::Internal(_)
        | DomainError::ProfileFull(_)
        | DomainError::NotVisible(_)
        | DomainError::NoSuchReviewRequest(_)
        | DomainError::ReviewConflict(_)
        | DomainError::InvalidValue { .. }
        | DomainError::NotStored(_)
        | DomainError::Database(_)) => e.into(),
    }
}

/// The REST answer for a domain error on the manage-facts route. A subject the caller may not act for, or whose
/// settings the caller's policy scope leaves out, answers as a subject that does not exist, so the answer reveals
/// nothing about it. The gear's own failures are mapped as everywhere else.
///
/// @cpt-dod:cpt-cf-construct-dod-planner-manage-route:p1
#[must_use]
#[expect(
    clippy::cognitive_complexity,
    reason = "flat match on the domain enum; the tracing macros count toward the complexity"
)]
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
        // A fact the subject does not have answers like a subject that
        // does not exist.
        DomainError::NoSuchFact(msg) => {
            tracing::warn!(msg = %msg, "construct subject fact not found");
            no_such_fact(subject_id)
        }
        // Answered on the subject facts resource, not as a profile's.
        DomainError::NotVisible(msg) => {
            tracing::warn!(msg = %msg, "construct subject facts not visible");
            SubjectFactsError::not_found("no such subject")
                .with_resource(subject_id.to_string())
                .create()
        }
        DomainError::NotConfigured(msg) => {
            tracing::error!(msg = %msg, "construct is not configured for this call");
            SubjectFactsError::unimplemented("this deployment does not provide this call").create()
        }
        e @ (DomainError::Refused(_)
        | DomainError::Unavailable(_)
        | DomainError::InvalidQuery(_)
        | DomainError::Internal(_)
        | DomainError::ProfileFull(_)
        | DomainError::NoSuchReviewRequest(_)
        | DomainError::ReviewConflict(_)
        | DomainError::InvalidValue { .. }
        | DomainError::NotStored(_)
        | DomainError::Database(_)) => e.into(),
    }
}

/// Errors about review requests.
#[resource_error(gts_id!("cf.construct.review.request.v1~"))]
pub struct ReviewRequestError;

/// A resolution whose body does not fit its outcome, as an invalid-argument
/// problem that names the field.
#[must_use]
pub fn bad_resolution(mistake: &crate::api::rest::dto::BodyMistake) -> CanonicalError {
    ReviewRequestError::invalid_argument()
        .with_field_violation(mistake.field, mistake.rule, "invalid_resolution")
        .create()
}

/// A correction the checks or Admission refused, logged here, once. On the
/// REST route (`rest`) it is `422`.
fn not_stored(reason: crate::domain::review_queue::NotStored, rest: bool) -> CanonicalError {
    tracing::info!(reason = reason.name(), "construct correction not stored");
    let error = ReviewRequestError::failed_precondition().with_precondition_violation(
        "value",
        "the correction was not stored; the review request stays open",
        reason.name(),
    );
    if rest {
        error.with_override(Http::status_code(422)).create()
    } else {
        error.create()
    }
}

/// The REST answer for a domain error on the reviewers' routes. A reviewer is
/// told a refusal as such, 403: the routes are about the whole tenant, not one
/// subject.
///
/// @cpt-dod:cpt-cf-construct-dod-review-queue-reviewer:p1
#[must_use]
pub fn for_reviews(e: DomainError) -> CanonicalError {
    match e {
        DomainError::Forbidden(msg) => {
            tracing::warn!(msg = %msg, "construct review requests access refused");
            ReviewRequestError::permission_denied()
                .with_reason("ACCESS_DENIED")
                .create()
        }
        DomainError::NotConfigured(msg) => {
            tracing::error!(msg = %msg, "construct is not configured for this call");
            ReviewRequestError::unimplemented("this deployment does not provide this call").create()
        }
        // A refused correction is 422, a status only the REST route carries.
        DomainError::NotStored(reason) => not_stored(reason, true),
        // A query of the reviewers' list names the review request resource.
        DomainError::InvalidQuery(err) => {
            let (field, detail, reason) = query_violation(err);
            ReviewRequestError::invalid_argument()
                .with_field_violation(field, detail, reason)
                .create()
        }
        e @ (DomainError::Refused(_)
        | DomainError::OutOfScope(_)
        | DomainError::Unavailable(_)
        | DomainError::NotVisible(_)
        | DomainError::NoSuchFact(_)
        | DomainError::NoSuchReviewRequest(_)
        | DomainError::ReviewConflict(_)
        | DomainError::InvalidValue { .. }
        | DomainError::Internal(_)
        | DomainError::ProfileFull(_)
        | DomainError::Database(_)) => e.into(),
    }
}

/// The field, detail and reason of a query a list does not take, so a
/// client can tell a bad filter, order, cursor and select apart.
fn query_violation(err: QueryError) -> (&'static str, String, &'static str) {
    match err {
        QueryError::Filter(detail) => ("$filter", detail, "invalid_filter"),
        QueryError::Order(detail) => ("$orderby", detail, "invalid_order"),
        QueryError::Cursor(detail) => ("cursor", detail, "invalid_cursor"),
        QueryError::Select(detail) => ("$select", detail, "unsupported_select"),
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
            // @cpt-begin:cpt-cf-construct-algo-profile-reader-read:p1:inst-read-hide
            // A subject the caller may not see is answered like one that does
            // not exist, on every transport.
            DomainError::NotVisible(msg) => {
                tracing::warn!(msg = %msg, "construct profile read refused");
                ProfileError::not_found("no such subject")
                    .with_resource("subject")
                    .create()
            }
            // @cpt-end:cpt-cf-construct-algo-profile-reader-read:p1:inst-read-hide
            // @cpt-begin:cpt-cf-construct-algo-gear-foundation-map-errors:p1:inst-map-unavailable
            // @cpt-begin:cpt-cf-construct-algo-gear-foundation-map-errors:p1:inst-map-unavailable-return
            DomainError::Unavailable(msg) => {
                tracing::warn!(msg = %msg, "construct dependency unavailable");
                CanonicalError::service_unavailable().create()
            }
            // @cpt-end:cpt-cf-construct-algo-gear-foundation-map-errors:p1:inst-map-unavailable-return
            // @cpt-end:cpt-cf-construct-algo-gear-foundation-map-errors:p1:inst-map-unavailable
            // A deployment without what the call needs answers 501, which a
            // caller does not retry, and the operator sees an error.
            DomainError::NotConfigured(msg) => {
                tracing::error!(msg = %msg, "construct is not configured for this call");
                ProfileError::unimplemented("this deployment does not provide this call").create()
            }
            DomainError::NoSuchFact(msg) => {
                tracing::warn!(msg = %msg, "construct subject fact not found");
                SubjectFactsError::not_found("no such fact")
                    .with_resource("fact")
                    .create()
            }
            DomainError::NoSuchReviewRequest(msg) => {
                tracing::warn!(msg = %msg, "construct review request not found");
                ReviewRequestError::not_found("no such review request")
                    .with_resource("review_request")
                    .create()
            }
            // A second open request on a fact, a request already closed, or a
            // resolution that was written but not closed: the message names
            // the request and fact ids, and a correction's new fact id.
            DomainError::ReviewConflict(conflict) => {
                tracing::info!(%conflict, "construct review request conflict");
                ReviewRequestError::aborted(conflict.to_string())
                    .with_reason(conflict.reason())
                    .create()
            }
            DomainError::InvalidValue { field, rule } => {
                tracing::info!(field, rule = %rule, "construct correction value refused");
                ReviewRequestError::invalid_argument()
                    .with_field_violation(field, rule, "invalid_value")
                    .create()
            }
            // The correction is refused by the checks or Admission; the
            // request stays open, and the reason says why.
            DomainError::NotStored(reason) => not_stored(reason, false),
            // The caller's query does not fit the list; the field violation
            // says which part, so a client can tell them apart.
            DomainError::InvalidQuery(err) => {
                let (field, detail, reason) = query_violation(err);
                SubjectFactsError::invalid_argument()
                    .with_field_violation(field, detail, reason)
                    .create()
            }
            // A full profile takes no new fact until facts are removed: the
            // caller's state, not the gear's failure. The subject stays in the
            // log, out of the answer.
            DomainError::ProfileFull(full) => {
                tracing::warn!(
                    tenant_id = %full.subject.tenant_id,
                    subject_id = %full.subject.subject_id,
                    facts = %full.facts,
                    limit = full.limit,
                    "construct profile full"
                );
                ProfileError::failed_precondition()
                    .with_precondition_violation(
                        "profile",
                        "the profile holds the most facts it may; remove facts to add new ones",
                        "PROFILE_FULL",
                    )
                    .create()
            }
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
