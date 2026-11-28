//! @cpt-dod:cpt-cf-bss-products-dod-quota-catalog-intake:p1
//! Manual records and candidate reads (P-D-268, P-D-269): an administrator authors one capability
//! record for a target, judged by the discovery's own rules; the tenant's candidates are listed
//! and read.
//!
//! A manual create asks `author` on `quota_definition`, anchored to the caller's tenant, before
//! it reads the body; the reads ask `read`. The record is judged through types-registry as the
//! caller, before the write's transaction, which claims the key (P-D-198) and answers the stored
//! candidate of the record id when its digest is the same, refuses other content under that id,
//! or writes the candidate, its audit row and the receipt.
use std::sync::Arc;

use authz_resolver_sdk::PolicyEnforcer;
use axum::extract::{Path, rejection::JsonRejection};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json, Router};
use toolkit::api::OpenApiRegistry;
use toolkit::api::canonical_prelude::CanonicalError;
use toolkit::api::odata::OData;
use toolkit::api::operation_builder::OperationBuilder;
use toolkit_db::secure::{AccessScope, DBRunner, TxConfig};
use toolkit_odata::{Error as ODataError, Page};
use toolkit_security::SecurityContext;
use uuid::Uuid;

use super::dto::{ProductsQuotaCandidate, ProductsQuotaCandidateRequest};
use super::targets::{TAG, load_target};
use super::{
    CANDIDATES, On, QuotaState, candidate_dto, candidate_dtos, definition_scope,
    registry_unavailable,
};
use crate::api::rest::sku_list::{RawQuery, cursor_hash, params};
use crate::api::rest::{
    ApiState, TxError, contention_db_err, preconditions, replay, require_authenticated,
};
use crate::authz::actions;
use crate::domain::concurrency::InternalRevision;
use crate::domain::error::DomainError;
use crate::domain::quota::record::{JudgeError, Resolutions, judge_record};
use crate::domain::quota::{
    CandidateFilter, CandidateOrigin, CandidateState, NewCandidate, QuotaCandidate,
};
use crate::domain::validation::ValidationReport;
use crate::infra::idempotency::IdempotencyClaimInput;
use crate::infra::storage::repo::quota_intake_repo as store;
use crate::infra::storage::repo::{self, AuditCommon, LifecycleMove};
use crate::infra::storage::{RepoError, RepoRefusal, stored_now};

/// The audit action of a manual record create.
pub const ACTION_CREATE: &str = "quota_candidate.create";
/// The audit subject kind of a candidate.
pub const SUBJECT_CANDIDATE: &str = "quota_candidate";
/// Other content is stored under the record id.
const QUOTA_SOURCE_CONFLICT: &str = super::intake::QUOTA_SOURCE_CONFLICT;

/// Register the candidate operations.
pub(super) fn register(router: Router, openapi: &dyn OpenApiRegistry) -> Router {
    let router = OperationBuilder::post(CANDIDATES)
        .operation_id("bss_products.create_quota_candidate")
        .summary("Author a quota capability record")
        .description(
            "Judges one capability record for a target of the caller's tenant by the discovery's \
             own rules and stores it as a manual candidate with its author (P-D-268). 201 with \
             the candidate and its ETag; 200 with the stored candidate when the target holds \
             the same record under its id. Asks `author` on `quota_definition`. Refusals: 400 \
             VALIDATION on a body that does not read, and the record's own code on \
             `record.<field>` (QUOTA_RECORD_INVALID, QUOTA_REFERENCE_UNKNOWN, \
             QUOTA_REFERENCE_FAMILY, QUOTA_VERSION_UNSUPPORTED, QUOTA_CAPABILITY_UNSUPPORTED, \
             QUOTA_UNIT_INVALID, QUOTA_UNIT_MISMATCH, QUOTA_POLICY_INVALID, \
             QUOTA_DISPLAY_TEXT_MISSING); 404 for an unknown or foreign target; 409 \
             QUOTA_SOURCE_CONFLICT when the target holds other content under the record's id, \
             IDEMPOTENCY_CONFLICT, IDEMPOTENCY_KEY_IN_FLIGHT; 503 QUOTA_REGISTRY_UNAVAILABLE \
             when types-registry does not answer.",
        )
        .tag(TAG)
        .authenticated()
        .no_license_required()
        .json_request::<ProductsQuotaCandidateRequest>(openapi, "target_id, record")
        .param(replay::param())
        .handler(create_candidate)
        .json_response_with_schema::<ProductsQuotaCandidate>(
            openapi,
            StatusCode::CREATED,
            "The candidate.",
        )
        .response_header(preconditions::etag_header())
        .json_response_with_schema::<ProductsQuotaCandidate>(
            openapi,
            StatusCode::OK,
            "The stored candidate of the same record.",
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
    let router = OperationBuilder::get(CANDIDATES)
        .operation_id("bss_products.list_quota_candidates")
        .summary("List quota candidates")
        .description(
            "One page of the caller's tenant's candidates, newest first, then by id. Plain \
             `target_id` and `state` (`staged`, `submitted`, `approved` or `rejected`; else 400 \
             VALIDATION); `$top` (alias `limit`; default 50, at most 200) and `cursor` (alias \
             `$skiptoken`) from `page_info`. The cursor binds the caller's tenant, `target_id` \
             and `state`: reused by another tenant or under other filters it is 400. Asks `read` \
             on `quota_definition`.",
        )
        .tag(TAG)
        .authenticated()
        .no_license_required()
        .query_param_typed(
            "target_id",
            false,
            "Only this target's candidates",
            "string",
        )
        .query_param_typed(
            "state",
            false,
            "Only the candidates in this state",
            "string",
        )
        .query_param_typed(
            "limit",
            false,
            "Page size, alias of $top (default 50, at most 200)",
            "integer",
        )
        .query_param_typed(
            "cursor",
            false,
            "Continuation from page_info (alias $skiptoken)",
            "string",
        )
        .handler(list_candidates)
        .json_response_with_schema::<Page<ProductsQuotaCandidate>>(
            openapi,
            StatusCode::OK,
            "One page of candidates.",
        )
        .error_400(openapi)
        .error_401(openapi)
        .error_403(openapi)
        .error_500(openapi)
        .error_503(openapi)
        .register(router, openapi);
    OperationBuilder::get(format!("{CANDIDATES}/{{id}}"))
        .operation_id("bss_products.get_quota_candidate")
        .summary("Read a quota candidate")
        .description(
            "One candidate of the caller's tenant with the ETag of its version; 404 for an \
             unknown or foreign one. Asks `read` on `quota_definition`.",
        )
        .tag(TAG)
        .authenticated()
        .no_license_required()
        .path_param("id", "The candidate's id")
        .handler(get_candidate)
        .json_response_with_schema::<ProductsQuotaCandidate>(
            openapi,
            StatusCode::OK,
            "The candidate.",
        )
        .response_header(preconditions::etag_header())
        .error_401(openapi)
        .error_403(openapi)
        .error_404(openapi)
        .error_500(openapi)
        .error_503(openapi)
        .register(router, openapi)
}

/// A judged record's refusal on the request: its code on `record.<field>`.
fn refused(code: &'static str, field: &str, detail: String) -> CanonicalError {
    let subject = if field == "record" {
        "record".to_owned()
    } else {
        format!("record.{field}")
    };
    let mut report = ValidationReport::new();
    report.violate(code, subject, detail);
    On::DEFINITION.refuse(DomainError::Validation(report))
}

/// The stored manual candidate of the record id: the same digest answers it (200), another is
/// 409 `QUOTA_SOURCE_CONFLICT`; `None` when the target holds none.
async fn stored_manual(
    runner: &impl DBRunner,
    scope: &AccessScope,
    tenant: Uuid,
    new: &NewCandidate,
) -> Result<Option<QuotaCandidate>, TxError> {
    let Some(stored) = store::manual_candidate(runner, scope, tenant, new.target_id, new.record_id)
        .await
        .map_err(TxError::Repo)?
    else {
        return Ok(None);
    };
    if stored.record_digest != new.record_digest {
        return Err(TxError::Refused(DomainError::Conflict {
            code: QUOTA_SOURCE_CONFLICT,
            detail: format!(
                "the target holds other content under record id {}",
                new.record_id
            ),
        }));
    }
    Ok(Some(stored))
}

/// The candidate's audit row, in the create's transaction.
async fn audit(
    tx: &impl DBRunner,
    scope: &AccessScope,
    tenant: Uuid,
    candidate: &QuotaCandidate,
) -> Result<(), TxError> {
    repo::write_eventless_act_audit(
        tx,
        scope,
        AuditCommon {
            audit_id: Uuid::now_v7(),
            tenant_id: tenant,
            actor_ref: candidate.created_by,
            action: ACTION_CREATE.to_owned(),
            subject_kind: SUBJECT_CANDIDATE.to_owned(),
            reason: None,
            correlation_id: None,
            written_at: candidate.created_at,
            lifecycle: LifecycleMove::NONE,
        },
        candidate.id,
        Some(candidate.version),
    )
    .await
    .map_err(TxError::Repo)
}

/// The create's statements, inside its transaction: the stored candidate of the record id answers
/// or refuses first; else the candidate and its audit row are written. The receipt records the
/// answer when the request is keyed.
async fn write_in(
    tx: &impl DBRunner,
    scope: &AccessScope,
    tenant: Uuid,
    new: &NewCandidate,
    claim: Option<&IdempotencyClaimInput>,
) -> Result<Response, TxError> {
    if let Some(response) = replay::begin(tx, tenant, claim).await? {
        return Ok(response);
    }
    let (status, candidate) = if let Some(stored) = stored_manual(tx, scope, tenant, new).await? {
        (StatusCode::OK, stored)
    } else {
        let mut written = store::insert_candidates(tx, scope, tenant, std::slice::from_ref(new))
            .await
            .map_err(TxError::Repo)?;
        let candidate = written.pop().ok_or_else(|| {
            TxError::Repo(RepoError::Db("a manual candidate did not write".into()))
        })?;
        audit(tx, scope, tenant, &candidate).await?;
        (StatusCode::CREATED, candidate)
    };
    let body = candidate_dto(tx, scope, tenant, candidate)
        .await
        .map_err(TxError::Repo)?;
    replay::finish(tx, tenant, claim, status, &body).await
}

/// [`write_keyed`] of an unkeyed request. Public for the Postgres race suite only.
///
/// # Errors
/// As [`write_keyed`].
#[doc(hidden)]
pub async fn write_manual(
    api: &ApiState,
    scope: &AccessScope,
    tenant: Uuid,
    new: &NewCandidate,
) -> Result<Response, CanonicalError> {
    write_keyed(api, scope, tenant, new, None).await
}

/// One transaction writes a judged manual candidate, or answers the stored one of its record id
/// (200), or refuses other content under that id (409 `QUOTA_SOURCE_CONFLICT`), with the receipt of
/// a keyed request (P-D-198). A create that loses the insert race to a concurrent one of the same
/// record id (Postgres then aborts its transaction) answers the winner from a fresh transaction,
/// which records the receipt too. No external call inside.
///
/// # Errors
/// 409 `QUOTA_SOURCE_CONFLICT`, `IDEMPOTENCY_CONFLICT`, `IDEMPOTENCY_KEY_IN_FLIGHT`; storage
/// failures (500, or 409 `CONTENDED` when contention outlasts the retries).
async fn write_keyed(
    api: &ApiState,
    scope: &AccessScope,
    tenant: Uuid,
    new: &NewCandidate,
    claim: Option<&IdempotencyClaimInput>,
) -> Result<Response, CanonicalError> {
    let (scope_tx, new_tx, claim_tx) = (scope.clone(), new.clone(), claim.cloned());
    let written = api
        .db
        .db()
        .transaction_with_retry::<Response, TxError, _, _>(
            TxConfig::default(),
            contention_db_err,
            move |tx| {
                let (scope, new, claim) = (scope_tx.clone(), new_tx.clone(), claim_tx.clone());
                Box::pin(async move { write_in(tx, &scope, tenant, &new, claim.as_ref()).await })
            },
        )
        .await;
    match written {
        Err(TxError::Repo(RepoError::Refused(RepoRefusal::QuotaManualRecordTaken))) => {
            lost_race(api, scope, tenant, new, claim).await
        }
        other => other.map_err(|e| On::DEFINITION.tx(e)),
    }
}

/// A concurrent create of the record id committed first: answer the winner, or refuse other
/// content, from a fresh transaction.
async fn lost_race(
    api: &ApiState,
    scope: &AccessScope,
    tenant: Uuid,
    new: &NewCandidate,
    claim: Option<&IdempotencyClaimInput>,
) -> Result<Response, CanonicalError> {
    let (scope_tx, new_tx, claim_tx) = (scope.clone(), new.clone(), claim.cloned());
    api.db
        .db()
        .transaction_with_retry::<Response, TxError, _, _>(
            TxConfig::default(),
            contention_db_err,
            move |tx| {
                let (scope, new, claim) = (scope_tx.clone(), new_tx.clone(), claim_tx.clone());
                Box::pin(async move {
                    if let Some(response) = replay::begin(tx, tenant, claim.as_ref()).await? {
                        return Ok(response);
                    }
                    let stored =
                        stored_manual(tx, &scope, tenant, &new)
                            .await?
                            .ok_or_else(|| {
                                TxError::Refused(DomainError::Conflict {
                                    code: "CONTENDED",
                                    detail: "a concurrent create of this record did not settle; \
                                         retry"
                                        .into(),
                                })
                            })?;
                    let body = candidate_dto(tx, &scope, tenant, stored)
                        .await
                        .map_err(TxError::Repo)?;
                    replay::finish(tx, tenant, claim.as_ref(), StatusCode::OK, &body).await
                })
            },
        )
        .await
        .map_err(|e| On::DEFINITION.tx(e))
}

async fn create_candidate(
    Extension(state): Extension<Arc<QuotaState>>,
    Extension(enforcer): Extension<PolicyEnforcer>,
    extension_ctx: Option<Extension<SecurityContext>>,
    headers: HeaderMap,
    body: Result<Json<serde_json::Value>, JsonRejection>,
) -> Result<Response, CanonicalError> {
    let ctx = require_authenticated(extension_ctx)?;
    // @cpt-begin:cpt-cf-bss-products-flow-quota-catalog-intake:p1:inst-quota-manual
    let tenant = ctx.subject_tenant_id();
    let actor = ctx.subject_id();
    let scope = definition_scope(&enforcer, &ctx, actions::AUTHOR).await?;
    let payload = On::DEFINITION.body(body)?;
    let claim = On::DEFINITION.input(&state.api, &headers, CANDIDATES.into(), &payload)?;
    if let Some(response) = On::DEFINITION
        .replayed(&state.api, tenant, claim.as_ref())
        .await?
    {
        return Ok(response);
    }
    let body: ProductsQuotaCandidateRequest = On::DEFINITION.read(payload)?;
    let conn = On::DEFINITION.conn(&state.api)?;
    let target = load_target(
        &conn,
        &scope,
        tenant,
        &body.target_id.to_string(),
        On::DEFINITION,
    )
    .await?;
    let record = match judge_record(
        state.registry.as_ref(),
        &ctx,
        &body.record,
        &mut Resolutions::default(),
    )
    .await
    {
        Ok(record) => record,
        Err(JudgeError::Refused(r)) => return Err(refused(r.code, &r.field, r.detail)),
        Err(JudgeError::Unavailable(why)) => return Err(registry_unavailable(&why)),
    };
    let new = NewCandidate {
        target_id: target.id,
        origin: CandidateOrigin::Manual,
        snapshot_row_id: None,
        record_id: record.record.id,
        capability_key: record.record.capability_key.clone(),
        record_json: record.raw.clone(),
        record_digest: record.record_digest.clone(),
        evidence_json: record.evidence.to_json(),
        created_by: actor,
        created_at: stored_now(),
    };
    write_keyed(&state.api, &scope, tenant, &new, claim.as_ref()).await
    // @cpt-end:cpt-cf-bss-products-flow-quota-catalog-intake:p1:inst-quota-manual
}

/// The plain filters of the candidate list: a target id and a state, each at most once.
fn filter_of(pairs: &[(String, String)]) -> Result<CandidateFilter, CanonicalError> {
    let value = |name: &str| pairs.iter().find(|(k, _)| k == name).map(|(_, v)| v);
    let mut report = ValidationReport::new();
    let target_id = value("target_id").and_then(|raw| {
        Uuid::parse_str(raw)
            .inspect_err(|_| report.violate("VALIDATION", "target_id", "a target id is a UUID"))
            .ok()
    });
    let state = value("state").and_then(|raw| {
        let parsed = CandidateState::parse(raw);
        if parsed.is_none() {
            report.violate(
                "VALIDATION",
                "state",
                "one of staged, submitted, approved or rejected",
            );
        }
        parsed
    });
    if !report.is_empty() {
        return Err(On::DEFINITION.refuse(DomainError::Validation(report)));
    }
    Ok(CandidateFilter { target_id, state })
}

async fn list_candidates(
    Extension(state): Extension<Arc<QuotaState>>,
    Extension(enforcer): Extension<PolicyEnforcer>,
    extension_ctx: Option<Extension<SecurityContext>>,
    query: RawQuery,
    odata: Result<OData, CanonicalError>,
) -> Result<Json<Page<ProductsQuotaCandidate>>, CanonicalError> {
    let ctx = require_authenticated(extension_ctx)?;
    let scope = definition_scope(&enforcer, &ctx, actions::READ).await?;
    let pairs: Vec<(String, String)> = query
        .as_ref()
        .map(|axum::extract::Query(pairs)| pairs.clone())
        .unwrap_or_default();
    params(
        query,
        &["limit", "cursor", "target_id", "state"],
        Some(&["$top", "$skiptoken"]),
    )?;
    let filter = filter_of(&pairs)?;
    let OData(mut odata) = odata?;
    let tenant = ctx.subject_tenant_id();
    // The cursor binds the caller's tenant and both filters: reused by another tenant or under
    // another filter it is refused, not misread.
    let hash = cursor_hash(&serde_json::json!({
        "quota_candidates": {
            "tenant": tenant.to_string(),
            "target_id": filter.target_id.map(|id| id.to_string()),
            "state": filter.state.map(CandidateState::as_str),
        }
    }));
    if let Some(cursor) = &odata.cursor
        && cursor.f.as_deref() != Some(hash.as_str())
    {
        return Err(ODataError::FilterMismatch.into());
    }
    odata.filter_hash = Some(hash);
    let conn = On::DEFINITION.conn(&state.api)?;
    let page = store::list_candidates(&conn, &scope, tenant, &filter, &odata)
        .await
        .map_err(|e| On::DEFINITION.list(e))?;
    // The page's definition digests in one read.
    let mut items = candidate_dtos(&conn, &scope, tenant, page.items)
        .await
        .map_err(|e| On::DEFINITION.stored(&e))?;
    // P-D-262: the page's creators in one lookup.
    state.api.actor_names.fill(&ctx, &mut items).await;
    Ok(Json(Page {
        items,
        page_info: page.page_info,
    }))
}

async fn get_candidate(
    Extension(state): Extension<Arc<QuotaState>>,
    Extension(enforcer): Extension<PolicyEnforcer>,
    extension_ctx: Option<Extension<SecurityContext>>,
    Path(id): Path<String>,
) -> Result<Response, CanonicalError> {
    let ctx = require_authenticated(extension_ctx)?;
    let scope = definition_scope(&enforcer, &ctx, actions::READ).await?;
    let tenant = ctx.subject_tenant_id();
    let not_found = |id: Uuid| -> CanonicalError {
        DomainError::NotFound {
            what: "quota_candidate",
            id,
        }
        .into()
    };
    let Ok(id) = Uuid::parse_str(&id) else {
        return Err(not_found(Uuid::nil()));
    };
    let conn = On::DEFINITION.conn(&state.api)?;
    let candidate = store::find_candidate(&conn, &scope, tenant, id)
        .await
        .map_err(|e| On::DEFINITION.stored(&e))?
        .ok_or_else(|| not_found(id))?;
    let mut body = candidate_dto(&conn, &scope, tenant, candidate)
        .await
        .map_err(|e| On::DEFINITION.stored(&e))?;
    state.api.actor_names.fill(&ctx, &mut body).await;
    let etag = preconditions::etag(InternalRevision::new(body.version));
    let mut response = Json(body).into_response();
    if let Ok(value) = HeaderValue::from_str(&etag) {
        response.headers_mut().insert(header::ETAG, value);
    }
    Ok(response)
}
