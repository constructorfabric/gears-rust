//! @cpt-dod:cpt-cf-bss-products-dod-quota-catalog-review:p1
//! The review endpoints of a candidate (P-D-269): `submit` proposes a staged candidate for one SKU
//! version under the `submit` grant; `approve` publishes its definition version and `reject`
//! closes it with a note, both under the `approve` grant and never by the candidate's submitter or
//! a manual record's author.
//!
//! Each authorizes before it reads the body, takes the candidate's `If-Match`, and answers a keyed
//! retry with its first answer before anything is resolved (P-D-198). Every registry read happens
//! before the write's transaction, which moves the candidate only from the version the caller read
//! and records the audit row and the receipt; a miss is 409 `QUOTA_CANDIDATE_STATE` when another
//! write moved it to another state, else 409 `STALE_REVISION`.
use std::sync::Arc;

use authz_resolver_sdk::PolicyEnforcer;
use axum::extract::{Path, rejection::JsonRejection};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use axum::{Extension, Json, Router};
use bss_products_sdk::quota::catalog::QuotaVersion;
use bss_products_sdk::quota::digest::content_digest;
use bss_products_sdk::quota::gts::{
    QuotaAccountingPolicyV1, QuotaCapabilityV1, QuotaMetricV1, QuotaScopePolicyV1, QuotaUnitV1,
    UNIT_TYPE,
};
use serde::de::DeserializeOwned;
use toolkit::api::OpenApiRegistry;
use toolkit::api::canonical_prelude::CanonicalError;
use toolkit::api::operation_builder::OperationBuilder;
use toolkit_db::secure::{AccessScope, DBRunner, TxConfig};
use toolkit_security::SecurityContext;
use uuid::Uuid;

use super::definitions::project;
use super::dto::{
    ProductsQuotaApproval, ProductsQuotaCandidate, ProductsQuotaDecisionRequest,
    ProductsQuotaDefinition, ProductsQuotaSubmitRequest,
};
use super::targets::TAG;
use super::{CANDIDATES, On, QuotaState, candidate_dto, registry_unavailable, review_scopes};
use crate::api::rest::{TxError, contention_db_err, preconditions, replay, require_authenticated};
use crate::authz::actions;
use crate::domain::concurrency::InternalRevision;
use crate::domain::error::DomainError;
use crate::domain::quota::definition::build;
use crate::domain::quota::evidence::{Evidence, EvidenceSet};
use crate::domain::quota::locale::MAX_TEXT_CHARS;
use crate::domain::quota::record::{
    QUOTA_RECORD_INVALID, QUOTA_REFERENCE_FAMILY, QUOTA_REFERENCE_UNKNOWN, QUOTA_UNIT_MISMATCH,
    RecordRefusal, major_only,
};
use crate::domain::quota::review::{
    AllowedUnit, Pinned, PinnedEntity, Proposal, check_profile, check_proposal_shape,
    convert_bounds, parse_basis, proposal_json,
};
use crate::domain::quota::{
    CandidateOrigin, CandidateState, DefinitionVersion, QuotaCandidate, ReviewWrite,
};
use crate::domain::validation::ValidationReport;
use crate::infra::idempotency::IdempotencyClaimInput;
use crate::infra::storage::repo::quota_catalog_repo as catalog;
use crate::infra::storage::repo::quota_intake_repo as store;
use crate::infra::storage::repo::{self, AuditCommon, LifecycleMove, version_repo};
use crate::infra::storage::{RepoError, RepoRefusal, stored_now};

/// The audit action of a submit.
pub const ACTION_SUBMIT: &str = "quota_candidate.submit";
/// The audit action of an approval.
pub const ACTION_APPROVE: &str = "quota_candidate.approve";
/// The audit action of a rejection.
pub const ACTION_REJECT: &str = "quota_candidate.reject";
/// The decider submitted the candidate or authored its manual record.
pub const SOD_VIOLATION: &str = "SOD_VIOLATION";
/// A rejection without a note.
pub const NOTE_REQUIRED: &str = "NOTE_REQUIRED";
/// The candidate is not in the state the review write moves from.
pub const QUOTA_CANDIDATE_STATE: &str = "QUOTA_CANDIDATE_STATE";
/// The SKU version is not one the tenant's SKU holds.
pub const QUOTA_SKU_VERSION_UNKNOWN: &str = "QUOTA_SKU_VERSION_UNKNOWN";
/// types-registry holds another revision of an entity than the candidate was judged with.
const QUOTA_SOURCE_CONFLICT: &str = super::intake::QUOTA_SOURCE_CONFLICT;

/// Register the review operations.
pub(super) fn register(router: Router, openapi: &dyn OpenApiRegistry) -> Router {
    let router = OperationBuilder::post(format!("{CANDIDATES}/{{id}}/submit"))
        .operation_id("bss_products.submit_quota_candidate")
        .summary("Submit a quota candidate for review")
        .description(
            "Proposes a staged candidate of the caller's tenant for one SKU version: the plan \
             units into which the record's bounds convert exactly, the bases (`fixed`, \
             `per_plan_unit`) and the reviewed conditions (P-D-269). The submit pins the \
             registry entities the definition carries (the metric, the record's unit and its \
             canonical unit, both policies and each plan unit) at the content intake judged. \
             Takes the candidate's ETag as If-Match; 200 with the submitted candidate and its \
             new ETag. Asks `submit` on `quota_definition`. Refusals: 400 VALIDATION (a missing \
             If-Match included), VALIDATION on `supported_bases[i]` for a basis other than \
             `fixed` or `per_plan_unit`, FIELD_TOO_LONG, QUOTA_SKU_VERSION_UNKNOWN on \
             `sku_version`, QUOTA_RECORD_INVALID on `definition_id` for a named definition \
             of another SKU or metric, QUOTA_VERSION_UNSUPPORTED, \
             QUOTA_REFERENCE_UNKNOWN, QUOTA_REFERENCE_FAMILY, QUOTA_UNIT_MISMATCH or \
             QUOTA_UNIT_INEXACT on `allowed_unit_refs[i]`, \
             QUOTA_REFERENCE_UNKNOWN on the record's field (`metric_ref`, `unit_ref`, \
             `metric_ref.canonical_unit_ref`, `accounting_policy_ref` or `scope_policy_ref`) \
             when types-registry no longer holds an entity the candidate was judged with, \
             QUOTA_DISPLAY_TEXT_MISSING on `profile_conditions`, QUOTA_PROFILE_UNSUPPORTED; 404 \
             for an unknown or foreign candidate, or a named definition the `submit` grant does \
             not reach (P-D-273); 409 QUOTA_CANDIDATE_STATE, STALE_REVISION, \
             QUOTA_SOURCE_CONFLICT when types-registry holds another revision of an entity the \
             candidate was judged with (a discovery of the same revision replays the candidate \
             unjudged: a new snapshot revision of the target, or a manual record under a new \
             record id, judges it again), IDEMPOTENCY_CONFLICT, IDEMPOTENCY_KEY_IN_FLIGHT; 503 \
             QUOTA_REGISTRY_UNAVAILABLE when types-registry does not answer.",
        )
        .tag(TAG)
        .authenticated()
        .no_license_required()
        .path_param("id", "The candidate's id")
        .param(preconditions::if_match_param())
        .param(replay::param())
        .json_request::<ProductsQuotaSubmitRequest>(
            openapi,
            "sku_id, sku_version, allowed_unit_refs, supported_bases, profile_conditions",
        )
        .handler(submit)
        .json_response_with_schema::<ProductsQuotaCandidate>(
            openapi,
            StatusCode::OK,
            "The submitted candidate.",
        )
        .response_header(preconditions::etag_header())
        .error_400(openapi)
        .error_401(openapi)
        .error_403(openapi)
        .error_404(openapi)
        .error_409(openapi)
        .error_500(openapi)
        .error_503(openapi)
        .register(router, openapi);
    let router = OperationBuilder::post(format!("{CANDIDATES}/{{id}}/approve"))
        .operation_id("bss_products.approve_quota_candidate")
        .summary("Approve a quota candidate")
        .description(
            "Approves a submitted candidate of the caller's tenant and publishes one immutable \
             definition version with its mapping and profile versions, built from the stored \
             candidate and proposal alone, and recorded unavailable for sale until qualification \
             (P-D-269, P-D-270, P-D-271). Takes the candidate's ETag as If-Match. A JSON body is \
             required (`{}` at least); the note is optional. 200 with the approved candidate and \
             the definition as its version read answers it in English; the answer carries no \
             ETag, since the approved candidate does not move again. Asks `approve` on \
             `quota_definition`. Refusals: 400 VALIDATION (a missing If-Match or body \
             included), FIELD_TOO_LONG on `note`, QUOTA_SKU_VERSION_UNKNOWN, \
             QUOTA_RECORD_INVALID on `definition_id` when the named definition is of another \
             SKU or metric; 403 SOD_VIOLATION for the candidate's submitter or a manual \
             record's author; 404 for an unknown or foreign candidate, or a named definition \
             the `approve` grant does not reach (P-D-273); 409 \
             QUOTA_CANDIDATE_STATE, STALE_REVISION, CONTENDED when a \
             concurrent approval took the definition's next version or the mapping lineage's \
             (not retried: send the approval again), IDEMPOTENCY_CONFLICT, \
             IDEMPOTENCY_KEY_IN_FLIGHT.",
        )
        .tag(TAG)
        .authenticated()
        .no_license_required()
        .path_param("id", "The candidate's id")
        .param(preconditions::if_match_param())
        .param(replay::param())
        .json_request::<ProductsQuotaDecisionRequest>(openapi, "note")
        .handler(approve)
        .json_response_with_schema::<ProductsQuotaApproval>(
            openapi,
            StatusCode::OK,
            "The approved candidate and the definition version it published.",
        )
        .error_400(openapi)
        .error_401(openapi)
        .error_403(openapi)
        .error_404(openapi)
        .error_409(openapi)
        .error_500(openapi)
        .error_503(openapi)
        .register(router, openapi);
    OperationBuilder::post(format!("{CANDIDATES}/{{id}}/reject"))
        .operation_id("bss_products.reject_quota_candidate")
        .summary("Reject a quota candidate")
        .description(
            "Rejects a submitted candidate of the caller's tenant with a note (P-D-269). Takes the \
             candidate's ETag as If-Match. A JSON body is required (`{}` at least); the note is \
             required. 200 with the rejected candidate and its new ETag. Asks `approve` on \
             `quota_definition`. Refusals: 400 VALIDATION (a missing If-Match or body \
             included), NOTE_REQUIRED, FIELD_TOO_LONG on `note`; 403 SOD_VIOLATION for the \
             candidate's submitter or a manual record's author; 404 for an unknown or foreign \
             candidate; 409 QUOTA_CANDIDATE_STATE, STALE_REVISION, IDEMPOTENCY_CONFLICT, \
             IDEMPOTENCY_KEY_IN_FLIGHT.",
        )
        .tag(TAG)
        .authenticated()
        .no_license_required()
        .path_param("id", "The candidate's id")
        .param(preconditions::if_match_param())
        .param(replay::param())
        .json_request::<ProductsQuotaDecisionRequest>(openapi, "note")
        .handler(reject)
        .json_response_with_schema::<ProductsQuotaCandidate>(
            openapi,
            StatusCode::OK,
            "The rejected candidate.",
        )
        .response_header(preconditions::etag_header())
        .error_400(openapi)
        .error_401(openapi)
        .error_403(openapi)
        .error_404(openapi)
        .error_409(openapi)
        .error_500(openapi)
        .error_503(openapi)
        .register(router, openapi)
}

/// A refusal on the request: `code` on `field`.
fn refused(code: &'static str, field: &str, detail: impl Into<String>) -> CanonicalError {
    let mut report = ValidationReport::new();
    report.violate(code, field, detail);
    On::DEFINITION.refuse(DomainError::Validation(report))
}

fn refusal(r: RecordRefusal) -> CanonicalError {
    refused(r.code, &r.field, r.detail)
}

/// The 404 of a candidate the tenant does not hold, or of a path that is not a UUID.
fn not_found(id: Uuid) -> CanonicalError {
    On::DEFINITION.refuse(DomainError::NotFound {
        what: "quota_candidate",
        id,
    })
}

/// 409 `QUOTA_CANDIDATE_STATE`: the candidate is not in `expected`.
fn wrong_state(candidate: &QuotaCandidate, expected: CandidateState) -> DomainError {
    DomainError::Conflict {
        code: QUOTA_CANDIDATE_STATE,
        detail: format!(
            "the candidate is {}, not {}",
            candidate.state.as_str(),
            expected.as_str()
        ),
    }
}

/// The candidate `path` names: a UUID, or the 404.
fn candidate_id(path: &str) -> Result<Uuid, CanonicalError> {
    Uuid::parse_str(path).map_err(|_| not_found(Uuid::nil()))
}

/// The tenant's candidate `id`, in `expected` and at the version the caller read: a missing one
/// is 404, another state 409 `QUOTA_CANDIDATE_STATE`, another version 409 `STALE_REVISION`. The
/// transaction checks both again.
async fn load_candidate(
    runner: &impl DBRunner,
    scope: &AccessScope,
    tenant: Uuid,
    id: Uuid,
    expected: CandidateState,
    seen: InternalRevision,
) -> Result<QuotaCandidate, CanonicalError> {
    let candidate = store::find_candidate(runner, scope, tenant, id)
        .await
        .map_err(|e| On::DEFINITION.stored(&e))?
        .ok_or_else(|| not_found(id))?;
    if candidate.state != expected {
        return Err(On::DEFINITION.refuse(wrong_state(&candidate, expected)));
    }
    if candidate.version != seen.get() {
        return Err(On::DEFINITION.refuse(DomainError::StaleRevision {
            expected: seen.get(),
            found: candidate.version,
        }));
    }
    Ok(candidate)
}

/// A stored member that does not read is a corrupt candidate row (P-D-274).
fn corrupt(candidate: &QuotaCandidate, why: &str) -> CanonicalError {
    On::DEFINITION.stored(&RepoError::CorruptRow(format!(
        "a stored quota row does not read: products_quota_candidate, id {}: {why}",
        candidate.id
    )))
}

/// The candidate's stored record and evidence, as intake judged them.
fn stored_record(
    candidate: &QuotaCandidate,
) -> Result<(QuotaCapabilityV1, EvidenceSet), CanonicalError> {
    let record = serde_json::from_value(candidate.record_json.clone())
        .map_err(|e| corrupt(candidate, &format!("record_json: {e}")))?;
    let evidence = EvidenceSet::from_json(&candidate.evidence_json)
        .map_err(|e| corrupt(candidate, &format!("evidence_json: {e}")))?;
    Ok((record, evidence))
}

/// The registry entities a record names, as the submit pins them.
struct RecordEntities {
    metric: QuotaMetricV1,
    unit: QuotaUnitV1,
    scope: QuotaScopePolicyV1,
    accounting: QuotaAccountingPolicyV1,
}

/// One entity the record names, read as the caller and pinned only when its content is the content
/// intake judged: an entity types-registry no longer holds is `QUOTA_REFERENCE_UNKNOWN` on
/// `field`, another revision 409 `QUOTA_SOURCE_CONFLICT`. A discovery of the same source revision
/// replays the stored candidate without judging it, so only a new snapshot revision of the target,
/// or a manual record under a new record id, stages a candidate judged against the new content.
async fn pin<T: DeserializeOwned>(
    state: &QuotaState,
    ctx: &SecurityContext,
    judged: &EvidenceSet,
    pinned: &mut Pinned,
    field: &str,
    id: &str,
) -> Result<T, CanonicalError> {
    let instance = state
        .registry
        .instance(ctx, id)
        .await
        .map_err(|e| registry_unavailable(&e.0))?
        .ok_or_else(|| {
            refused(
                QUOTA_REFERENCE_UNKNOWN,
                field,
                format!("{id} is not registered"),
            )
        })?;
    let digest = content_digest(&instance.content);
    if judged.0.get(id).is_none_or(|e| e.digest != digest) {
        return Err(On::DEFINITION.refuse(DomainError::Conflict {
            code: QUOTA_SOURCE_CONFLICT,
            detail: format!(
                "types-registry holds another revision of {id} than the candidate was judged \
                 with; a new snapshot revision of the target, or a manual record under a new \
                 record id, judges it again"
            ),
        }));
    }
    let parsed = serde_json::from_value(instance.content.clone()).map_err(|e| {
        refused(
            QUOTA_REFERENCE_FAMILY,
            field,
            format!("{id} does not read as its family: {e}"),
        )
    })?;
    pinned.insert(PinnedEntity {
        evidence: Evidence {
            gts_id: id.to_owned(),
            uuid: instance.uuid,
            resource_version: instance.resource_version,
            digest,
        },
        content: instance.content,
    });
    Ok(parsed)
}

/// The metric, the record's unit and its canonical unit, and both policies, pinned.
async fn pin_record(
    state: &QuotaState,
    ctx: &SecurityContext,
    record: &QuotaCapabilityV1,
    judged: &EvidenceSet,
    pinned: &mut Pinned,
) -> Result<RecordEntities, CanonicalError> {
    let metric: QuotaMetricV1 = pin(
        state,
        ctx,
        judged,
        pinned,
        "metric_ref",
        record.metric_ref.as_ref(),
    )
    .await?;
    let unit: QuotaUnitV1 = pin(
        state,
        ctx,
        judged,
        pinned,
        "unit_ref",
        record.unit_ref.as_ref(),
    )
    .await?;
    let canonical = metric.canonical_unit_ref.clone();
    if canonical != record.unit_ref.as_ref() {
        pin::<QuotaUnitV1>(
            state,
            ctx,
            judged,
            pinned,
            "metric_ref.canonical_unit_ref",
            &canonical,
        )
        .await?;
    }
    let accounting = pin(
        state,
        ctx,
        judged,
        pinned,
        "accounting_policy_ref",
        record.accounting_policy_ref.as_ref(),
    )
    .await?;
    let scope = pin(
        state,
        ctx,
        judged,
        pinned,
        "scope_policy_ref",
        record.scope_policy_ref.as_ref(),
    )
    .await?;
    Ok(RecordEntities {
        metric,
        unit,
        scope,
        accounting,
    })
}

/// Each plan unit of the proposal, read as the caller: a unit instance of the metric's canonical
/// unit into which the record's bounds convert exactly, each refusal on `allowed_unit_refs[i]`.
async fn allowed_units(
    state: &QuotaState,
    ctx: &SecurityContext,
    record: &QuotaCapabilityV1,
    entities: &RecordEntities,
    refs: &[String],
    pinned: &mut Pinned,
) -> Result<Vec<AllowedUnit>, CanonicalError> {
    let mut allowed = Vec::with_capacity(refs.len());
    for (i, unit_ref) in refs.iter().enumerate() {
        let field = format!("allowed_unit_refs[{i}]");
        major_only(&field, unit_ref).map_err(refusal)?;
        let instance = state
            .registry
            .instance(ctx, unit_ref)
            .await
            .map_err(|e| registry_unavailable(&e.0))?
            .ok_or_else(|| {
                refused(
                    QUOTA_REFERENCE_UNKNOWN,
                    &field,
                    format!("{unit_ref} is not registered"),
                )
            })?;
        let names_itself = instance
            .content
            .get("id")
            .and_then(serde_json::Value::as_str)
            == Some(unit_ref);
        let unit = (instance.type_id == UNIT_TYPE && names_itself)
            .then(|| serde_json::from_value::<QuotaUnitV1>(instance.content.clone()).ok())
            .flatten()
            .ok_or_else(|| {
                refused(
                    QUOTA_REFERENCE_FAMILY,
                    &field,
                    format!("{unit_ref} is not a {UNIT_TYPE} instance"),
                )
            })?;
        if unit.canonical_unit_ref != entities.metric.canonical_unit_ref {
            return Err(refused(
                QUOTA_UNIT_MISMATCH,
                &field,
                format!(
                    "{unit_ref} converts into {}, the metric counts in {}",
                    unit.canonical_unit_ref, entities.metric.canonical_unit_ref
                ),
            ));
        }
        let (min, step, max) = convert_bounds(&record.limits, &entities.unit, &unit)
            .map_err(|e| refused(e.code(), &field, format!("the record's bounds: {e}")))?;
        let evidence = Evidence {
            gts_id: unit_ref.clone(),
            uuid: instance.uuid,
            resource_version: instance.resource_version,
            digest: content_digest(&instance.content),
        };
        pinned.insert(PinnedEntity {
            evidence: evidence.clone(),
            content: instance.content,
        });
        allowed.push(AllowedUnit {
            unit,
            evidence,
            min,
            step,
            max,
        });
    }
    Ok(allowed)
}

/// The SKU version a proposal names: a canonical decimal from 1 that the tenant's SKU holds.
async fn sku_version(
    runner: &impl DBRunner,
    scope: &AccessScope,
    tenant: Uuid,
    sku_id: Uuid,
    text: &str,
) -> Result<QuotaVersion, CanonicalError> {
    let unknown = || {
        refused(
            QUOTA_SKU_VERSION_UNKNOWN,
            "sku_version",
            format!("the SKU {sku_id} of the caller's tenant holds no version {text:?}"),
        )
    };
    let version: QuotaVersion = text.parse().map_err(|_| unknown())?;
    let held = version_repo::versions(runner, scope, tenant, sku_id)
        .await
        .map_err(|e| On::DEFINITION.stored(&e))?
        .iter()
        .any(|v| u64::try_from(v.published_version).ok() == Some(version.get()));
    if held { Ok(version) } else { Err(unknown()) }
}

/// The deterministic UUID (`GtsId::to_uuid`) of the metric a record names, as a definition
/// stores it; `None` for a reference that does not parse.
fn metric_uuid_of(metric_ref: &str) -> Option<Uuid> {
    gts::GtsId::try_new(metric_ref).ok().map(|id| id.to_uuid())
}

/// Why a proposal for `sku_id` of the metric `metric_uuid` may not take the next version of the
/// stored definition `definition_id` (`None` when it may), P-D-270: every version of a definition
/// is of one SKU and one metric; the target lineage of the candidate may differ.
fn named_definition_misfit(
    definition_id: Uuid,
    stored: &DefinitionVersion,
    sku_id: Uuid,
    metric_uuid: Option<Uuid>,
) -> Option<String> {
    if stored.sku_id != sku_id {
        return Some(format!(
            "the definition {definition_id} is of the SKU {}, not of the proposal's SKU {sku_id}; \
             its next version must be for the same SKU",
            stored.sku_id
        ));
    }
    (Some(stored.metric_uuid) != metric_uuid)
        .then(|| format!("the definition {definition_id} counts another metric than the candidate"))
}

/// The latest stored version of the definition `definition_id`, with its number.
async fn latest_definition(
    runner: &impl DBRunner,
    scope: &AccessScope,
    tenant: Uuid,
    definition_id: Uuid,
) -> Result<Option<(QuotaVersion, DefinitionVersion)>, RepoError> {
    let Some(latest) =
        catalog::latest_definition_version(runner, scope, tenant, definition_id).await?
    else {
        return Ok(None);
    };
    Ok(
        catalog::find_definition(runner, scope, tenant, definition_id, latest)
            .await?
            .map(|(definition, _)| (latest, definition)),
    )
}

/// The miss of a named definition: the tenant does not hold it, or the grant does not reach it.
fn definition_missed(id: Uuid) -> DomainError {
    DomainError::NotFound {
        what: "quota_definition",
        id,
    }
}

/// A definition a proposal versions, read under the whole `grant` (P-D-273): one the grant
/// reaches, else 404 on the quota definition; of the proposal's SKU and the candidate's metric
/// (P-D-270), else 400 `QUOTA_RECORD_INVALID` on `definition_id`.
async fn named_definition(
    runner: &impl DBRunner,
    grant: &AccessScope,
    tenant: Uuid,
    definition_id: Uuid,
    (sku_id, metric_uuid): (Uuid, Option<Uuid>),
) -> Result<(), CanonicalError> {
    let (_, stored) = latest_definition(runner, grant, tenant, definition_id)
        .await
        .map_err(|e| On::DEFINITION.stored(&e))?
        .ok_or_else(|| On::DEFINITION.refuse(definition_missed(definition_id)))?;
    match named_definition_misfit(definition_id, &stored, sku_id, metric_uuid) {
        None => Ok(()),
        Some(detail) => Err(refused(QUOTA_RECORD_INVALID, "definition_id", detail)),
    }
}

/// The candidate's audit row for `action`, in the review write's transaction.
async fn audit(
    tx: &impl DBRunner,
    scope: &AccessScope,
    tenant: Uuid,
    candidate: &QuotaCandidate,
    review: Audited,
) -> Result<(), TxError> {
    let (action, actor, at, reason) = review;
    repo::write_eventless_act_audit(
        tx,
        scope,
        AuditCommon {
            audit_id: Uuid::now_v7(),
            tenant_id: tenant,
            actor_ref: actor,
            action: action.to_owned(),
            subject_kind: super::candidates::SUBJECT_CANDIDATE.to_owned(),
            reason,
            correlation_id: None,
            written_at: at,
            lifecycle: LifecycleMove::NONE,
        },
        candidate.id,
        Some(candidate.version),
    )
    .await
    .map_err(TxError::Repo)
}

/// One review write inside its transaction (P-D-269): the candidate moves only from the state the
/// write leaves and the version the caller read; a miss reads it again, and another state is 409
/// `QUOTA_CANDIDATE_STATE`, the same state 409 `STALE_REVISION`. Answers the moved candidate.
// @cpt-state:cpt-cf-bss-products-state-quota-candidate:p1
pub(super) async fn review_in(
    tx: &impl DBRunner,
    scope: &AccessScope,
    tenant: Uuid,
    id: Uuid,
    seen: InternalRevision,
    write: ReviewWrite,
) -> Result<QuotaCandidate, TxError> {
    let from = write.state_before();
    let moved = store::review_candidate(tx, scope, tenant, id, seen.get(), write)
        .await
        .map_err(TxError::Repo)?;
    let read = store::find_candidate(tx, scope, tenant, id)
        .await
        .map_err(TxError::Repo)?
        .ok_or(TxError::Refused(DomainError::NotFound {
            what: "quota_candidate",
            id,
        }))?;
    if moved {
        return Ok(read);
    }
    Err(TxError::Refused(if read.state == from {
        DomainError::StaleRevision {
            expected: seen.get(),
            found: read.version,
        }
    } else {
        wrong_state(&read, from)
    }))
}

/// What a review write's audit row says: its action, its actor, its time and its reason.
type Audited = (&'static str, Uuid, time::OffsetDateTime, Option<String>);

/// A submit's or a rejection's transaction: the claim, the move, the audit row and the receipt
/// of the moved candidate.
async fn move_tx(
    state: &QuotaState,
    scope: &AccessScope,
    tenant: Uuid,
    (id, seen): (Uuid, InternalRevision),
    write: ReviewWrite,
    audited: Audited,
    claim: Option<IdempotencyClaimInput>,
) -> Result<Response, CanonicalError> {
    let scope = scope.clone();
    state
        .api
        .db
        .db()
        .transaction_with_retry::<Response, TxError, _, _>(
            TxConfig::default(),
            contention_db_err,
            move |tx| {
                let (scope, write, claim) = (scope.clone(), write.clone(), claim.clone());
                let audited = audited.clone();
                Box::pin(async move {
                    if let Some(response) = replay::begin(tx, tenant, claim.as_ref()).await? {
                        return Ok(response);
                    }
                    let candidate = review_in(tx, &scope, tenant, id, seen, write).await?;
                    audit(tx, &scope, tenant, &candidate, audited).await?;
                    let body = candidate_dto(tx, &scope, tenant, candidate)
                        .await
                        .map_err(TxError::Repo)?;
                    replay::finish(tx, tenant, claim.as_ref(), StatusCode::OK, &body).await
                })
            },
        )
        .await
        .map_err(|e| On::DEFINITION.tx(e))
}

// @cpt-flow:cpt-cf-bss-products-flow-quota-catalog-review:p1
async fn submit(
    Extension(state): Extension<Arc<QuotaState>>,
    Extension(enforcer): Extension<PolicyEnforcer>,
    extension_ctx: Option<Extension<SecurityContext>>,
    Path(path): Path<String>,
    headers: HeaderMap,
    body: Result<Json<serde_json::Value>, JsonRejection>,
) -> Result<Response, CanonicalError> {
    let ctx = require_authenticated(extension_ctx)?;
    // @cpt-begin:cpt-cf-bss-products-flow-quota-catalog-review:p1:inst-quota-submit
    let tenant = ctx.subject_tenant_id();
    let (grant, scope) = review_scopes(&enforcer, &ctx, actions::SUBMIT).await?;
    let seen = preconditions::if_match(&headers).map_err(|e| On::DEFINITION.refuse(e))?;
    let id = candidate_id(&path)?;
    let payload = On::DEFINITION.body(body)?;
    let endpoint = format!("{CANDIDATES}/{id}/submit");
    let claim = On::DEFINITION.input(&state.api, &headers, endpoint, &payload)?;
    if let Some(response) = On::DEFINITION
        .replayed(&state.api, tenant, claim.as_ref())
        .await?
    {
        return Ok(response);
    }
    let request: ProductsQuotaSubmitRequest = On::DEFINITION.read(payload)?;
    let conn = On::DEFINITION.conn(&state.api)?;
    let candidate = load_candidate(&conn, &scope, tenant, id, CandidateState::Staged, seen).await?;
    let (record, judged) = stored_record(&candidate)?;
    // @cpt-begin:cpt-cf-bss-products-algo-quota-catalog-definition:p1:inst-quota-proposal
    let bases = request
        .supported_bases
        .iter()
        .enumerate()
        .map(|(i, token)| {
            parse_basis(token).ok_or_else(|| {
                refused(
                    "VALIDATION",
                    &format!("supported_bases[{i}]"),
                    "one of fixed or per_plan_unit",
                )
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    check_proposal_shape(
        &bases,
        &request.allowed_unit_refs,
        &request.profile_conditions,
        request.note.as_deref(),
    )
    .map_err(refusal)?;
    let sku_version =
        sku_version(&conn, &scope, tenant, request.sku_id, &request.sku_version).await?;
    // The definition stores the metric's deterministic UUID; compare the same.
    if let Some(definition_id) = request.definition_id {
        let identity = (request.sku_id, metric_uuid_of(record.metric_ref.as_ref()));
        named_definition(&conn, &grant, tenant, definition_id, identity).await?;
    }
    let mut pinned = Pinned::default();
    let entities = pin_record(&state, &ctx, &record, &judged, &mut pinned).await?;
    check_profile(&record, &entities.scope, &entities.accounting).map_err(refusal)?;
    let allowed = allowed_units(
        &state,
        &ctx,
        &record,
        &entities,
        &request.allowed_unit_refs,
        &mut pinned,
    )
    .await?;
    let proposal = Proposal {
        sku_id: request.sku_id,
        sku_version,
        definition_id: request.definition_id,
        allowed_units: allowed,
        supported_bases: bases,
        profile_conditions: request.profile_conditions,
    };
    // @cpt-end:cpt-cf-bss-products-algo-quota-catalog-definition:p1:inst-quota-proposal
    // @cpt-begin:cpt-cf-bss-products-state-quota-candidate:p1:inst-quota-state-submit
    let (actor, at) = (ctx.subject_id(), stored_now());
    let audited = (ACTION_SUBMIT, actor, at, request.note.clone());
    let write = ReviewWrite::Submit {
        proposal: proposal_json(&proposal),
        evidence: pinned.to_json(),
        by: actor,
        at,
        note: request.note,
    };
    move_tx(&state, &scope, tenant, (id, seen), write, audited, claim).await
    // @cpt-end:cpt-cf-bss-products-state-quota-candidate:p1:inst-quota-state-submit
    // @cpt-end:cpt-cf-bss-products-flow-quota-catalog-review:p1:inst-quota-submit
}

/// A decision's note: at most 2000 characters, and required (not blank) on a rejection.
fn checked_note(note: Option<String>, required: bool) -> Result<Option<String>, CanonicalError> {
    match note {
        Some(note) if note.chars().count() > MAX_TEXT_CHARS => Err(refused(
            "FIELD_TOO_LONG",
            "note",
            format!("at most {MAX_TEXT_CHARS} characters"),
        )),
        Some(note) if required && note.trim().is_empty() => {
            Err(refused(NOTE_REQUIRED, "note", "a rejection needs a note"))
        }
        None if required => Err(refused(NOTE_REQUIRED, "note", "a rejection needs a note")),
        other => Ok(other),
    }
}

// @cpt-begin:cpt-cf-bss-products-algo-quota-catalog-definition:p1:inst-quota-separation
/// Separation of duties (P-D-269): the candidate's submitter, and a manual record's author, may
/// not decide it: 403 `SOD_VIOLATION`, logged as a denial.
fn separated(actor: Uuid, tenant: Uuid, candidate: &QuotaCandidate) -> Result<(), CanonicalError> {
    let submitted = candidate.submitted_by == Some(actor);
    let authored = candidate.origin == CandidateOrigin::Manual && candidate.created_by == actor;
    if !(submitted || authored) {
        return Ok(());
    }
    let why = if submitted {
        "the candidate's submitter may not decide it"
    } else {
        "a manual record's author may not decide its candidate"
    };
    tracing::warn!(
        target: "bss_products.authz.deny",
        subject_id = %actor,
        subject_tenant_id = %tenant,
        candidate = %candidate.id,
        reason = why,
        "bss-products: a quota candidate's decision refused by separation of duties"
    );
    Err(On::DEFINITION.refuse(DomainError::Forbidden {
        code: SOD_VIOLATION,
        detail: why.to_owned(),
    }))
}
// @cpt-end:cpt-cf-bss-products-algo-quota-catalog-definition:p1:inst-quota-separation

/// What a decision's first checks leave: the decision to make, or the stored answer of a keyed
/// retry.
enum Gate {
    Decide(Decision),
    Replayed(Response),
}

/// A decision's request after the caller, the grant, the tag, the path and the replay store:
/// `grant` is the whole `approve` grant, which reads the definition a proposal names, and `scope`
/// its `tenant_only()`, for every other row (P-D-273).
struct Decision {
    tenant: Uuid,
    actor: Uuid,
    grant: AccessScope,
    scope: AccessScope,
    id: Uuid,
    seen: InternalRevision,
    note: Option<String>,
    claim: Option<IdempotencyClaimInput>,
}

/// What an approve and a reject check first, in order: the caller, `approve` on
/// `quota_definition` anchored to its tenant, `If-Match`, the path, the body, and the replay store
/// (P-D-198). `Err` carries the answer: a refusal, or the stored answer of a keyed retry.
async fn decision(
    enforcer: &PolicyEnforcer,
    extension_ctx: Option<Extension<SecurityContext>>,
    state: &QuotaState,
    (path, headers): (&str, &HeaderMap),
    body: Result<Json<serde_json::Value>, JsonRejection>,
    (verb, note_required): (&str, bool),
) -> Result<Gate, CanonicalError> {
    let ctx = require_authenticated(extension_ctx)?;
    let tenant = ctx.subject_tenant_id();
    let (grant, scope) = review_scopes(enforcer, &ctx, actions::APPROVE).await?;
    let seen = preconditions::if_match(headers).map_err(|e| On::DEFINITION.refuse(e))?;
    let id = candidate_id(path)?;
    let payload = On::DEFINITION.body(body)?;
    let endpoint = format!("{CANDIDATES}/{id}/{verb}");
    let claim = On::DEFINITION.input(&state.api, headers, endpoint, &payload)?;
    if let Some(response) = On::DEFINITION
        .replayed(&state.api, tenant, claim.as_ref())
        .await?
    {
        return Ok(Gate::Replayed(response));
    }
    let request: ProductsQuotaDecisionRequest = On::DEFINITION.read(payload)?;
    let note = checked_note(request.note, note_required)?;
    Ok(Gate::Decide(Decision {
        tenant,
        actor: ctx.subject_id(),
        grant,
        scope,
        id,
        seen,
        note,
        claim,
    }))
}

/// A stored candidate whose approval does not build is a corrupt row (P-D-274).
fn unbuildable(tenant: Uuid, id: Uuid, why: &str) -> TxError {
    TxError::Repo(RepoError::CorruptRow(format!(
        "a stored quota row does not read: products_quota_candidate, tenant {tenant}, id {id}: \
         {why}"
    )))
}

/// A concurrent approval took the definition's next version, or the mapping lineage's (its first
/// included: the lineage holds one row per version): 409 `CONTENDED` (P-D-270).
fn contended(e: RepoError) -> TxError {
    match e {
        RepoError::Refused(RepoRefusal::QuotaApprovalContended) => {
            TxError::Refused(DomainError::Conflict {
                code: "CONTENDED",
                detail: "a concurrent approval took this definition's or this mapping lineage's \
                         next version; retry"
                    .into(),
            })
        }
        other => TxError::Repo(other),
    }
}

/// The approval's statements, inside its transaction (P-D-270, P-D-271): the SKU version again, the
/// lineage, the named definition's latest version under the whole grant (P-D-273) with its SKU and
/// metric again, the build from stored rows, then the candidate's move first (a second approval of
/// it misses here), the mapping unless reused, the definition with its resource types, availability
/// revision 1, the audit row, and the answer.
async fn approve_in(
    tx: &impl DBRunner,
    hook: Option<&tokio::sync::Barrier>,
    d: &Decision,
    candidate: &QuotaCandidate,
    target: &crate::domain::quota::QuotaTarget,
) -> Result<ProductsQuotaApproval, TxError> {
    let (scope, tenant, at) = (&d.scope, d.tenant, stored_now());
    let proposal = candidate
        .proposal_json
        .as_ref()
        .ok_or_else(|| unbuildable(tenant, d.id, "no proposal"))
        .and_then(|p| {
            crate::domain::quota::review::proposal_from_json(p)
                .map_err(|why| unbuildable(tenant, d.id, &why))
        })?;
    let held = version_repo::versions(tx, scope, tenant, proposal.sku_id)
        .await
        .map_err(TxError::Repo)?
        .iter()
        .any(|v| u64::try_from(v.published_version).ok() == Some(proposal.sku_version.get()));
    if !held {
        let mut report = ValidationReport::new();
        report.violate(
            QUOTA_SKU_VERSION_UNKNOWN,
            "sku_version",
            "the SKU no longer holds the proposed version",
        );
        return Err(TxError::Refused(DomainError::Validation(report)));
    }
    let lineage = catalog::latest_mapping(tx, scope, tenant, target.id, &candidate.capability_key)
        .await
        .map_err(TxError::Repo)?;
    // The named definition's SKU and metric are read again under the approval's transaction.
    let latest = match proposal.definition_id {
        Some(definition_id) => {
            let (version, stored) = latest_definition(tx, &d.grant, tenant, definition_id)
                .await
                .map_err(TxError::Repo)?
                .ok_or_else(|| TxError::Refused(definition_missed(definition_id)))?;
            let metric = candidate.record_json["metric_ref"]
                .as_str()
                .and_then(metric_uuid_of);
            let misfit = named_definition_misfit(definition_id, &stored, proposal.sku_id, metric);
            if let Some(detail) = misfit {
                let mut report = ValidationReport::new();
                report.violate(QUOTA_RECORD_INVALID, "definition_id", detail);
                return Err(TxError::Refused(DomainError::Validation(report)));
            }
            Some(version)
        }
        None => None,
    };
    // @cpt-begin:cpt-cf-bss-products-flow-quota-catalog-review:p1:inst-quota-unavailable
    let draft = build(candidate, target, lineage.as_ref(), latest, d.actor, at)
        .map_err(|why| unbuildable(tenant, d.id, &why))?;
    if let Some(hook) = hook {
        hook.wait().await;
    }
    // @cpt-begin:cpt-cf-bss-products-state-quota-candidate:p1:inst-quota-state-approve
    let approved = review_in(
        tx,
        scope,
        tenant,
        d.id,
        d.seen,
        ReviewWrite::Approve {
            by: d.actor,
            at,
            note: d.note.clone(),
            definition_id: draft.definition.definition_id,
            definition_version: draft.definition.version,
        },
    )
    .await?;
    // @cpt-end:cpt-cf-bss-products-state-quota-candidate:p1:inst-quota-state-approve
    if !draft.reuse_mapping {
        catalog::insert_mapping(tx, scope, tenant, &draft.mapping)
            .await
            .map_err(contended)?;
    }
    catalog::insert_definition(
        tx,
        scope,
        tenant,
        &draft.definition,
        &draft.resource_type_uuids,
    )
    .await
    .map_err(contended)?;
    catalog::insert_availability(tx, scope, tenant, &draft.availability)
        .await
        .map_err(contended)?;
    // @cpt-end:cpt-cf-bss-products-flow-quota-catalog-review:p1:inst-quota-unavailable
    let (definition_id, version) = (draft.definition.definition_id, draft.definition.version);
    let reason = format!("definition {definition_id} version {version}");
    audit(
        tx,
        scope,
        tenant,
        &approved,
        (ACTION_APPROVE, d.actor, at, Some(reason)),
    )
    .await?;
    let (row, availability) = catalog::find_definition(tx, scope, tenant, definition_id, version)
        .await
        .map_err(TxError::Repo)?
        .ok_or_else(|| unbuildable(tenant, d.id, "the published version did not read back"))?;
    let definition =
        project(&row, &availability, None).map_err(|why| unbuildable(tenant, d.id, &why))?;
    let candidate = candidate_dto(tx, scope, tenant, approved)
        .await
        .map_err(TxError::Repo)?;
    Ok(ProductsQuotaApproval {
        candidate,
        definition: ProductsQuotaDefinition::from(definition),
    })
}

/// The approval of a decided request: the candidate submitted and at the version read, an
/// independent approver, then one transaction (no registry or plugin call) that writes the
/// definition version and records the receipt of a keyed request.
async fn approve_decided(state: &QuotaState, d: Decision) -> Result<Response, CanonicalError> {
    let conn = On::DEFINITION.conn(&state.api)?;
    let candidate = load_candidate(
        &conn,
        &d.scope,
        d.tenant,
        d.id,
        CandidateState::Submitted,
        d.seen,
    )
    .await?;
    separated(d.actor, d.tenant, &candidate)?;
    let target = store::find_target(&conn, &d.scope, d.tenant, candidate.target_id)
        .await
        .map_err(|e| On::DEFINITION.stored(&e))?
        .ok_or_else(|| {
            On::DEFINITION.tx(unbuildable(d.tenant, d.id, "its target is not stored"))
        })?;
    let d = Arc::new(d);
    let (candidate, target) = (Arc::new(candidate), Arc::new(target));
    let shared = (d.clone(), candidate, target, state.approve_hook.clone());
    state
        .api
        .db
        .db()
        .transaction_with_retry::<Response, TxError, _, _>(
            TxConfig::default(),
            contention_db_err,
            move |tx| {
                let (d, candidate, target, hook) = shared.clone();
                Box::pin(async move {
                    if let Some(response) = replay::begin(tx, d.tenant, d.claim.as_ref()).await? {
                        return Ok(response);
                    }
                    let answer = approve_in(tx, hook.as_deref(), &d, &candidate, &target).await?;
                    replay::finish(tx, d.tenant, d.claim.as_ref(), StatusCode::OK, &answer).await
                })
            },
        )
        .await
        .map_err(|e| On::DEFINITION.tx(e))
}

/// An approval by `actor` of the candidate `id` at version `seen`, past the endpoint's grant and
/// body checks, unkeyed: `scope` stands for the `approve` grant, which reads the named
/// definition, and its `tenant_only()` for every other row. Public for the Postgres race suite
/// only.
///
/// # Errors
/// As the approve endpoint: 403 `SOD_VIOLATION`, 404, 409 `QUOTA_CANDIDATE_STATE`,
/// `STALE_REVISION`, `CONTENDED`; storage failures.
#[doc(hidden)]
pub async fn approve_candidate(
    state: &QuotaState,
    scope: &AccessScope,
    (tenant, actor): (Uuid, Uuid),
    id: Uuid,
    seen: i64,
) -> Result<Response, CanonicalError> {
    let d = Decision {
        tenant,
        actor,
        grant: scope.clone(),
        scope: scope.tenant_only(),
        id,
        seen: InternalRevision::new(seen),
        note: None,
        claim: None,
    };
    approve_decided(state, d).await
}

async fn approve(
    Extension(state): Extension<Arc<QuotaState>>,
    Extension(enforcer): Extension<PolicyEnforcer>,
    extension_ctx: Option<Extension<SecurityContext>>,
    Path(path): Path<String>,
    headers: HeaderMap,
    body: Result<Json<serde_json::Value>, JsonRejection>,
) -> Result<Response, CanonicalError> {
    // @cpt-begin:cpt-cf-bss-products-flow-quota-catalog-review:p1:inst-quota-approve
    let d = match decision(
        &enforcer,
        extension_ctx,
        &state,
        (&path, &headers),
        body,
        ("approve", false),
    )
    .await?
    {
        Gate::Decide(d) => d,
        Gate::Replayed(response) => return Ok(response),
    };
    approve_decided(&state, d).await
    // @cpt-end:cpt-cf-bss-products-flow-quota-catalog-review:p1:inst-quota-approve
}

async fn reject(
    Extension(state): Extension<Arc<QuotaState>>,
    Extension(enforcer): Extension<PolicyEnforcer>,
    extension_ctx: Option<Extension<SecurityContext>>,
    Path(path): Path<String>,
    headers: HeaderMap,
    body: Result<Json<serde_json::Value>, JsonRejection>,
) -> Result<Response, CanonicalError> {
    // @cpt-begin:cpt-cf-bss-products-flow-quota-catalog-review:p1:inst-quota-reject
    let d = match decision(
        &enforcer,
        extension_ctx,
        &state,
        (&path, &headers),
        body,
        ("reject", true),
    )
    .await?
    {
        Gate::Decide(d) => d,
        Gate::Replayed(response) => return Ok(response),
    };
    let conn = On::DEFINITION.conn(&state.api)?;
    let candidate = load_candidate(
        &conn,
        &d.scope,
        d.tenant,
        d.id,
        CandidateState::Submitted,
        d.seen,
    )
    .await?;
    separated(d.actor, d.tenant, &candidate)?;
    let at = stored_now();
    let note = d.note.clone().unwrap_or_default();
    // @cpt-begin:cpt-cf-bss-products-state-quota-candidate:p1:inst-quota-state-reject
    let write = ReviewWrite::Reject {
        by: d.actor,
        at,
        note: note.clone(),
    };
    let audited = (ACTION_REJECT, d.actor, at, Some(note));
    move_tx(
        &state,
        &d.scope,
        d.tenant,
        (d.id, d.seen),
        write,
        audited,
        d.claim,
    )
    .await
    // @cpt-end:cpt-cf-bss-products-state-quota-candidate:p1:inst-quota-state-reject
    // @cpt-end:cpt-cf-bss-products-flow-quota-catalog-review:p1:inst-quota-reject
}
