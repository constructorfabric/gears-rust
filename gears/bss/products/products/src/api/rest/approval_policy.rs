//! Direct tenant quorum policy under SETTINGS, with atomic audit.
//!
//! P-D-205: the policy is read with a strong content `ETag` and written under `If-Match`. The
//! policy has no revision column (it is a set of `(kind, quorum)` rows), so its tag is a digest of
//! its content, as pricing's is: a write to any kind moves it, and a writer holding the tag of an
//! earlier read is refused `STALE_REVISION` instead of silently overwriting a concurrent edit.
//! @cpt-dod:cpt-cf-bss-products-dod-quorum-zero-records-unit:p1
use super::{
    ApiState, TxError, category_tx_config, contention_db_err,
    dto::{ApprovalPolicyDto, ApprovalPolicyRequest},
    governance as g, json_body,
    preconditions::{etag_header, if_match_content, if_match_param},
    require_authenticated, tx_to_canonical,
};
use crate::{
    authz::{actions, resource_types},
    domain::{
        approvals::{KIND_SKU_CHANGE, KIND_SKU_PUBLISH, KIND_SKU_RETIRE},
        canonical::{canonical_rendering, content_digest},
        concurrency::ContentTag,
        error::DomainError,
    },
    infra::storage::{RepoError, repo},
};
use authz_resolver_sdk::PolicyEnforcer;
use axum::{
    Extension, Json, Router,
    extract::{Path, rejection::JsonRejection},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
};
use std::sync::Arc;
use toolkit::api::{
    OpenApiRegistry,
    canonical_prelude::{CanonicalError, resource_error},
    operation_builder::OperationBuilder,
};
use toolkit_security::SecurityContext;
/// The policy is authorized as `approval_unit × settings`, so its refusals name that label
/// (RS-25), not the SKU's.
#[resource_error(gts_id!("cf.bss.products.approval_unit.v1~"))]
struct PolicyResource;
pub(crate) fn router(state: Arc<ApiState>, openapi: &dyn OpenApiRegistry) -> Router {
    let router = OperationBuilder::get("/bss-products/v1/approval-policy")
        .operation_id("bss_products.get_approval_policy")
        .summary("Read approval policy")
        .description(
            "Returns the tenant's default quorum and the per-kind overrides, with a content ETag \
             a following PUT sends back as If-Match (P-D-205). A caller without products \
             settings is refused (403).",
        )
        .tag("Approval policy")
        .authenticated()
        .no_license_required()
        .handler(get)
        .json_response_with_schema::<ApprovalPolicyDto>(openapi, StatusCode::OK, "Policy")
        .response_header(etag_header())
        .error_401(openapi)
        .error_403(openapi)
        .error_500(openapi)
        .error_503(openapi)
        .register(Router::new(), openapi);
    let router = OperationBuilder::put("/bss-products/v1/approval-policy")
        .operation_id("bss_products.put_approval_policy")
        .summary("Set approval policy")
        .description(
            "Sets the default quorum, or one kind's (sku_publish, sku_change, sku_retire), at the \
             policy the caller read (If-Match, P-D-205). Refusals: 403 without products settings, \
             judged first; 400 for a missing or malformed If-Match, an unknown kind or a quorum \
             past storage; 409 STALE_REVISION when the policy changed since the read.",
        )
        .tag("Approval policy")
        .authenticated()
        .no_license_required()
        .param(if_match_param())
        .json_request::<ApprovalPolicyRequest>(openapi, "Policy")
        .handler(put)
        .json_response_with_schema::<ApprovalPolicyDto>(openapi, StatusCode::OK, "Policy")
        .response_header(etag_header())
        .error_400(openapi)
        .error_401(openapi)
        .error_403(openapi)
        .error_409(openapi)
        .error_500(openapi)
        .error_503(openapi)
        .register(router, openapi);
    OperationBuilder::delete("/bss-products/v1/approval-policy/{kind}")
        .operation_id("bss_products.delete_approval_policy_override")
        .summary("Reset a kind's quorum to the default")
        .description(
            "Removes one kind's override (sku_publish, sku_change, sku_retire) at the policy the \
             caller read (If-Match), so the kind follows the default quorum again (P-D-216); \
             answers the policy with its new ETag. Refusals: 403 without products settings, \
             judged first; 400 for a missing or malformed If-Match, POLICY_DEFAULT_REQUIRED for \
             the default (*), which is never deleted, or an unknown kind; 404 when the kind has \
             no override; 409 STALE_REVISION.",
        )
        .tag("Approval policy")
        .authenticated()
        .no_license_required()
        .path_param(
            "kind",
            "Approval kind: sku_publish, sku_change or sku_retire",
        )
        .param(if_match_param())
        .handler(delete)
        .json_response_with_schema::<ApprovalPolicyDto>(openapi, StatusCode::OK, "Policy")
        .response_header(etag_header())
        .error_400(openapi)
        .error_401(openapi)
        .error_403(openapi)
        .error_404(openapi)
        .error_409(openapi)
        .error_500(openapi)
        .error_503(openapi)
        .register(router, openapi)
        .layer(Extension(state))
}
/// The strong content tag of a policy: a digest of its wire rendering, so the tag and the body a
/// client reads can never disagree.
fn policy_tag(policy: &ApprovalPolicyDto) -> Result<ContentTag, CanonicalError> {
    let value = serde_json::to_value(policy)
        .map_err(|e| CanonicalError::internal(format!("bss-products: policy tag: {e}")).create())?;
    Ok(ContentTag::of_digest(&content_digest(
        &canonical_rendering(&value),
    )))
}
/// The policy with its `ETag`.
fn answer(policy: ApprovalPolicyDto) -> Result<Response, CanonicalError> {
    let tag = policy_tag(&policy)?.to_etag();
    Ok(([(header::ETAG, tag)], Json(policy)).into_response())
}
async fn get(
    Extension(state): Extension<Arc<ApiState>>,
    Extension(enforcer): Extension<PolicyEnforcer>,
    ctx: Option<Extension<SecurityContext>>,
) -> Result<Response, CanonicalError> {
    let ctx = require_authenticated(ctx)?;
    let scope = g::settings_read(&enforcer, &ctx).await?;
    let p = repo::read_policy(
        &state.db.conn().map_err(|e| tx_to_canonical(e.into()))?,
        &scope,
        ctx.subject_tenant_id(),
    )
    .await
    .map_err(|e| tx_to_canonical(TxError::Repo(e)))?;
    answer(ApprovalPolicyDto::from(p))
}
/// @cpt-cf-bss-products-fr-approval-units
async fn put(
    Extension(state): Extension<Arc<ApiState>>,
    Extension(enforcer): Extension<PolicyEnforcer>,
    ctx: Option<Extension<SecurityContext>>,
    headers: HeaderMap,
    body: Result<Json<ApprovalPolicyRequest>, JsonRejection>,
) -> Result<Response, CanonicalError> {
    let ctx = require_authenticated(ctx)?;
    // Authorization first, then the precondition, then the body (as pricing's doors do).
    let scope = g::scope(
        &enforcer,
        &ctx,
        &resource_types::APPROVAL_UNIT,
        actions::SETTINGS,
    )
    .await?;
    let expected = if_match_content(&headers)?;
    let p = json_body(body)?;
    let kind = p.kind.unwrap_or_else(|| "*".into());
    if p.quorum > i32::MAX.cast_unsigned()
        || !matches!(
            kind.as_str(),
            "*" | KIND_SKU_PUBLISH | KIND_SKU_CHANGE | KIND_SKU_RETIRE
        )
    {
        return Err(g::validation("policy", "unknown kind or quorum exceeds storage range").into());
    }
    let policy = state
        .db
        .db()
        .transaction_with_retry(category_tx_config(&state), contention_db_err, move |tx| {
            let scope = scope.clone();
            let ctx = ctx.clone();
            let kind = kind.clone();
            Box::pin(async move {
                // The comparison reads the policy under the write, in its transaction.
                let current = ApprovalPolicyDto::from(
                    repo::read_policy(tx, &scope, ctx.subject_tenant_id())
                        .await
                        .map_err(TxError::Repo)?,
                );
                let tag = policy_tag(&current).map_err(|e| {
                    TxError::Repo(RepoError::Db(format!(
                        "the policy's content tag: {}",
                        e.diagnostic().unwrap_or(e.detail())
                    )))
                })?;
                if tag != expected {
                    return Err(stale_tag());
                }
                repo::write_policy(tx, &scope, ctx.subject_tenant_id(), &kind, p.quorum)
                    .await
                    .map_err(TxError::Repo)?;
                g::audit(
                    tx,
                    &ctx,
                    "approval_policy.write",
                    "approval_policy",
                    ctx.subject_tenant_id(),
                    None,
                    crate::infra::storage::stored_now(),
                    repo::LifecycleMove::NONE,
                )
                .await?;
                repo::read_policy(tx, &scope, ctx.subject_tenant_id())
                    .await
                    .map_err(TxError::Repo)
            })
        })
        .await
        .map_err(tx_to_canonical)?;
    answer(ApprovalPolicyDto::from(policy))
}
/// P-D-216: `DELETE /approval-policy/{kind}` removes one kind's override at the policy the caller
/// read, so the kind follows the default again. The default (`*`) is never deleted: a tenant always
/// has a quorum to fall back to. Authorization first, then the precondition, then the kind.
async fn delete(
    Extension(state): Extension<Arc<ApiState>>,
    Extension(enforcer): Extension<PolicyEnforcer>,
    ctx: Option<Extension<SecurityContext>>,
    Path(kind): Path<String>,
    headers: HeaderMap,
) -> Result<Response, CanonicalError> {
    let ctx = require_authenticated(ctx)?;
    let scope = g::scope(
        &enforcer,
        &ctx,
        &resource_types::APPROVAL_UNIT,
        actions::SETTINGS,
    )
    .await?;
    let expected = if_match_content(&headers)?;
    if kind == "*" {
        let mut report = crate::domain::validation::ValidationReport::new();
        report.violate(
            "POLICY_DEFAULT_REQUIRED",
            "kind",
            "the default quorum is never deleted; set it with PUT /approval-policy",
        );
        return Err(DomainError::Validation(report).into());
    }
    if !matches!(
        kind.as_str(),
        KIND_SKU_PUBLISH | KIND_SKU_CHANGE | KIND_SKU_RETIRE
    ) {
        return Err(g::validation("kind", "unknown kind").into());
    }
    let reset_kind = kind.clone();
    let policy = state
        .db
        .db()
        .transaction_with_retry(category_tx_config(&state), contention_db_err, move |tx| {
            let scope = scope.clone();
            let ctx = ctx.clone();
            let kind = reset_kind.clone();
            Box::pin(async move {
                let tenant = ctx.subject_tenant_id();
                let current = repo::read_policy(tx, &scope, tenant)
                    .await
                    .map_err(TxError::Repo)?;
                let tag = policy_tag(&ApprovalPolicyDto::from(current.clone())).map_err(|e| {
                    TxError::Repo(RepoError::Db(format!(
                        "the policy's content tag: {}",
                        e.diagnostic().unwrap_or(e.detail())
                    )))
                })?;
                if tag != expected {
                    return Err(stale_tag());
                }
                if !current.overrides.contains_key(&kind)
                    || repo::delete_policy(tx, &scope, tenant, &kind)
                        .await
                        .map_err(TxError::Repo)?
                        == 0
                {
                    return Ok(None);
                }
                g::audit(
                    tx,
                    &ctx,
                    "approval_policy.reset",
                    "approval_policy",
                    tenant,
                    None,
                    crate::infra::storage::stored_now(),
                    repo::LifecycleMove::NONE,
                )
                .await?;
                repo::read_policy(tx, &scope, tenant)
                    .await
                    .map(Some)
                    .map_err(TxError::Repo)
            })
        })
        .await
        .map_err(tx_to_canonical)?;
    policy.map_or_else(
        || {
            Err(
                PolicyResource::not_found(format!("the tenant has no {kind} override"))
                    .with_resource(kind.clone())
                    .create(),
            )
        },
        |p| answer(ApprovalPolicyDto::from(p)),
    )
}
/// The policy changed since the caller's read.
fn stale_tag() -> TxError {
    TxError::Refused(DomainError::Conflict {
        code: "STALE_REVISION",
        detail: "the approval policy changed since it was read; read it again".into(),
    })
}
#[cfg(test)]
#[path = "approval_policy_tests.rs"]
mod approval_policy_tests;
