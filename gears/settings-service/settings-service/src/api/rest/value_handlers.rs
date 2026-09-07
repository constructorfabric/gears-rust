// Created: 2026-09-07 by Constructor Tech
// @cpt-dod:cpt-cf-settings-service-dod-value-writes-operations:p1
//! Handlers of the write surface.

use std::sync::Arc;

use axum::extract::{Path, Query};
use axum::http::{HeaderMap, HeaderValue, header};
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json};
use serde::Deserialize;
use serde_json::Value;
use settings_service_sdk::SettingKey;
use toolkit::api::canonical_prelude::*;
use toolkit_canonical_errors::CanonicalError;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use crate::api::authz::{self, resource};
use crate::api::rest::setting_dto::render;
use crate::api::rest::setting_handlers::TenantParam;
use crate::api::rest::value_dto::{
    BatchRequest, BatchResultDto, CloneRequest, FallbackResultDto, SetValueRequest,
    ValidateRequest, render_batch_item, render_committed, render_impact, render_validation,
};
use crate::domain::error::DomainError;
use crate::domain::writes::{Change, WriteActor};
use crate::field;
use crate::infra::value_writes::{BatchChange, WriteCoordinator};

const READ: &str = "read";
const WRITE: &str = "write";
const READ_UNMASKED: &str = "read_unmasked";

/// The header a fresh step-up token travels in. Absent, the bearer token
/// itself is the assertion: a session re-authenticated just now carries a
/// fresh `auth_time` of its own.
pub const STEP_UP_HEADER: &str = "x-step-up-token";

fn header_str<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers.get(name).and_then(|v| v.to_str().ok())
}

fn request_id(headers: &HeaderMap) -> String {
    toolkit::api::error_layer::extract_trace_id(headers)
        .unwrap_or_else(|| Uuid::new_v4().to_string())
}

fn if_match(headers: &HeaderMap) -> Option<&str> {
    header_str(headers, "if-match").map(|v| v.trim().trim_matches('"'))
}

fn actor(ctx: &SecurityContext, headers: &HeaderMap) -> WriteActor {
    // @cpt-begin:cpt-cf-settings-service-flow-value-writes-set:p1:inst-vw-set-1
    let step_up_token = header_str(headers, STEP_UP_HEADER)
        .map(str::to_owned)
        .or_else(|| {
            ctx.bearer_token()
                .map(|t| secrecy::ExposeSecret::expose_secret(t).to_owned())
        });
    WriteActor {
        ctx: ctx.clone(),
        request_id: request_id(headers),
        step_up_token,
    }
    // @cpt-end:cpt-cf-settings-service-flow-value-writes-set:p1:inst-vw-set-1
}

fn parse_key(raw: &str) -> Result<SettingKey, DomainError> {
    SettingKey::parse(raw).map_err(|e| DomainError::Validation {
        field: "key".to_owned(),
        code: field::VALIDATION,
        message: e.to_string(),
    })
}

fn parse_tenant(raw: Option<&str>) -> Result<Option<Uuid>, DomainError> {
    match raw {
        None | Some("") => Ok(None),
        Some(raw) => Uuid::parse_str(raw)
            .map(Some)
            .map_err(|_| DomainError::Validation {
                field: "tenant".to_owned(),
                code: field::TENANT_PARAM,
                message: format!("`{raw}` is not a tenant id"),
            }),
    }
}

/// The RFC 9470 challenge: `401` telling the client what to ask the
/// provider for.
// @cpt-begin:cpt-cf-settings-service-algo-gear-foundation-authz-stepup:p1:inst-gf-authz-7
fn step_up_challenge(err: DomainError) -> Response {
    let (reason, max_age, acr) = match &err {
        DomainError::StepUpRequired {
            reason,
            max_age_seconds,
            acr_values,
        } => (*reason, *max_age_seconds, acr_values.clone()),
        _ => ("unknown", 0, Vec::new()),
    };
    let mut challenge = format!(
        "Bearer error=\"insufficient_user_authentication\", \
         error_description=\"a fresh re-authentication is required ({reason})\", \
         max_age={max_age}"
    );
    if !acr.is_empty() {
        challenge.push_str(", acr_values=\"");
        challenge.push_str(&acr.join(" "));
        challenge.push('"');
    }
    let mut response = CanonicalError::from(err).into_response();
    if let Ok(value) = HeaderValue::from_str(&challenge) {
        response
            .headers_mut()
            .insert(header::WWW_AUTHENTICATE, value);
    }
    response
}
// @cpt-end:cpt-cf-settings-service-algo-gear-foundation-authz-stepup:p1:inst-gf-authz-7

/// Map a write outcome to a response, turning a step-up refusal into its
/// challenge.
fn respond<T: IntoResponse>(outcome: Result<T, DomainError>) -> ApiResult<Response> {
    match outcome {
        Ok(body) => Ok(body.into_response()),
        Err(err @ DomainError::StepUpRequired { .. }) => Ok(step_up_challenge(err)),
        Err(err) => Err(CanonicalError::from(err)),
    }
}

async fn may_read_pii(
    enforcer: &authz_resolver_sdk::PolicyEnforcer,
    ctx: &SecurityContext,
) -> bool {
    authz::access_scope(enforcer, ctx, &resource::VALUE, READ_UNMASKED, None)
        .await
        .is_ok()
}

/// `PUT /settings-service/v1/settings/{key}/value?tenant={tenant_id}`
///
/// # Errors
/// 400 for a malformed key, `tenant` or value; 401 with the RFC 9470 challenge
/// when step-up is required and not proven; 403 when not authorized, the
/// target is outside the subtree, a service principal writes a step-up
/// declaration, or the caller's own access is not overridable; 404 when the
/// declaration is absent or hidden; 409 for a tenant-scoped write to a global
/// setting; 410 when retired; 412 or 428 on the tag; 503 when a dependency
/// cannot answer.
pub async fn set_value(
    Extension(ctx): Extension<SecurityContext>,
    Extension(writes): Extension<Arc<WriteCoordinator>>,
    Extension(enforcer): Extension<Arc<authz_resolver_sdk::PolicyEnforcer>>,
    Path(key): Path<String>,
    Query(params): Query<TenantParam>,
    headers: HeaderMap,
    Json(body): Json<SetValueRequest>,
) -> ApiResult<Response> {
    let key = parse_key(&key)?;
    let tenant = parse_tenant(params.tenant.as_deref())?;
    // @cpt-begin:cpt-cf-settings-service-algo-value-writes-gates:p1:inst-vw-gate-1
    // Authorization first, and alone: an unauthorized caller never has its
    // step-up token looked at.
    authz::access_scope(&enforcer, &ctx, &resource::VALUE, WRITE, None).await?;
    // @cpt-end:cpt-cf-settings-service-algo-value-writes-gates:p1:inst-vw-gate-1
    let actor = actor(&ctx, &headers);
    let outcome = writes
        .change(
            &actor,
            &key,
            tenant,
            Change::Set(body.value),
            if_match(&headers),
            "set",
        )
        .await;
    // @cpt-begin:cpt-cf-settings-service-flow-value-writes-set:p1:inst-vw-set-6
    let pii = may_read_pii(&enforcer, &ctx).await;
    respond(outcome.map(|committed| {
        let dto = render_committed(&committed, pii);
        ([(header::ETAG, dto.etag.clone())], Json(dto))
    }))
    // @cpt-end:cpt-cf-settings-service-flow-value-writes-set:p1:inst-vw-set-6
}

#[allow(clippy::too_many_arguments)]
async fn fall_back(
    ctx: &SecurityContext,
    writes: &WriteCoordinator,
    enforcer: &authz_resolver_sdk::PolicyEnforcer,
    key: String,
    params: TenantParam,
    headers: HeaderMap,
    change: Change,
    operation: &'static str,
) -> ApiResult<Response> {
    let key = parse_key(&key)?;
    let tenant = parse_tenant(params.tenant.as_deref())?;
    authz::access_scope(enforcer, ctx, &resource::VALUE, WRITE, None).await?;
    let actor = actor(ctx, &headers);
    let outcome = writes
        .change(&actor, &key, tenant, change, if_match(&headers), operation)
        .await;
    let committed = match outcome {
        Ok(committed) => committed,
        Err(err @ DomainError::StepUpRequired { .. }) => return Ok(step_up_challenge(err)),
        Err(err) => return Err(CanonicalError::from(err)),
    };
    // @cpt-begin:cpt-cf-settings-service-flow-value-writes-revert:p1:inst-vw-rev-3
    // @cpt-begin:cpt-cf-settings-service-flow-value-writes-revert:p1:inst-vw-rev-5
    // @cpt-begin:cpt-cf-settings-service-flow-value-writes-remove:p1:inst-vw-rm-4
    // The response carries what the scope resolves to now: the nearest
    // ancestor for a cascading tenant scope, otherwise the Schema Default,
    // which the change never touched.
    let target = crate::domain::resolution::ScopeTarget::parse(&committed.scope)?;
    let effective = writes.effective_after(&key, target).await?;
    let pii = may_read_pii(enforcer, ctx).await;
    Ok(Json(FallbackResultDto {
        change: render_committed(&committed, pii),
        effective: render(&effective, pii),
    })
    .into_response())
    // @cpt-end:cpt-cf-settings-service-flow-value-writes-remove:p1:inst-vw-rm-4
    // @cpt-end:cpt-cf-settings-service-flow-value-writes-revert:p1:inst-vw-rev-5
    // @cpt-end:cpt-cf-settings-service-flow-value-writes-revert:p1:inst-vw-rev-3
}

/// `POST /settings-service/v1/settings/{key}/value/revert?tenant={tenant_id}`
///
/// # Errors
/// As [`set_value`], plus 404 when the scope holds no override.
pub async fn revert_value(
    Extension(ctx): Extension<SecurityContext>,
    Extension(writes): Extension<Arc<WriteCoordinator>>,
    Extension(enforcer): Extension<Arc<authz_resolver_sdk::PolicyEnforcer>>,
    Path(key): Path<String>,
    Query(params): Query<TenantParam>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    // @cpt-begin:cpt-cf-settings-service-flow-value-writes-revert:p1:inst-vw-rev-1
    // @cpt-begin:cpt-cf-settings-service-flow-value-writes-revert:p1:inst-vw-rev-2
    // @cpt-begin:cpt-cf-settings-service-flow-value-writes-revert:p1:inst-vw-rev-4
    fall_back(
        &ctx,
        &writes,
        &enforcer,
        key,
        params,
        headers,
        Change::Revert,
        "revert",
    )
    .await
    // @cpt-end:cpt-cf-settings-service-flow-value-writes-revert:p1:inst-vw-rev-4
    // @cpt-end:cpt-cf-settings-service-flow-value-writes-revert:p1:inst-vw-rev-2
    // @cpt-end:cpt-cf-settings-service-flow-value-writes-revert:p1:inst-vw-rev-1
}

/// `DELETE /settings-service/v1/settings/{key}/value?tenant={tenant_id}`
///
/// # Errors
/// As [`revert_value`].
pub async fn remove_value(
    Extension(ctx): Extension<SecurityContext>,
    Extension(writes): Extension<Arc<WriteCoordinator>>,
    Extension(enforcer): Extension<Arc<authz_resolver_sdk::PolicyEnforcer>>,
    Path(key): Path<String>,
    Query(params): Query<TenantParam>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    // @cpt-begin:cpt-cf-settings-service-flow-value-writes-remove:p1:inst-vw-rm-1
    // @cpt-begin:cpt-cf-settings-service-flow-value-writes-remove:p1:inst-vw-rm-2
    // @cpt-begin:cpt-cf-settings-service-flow-value-writes-remove:p1:inst-vw-rm-3
    fall_back(
        &ctx,
        &writes,
        &enforcer,
        key,
        params,
        headers,
        Change::Remove,
        "remove",
    )
    .await
    // @cpt-end:cpt-cf-settings-service-flow-value-writes-remove:p1:inst-vw-rm-3
    // @cpt-end:cpt-cf-settings-service-flow-value-writes-remove:p1:inst-vw-rm-2
    // @cpt-end:cpt-cf-settings-service-flow-value-writes-remove:p1:inst-vw-rm-1
}

/// `POST /settings-service/v1/settings/{key}/value/clone?tenant={to}`
///
/// # Errors
/// As [`set_value`], plus 403 when the caller may not read the source and 400
/// with `secret_not_cloneable` for a secret setting.
pub async fn clone_value(
    Extension(ctx): Extension<SecurityContext>,
    Extension(writes): Extension<Arc<WriteCoordinator>>,
    Extension(enforcer): Extension<Arc<authz_resolver_sdk::PolicyEnforcer>>,
    Path(key): Path<String>,
    Query(params): Query<TenantParam>,
    headers: HeaderMap,
    Json(body): Json<CloneRequest>,
) -> ApiResult<Response> {
    // @cpt-begin:cpt-cf-settings-service-flow-value-writes-clone:p1:inst-vw-clone-1
    let key = parse_key(&key)?;
    let to = parse_tenant(params.tenant.as_deref())?;
    // @cpt-end:cpt-cf-settings-service-flow-value-writes-clone:p1:inst-vw-clone-1
    // Both ends authorized: `read` for the source, `write` for the target.
    authz::access_scope(&enforcer, &ctx, &resource::VALUE, READ, None).await?;
    authz::access_scope(&enforcer, &ctx, &resource::VALUE, WRITE, None).await?;
    let actor = actor(&ctx, &headers);
    let outcome = writes
        .clone_value(&actor, &key, body.from, to, if_match(&headers))
        .await;
    // @cpt-begin:cpt-cf-settings-service-flow-value-writes-clone:p1:inst-vw-clone-7
    let pii = may_read_pii(&enforcer, &ctx).await;
    respond(outcome.map(|committed| {
        let dto = render_committed(&committed, pii);
        ([(header::ETAG, dto.etag.clone())], Json(dto))
    }))
    // @cpt-end:cpt-cf-settings-service-flow-value-writes-clone:p1:inst-vw-clone-7
}

/// `POST /settings-service/v1/settings/batch`
///
/// # Errors
/// 400 over five hundred changes or on a malformed key; 401 with the challenge
/// when step-up is required and not proven; 403 when not authorized or a
/// service principal targets a step-up declaration. Per-change refusals are
/// entries, not errors.
pub async fn batch_set(
    Extension(ctx): Extension<SecurityContext>,
    Extension(writes): Extension<Arc<WriteCoordinator>>,
    Extension(enforcer): Extension<Arc<authz_resolver_sdk::PolicyEnforcer>>,
    headers: HeaderMap,
    Json(body): Json<BatchRequest>,
) -> ApiResult<Response> {
    // @cpt-begin:cpt-cf-settings-service-flow-value-writes-batch:p1:inst-vw-batch-1
    authz::access_scope(&enforcer, &ctx, &resource::VALUE, WRITE, None).await?;
    let actor = actor(&ctx, &headers);
    let keys: Vec<String> = body.changes.iter().map(|c| c.key.clone()).collect();
    let mut changes = Vec::with_capacity(body.changes.len());
    for change in body.changes {
        changes.push(BatchChange {
            key: parse_key(&change.key)?,
            tenant: change.tenant,
            value: change.value,
            if_match: change.if_match,
        });
    }
    // @cpt-end:cpt-cf-settings-service-flow-value-writes-batch:p1:inst-vw-batch-1
    let outcome = writes.batch(&actor, changes).await;
    // @cpt-begin:cpt-cf-settings-service-flow-value-writes-batch:p1:inst-vw-batch-9
    let pii = may_read_pii(&enforcer, &ctx).await;
    respond(outcome.map(|batch| {
        Json(BatchResultDto {
            change_set_id: batch.change_set_id,
            results: keys
                .iter()
                .zip(batch.results.iter())
                .map(|(key, result)| render_batch_item(key, result, pii))
                .collect(),
        })
    }))
    // @cpt-end:cpt-cf-settings-service-flow-value-writes-batch:p1:inst-vw-batch-9
}

/// `POST /settings-service/v1/settings/{key}/validate?tenant={tenant_id}`
///
/// # Errors
/// 400 for a malformed key or `tenant`; 403 when the caller may not read or
/// the target is outside the subtree; 404 when the declaration is absent or
/// hidden; 410 when retired; 503 when a dependency cannot answer.
pub async fn validate_value(
    Extension(ctx): Extension<SecurityContext>,
    Extension(writes): Extension<Arc<WriteCoordinator>>,
    Extension(enforcer): Extension<Arc<authz_resolver_sdk::PolicyEnforcer>>,
    Path(key): Path<String>,
    Query(params): Query<TenantParam>,
    headers: HeaderMap,
    Json(body): Json<ValidateRequest>,
) -> ApiResult<Response> {
    // @cpt-begin:cpt-cf-settings-service-flow-value-writes-validate:p1:inst-vw-val-1
    let key = parse_key(&key)?;
    let tenant = parse_tenant(params.tenant.as_deref())?;
    // @cpt-end:cpt-cf-settings-service-flow-value-writes-validate:p1:inst-vw-val-1
    // @cpt-begin:cpt-cf-settings-service-flow-value-writes-validate:p1:inst-vw-val-2
    // `read`, and no step-up: nothing is written.
    authz::access_scope(&enforcer, &ctx, &resource::VALUE, READ, None).await?;
    // @cpt-end:cpt-cf-settings-service-flow-value-writes-validate:p1:inst-vw-val-2
    let actor = actor(&ctx, &headers);
    let report = writes
        .validate(&actor, &key, tenant, &body.value, body.limit)
        .await?;
    // @cpt-begin:cpt-cf-settings-service-flow-value-writes-validate:p1:inst-vw-val-8
    let pii = may_read_pii(&enforcer, &ctx).await;
    Ok(Json(render_validation(&report, pii)).into_response())
    // @cpt-end:cpt-cf-settings-service-flow-value-writes-validate:p1:inst-vw-val-8
}

/// The candidate and page size of an impact request.
#[derive(Debug, Default, Deserialize)]
pub struct ImpactParams {
    /// The target tenant.
    #[serde(default)]
    pub tenant: Option<String>,
    /// The candidate value, as JSON.
    #[serde(default)]
    pub value: Option<String>,
    /// Page size, one to five hundred.
    #[serde(default)]
    pub limit: Option<usize>,
}

/// `GET /settings-service/v1/settings/{key}/impact?tenant={tenant_id}&value={json}&limit={n}`
///
/// # Errors
/// 400 for a malformed key, `tenant` or `value`; 403 when the caller may not
/// read or the target is outside the subtree; 404 when the declaration is
/// absent or hidden; 410 when retired.
pub async fn impact(
    Extension(ctx): Extension<SecurityContext>,
    Extension(writes): Extension<Arc<WriteCoordinator>>,
    Extension(enforcer): Extension<Arc<authz_resolver_sdk::PolicyEnforcer>>,
    Path(key): Path<String>,
    Query(params): Query<ImpactParams>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    // @cpt-begin:cpt-cf-settings-service-flow-value-writes-impact:p1:inst-vw-imp-1
    let key = parse_key(&key)?;
    let tenant = parse_tenant(params.tenant.as_deref())?;
    let candidate: Value = match params.value.as_deref() {
        None | Some("") => Value::Null,
        Some(raw) => serde_json::from_str(raw).map_err(|e| DomainError::Validation {
            field: "value".to_owned(),
            code: field::VALIDATION,
            message: format!("`value` is not JSON: {e}"),
        })?,
    };
    // @cpt-end:cpt-cf-settings-service-flow-value-writes-impact:p1:inst-vw-imp-1
    authz::access_scope(&enforcer, &ctx, &resource::VALUE, READ, None).await?;
    let actor = actor(&ctx, &headers);
    let report = writes
        .impact(&actor, &key, tenant, &candidate, params.limit)
        .await?;
    // @cpt-begin:cpt-cf-settings-service-flow-value-writes-impact:p1:inst-vw-imp-5
    let pii = may_read_pii(&enforcer, &ctx).await;
    Ok(Json(render_impact(&report, "public", pii)).into_response())
    // @cpt-end:cpt-cf-settings-service-flow-value-writes-impact:p1:inst-vw-imp-5
}
