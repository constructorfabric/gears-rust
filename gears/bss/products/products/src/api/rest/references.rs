//! @cpt-dod:cpt-cf-bss-products-dod-reserve-refused-when-fenced:p1
//! Owner-bound reservations and explicit, audited operator release.
//! @cpt-dod:cpt-cf-bss-products-dod-reference-registry:p1
use super::{
    ApiState, TxError, category_tx_config, contention_db_err,
    dto::{ReferenceReceipt, ReleaseRequest, ReserveRequest},
    governance as g, json_body, replay, repo_error_to_canonical, require_authenticated,
    tx_to_canonical,
};
use crate::{
    authz::{actions, resource_types},
    domain::{
        error::DomainError,
        references::{RefKind, reservation_allowed},
    },
    infra::{
        broker, events,
        storage::{RepoError, RepoRefusal, repo},
    },
};
use authz_resolver_sdk::PolicyEnforcer;
use axum::{
    Extension, Json, Router,
    extract::{Path, rejection::JsonRejection},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use std::sync::Arc;
use toolkit::api::{
    OpenApiRegistry, canonical_prelude::CanonicalError, operation_builder::OperationBuilder,
};
use toolkit_security::SecurityContext;
use uuid::Uuid;
pub(crate) fn router(state: Arc<ApiState>, openapi: &dyn OpenApiRegistry) -> Router {
    let router = Router::new();
    let router = OperationBuilder::post("/bss-products/v1/skus/{id}/references/reserve")
        .operation_id("bss_products.reserve_reference")
        .summary("reserve_reference")
        .tag("References")
        .authenticated()
        .no_license_required()
        .path_param("id", "Resource id")
        .json_request::<ReserveRequest>(openapi, "Request")
        .param(replay::param())
        .handler(reserve)
        .json_response_with_schema::<ReferenceReceipt>(openapi, StatusCode::OK, "Reference")
        .json_response_with_schema::<ReferenceReceipt>(
            openapi,
            StatusCode::CREATED,
            "New reservation",
        )
        .error_400(openapi)
        .error_401(openapi)
        .error_403(openapi)
        .error_404(openapi)
        .error_409(openapi)
        .error_500(openapi)
        .error_503(openapi)
        .register(router, openapi);
    let router = OperationBuilder::post("/bss-products/v1/references/{id}/confirm")
        .operation_id("bss_products.confirm_reference")
        .summary("confirm_reference")
        .tag("References")
        .authenticated()
        .no_license_required()
        .path_param("id", "Resource id")
        .param(replay::param())
        .handler(confirm)
        .json_response_with_schema::<ReferenceReceipt>(openapi, StatusCode::OK, "Reference")
        .error_400(openapi)
        .error_401(openapi)
        .error_403(openapi)
        .error_404(openapi)
        .error_409(openapi)
        .error_500(openapi)
        .error_503(openapi)
        .register(router, openapi);
    let router = OperationBuilder::delete("/bss-products/v1/references/{id}")
        .operation_id("bss_products.release_reference")
        .summary("release_reference")
        .description(
            "Releases a reference: its owner gear releases its own, and an operator forces the \
             release with `force: true` and a reason of at most 2000 characters (P-D-225), which \
             the audit row and the `ReferenceForceReleased` event carry. Refusals: 400 for a \
             forced release without a reason, 400 FIELD_TOO_LONG on a reason over its cap; 403 \
             REFERENCE_OWNER_MISMATCH; 404.",
        )
        .tag("References")
        .authenticated()
        .no_license_required()
        .path_param("id", "Resource id")
        .json_request::<ReleaseRequest>(openapi, "Request")
        .handler(release)
        .json_response_with_schema::<ReferenceReceipt>(openapi, StatusCode::OK, "Reference")
        .error_400(openapi)
        .error_401(openapi)
        .error_403(openapi)
        .error_404(openapi)
        .error_409(openapi)
        .error_500(openapi)
        .error_503(openapi)
        .register(router, openapi);
    router.layer(Extension(state))
}
fn owner<'a>(state: &'a ApiState, ctx: &SecurityContext) -> Option<&'a str> {
    state
        .reference_principals
        .get(&ctx.subject_id())
        .map(String::as_str)
}
/// The owner-binding refusal, 403 `REFERENCE_OWNER_MISMATCH`, logged once where it is decided
/// (RS-20), target `bss_products.authz.deny` as the PDP's denials: `subject` is the principal
/// refused and `why` what did not match.
pub(crate) fn forbidden(subject: Uuid, tenant: Uuid, why: &str) -> DomainError {
    tracing::warn!(
        target: "bss_products.authz.deny",
        subject_id = %subject,
        subject_tenant_id = %tenant,
        reason = "REFERENCE_OWNER_MISMATCH",
        why,
        "bss-products: reference owner refused"
    );
    DomainError::Forbidden {
        code: "REFERENCE_OWNER_MISMATCH",
        detail: "principal is not the registered owner gear".into(),
    }
}
/// @cpt-cf-bss-products-fr-reference-registry
async fn reserve(
    Extension(state): Extension<Arc<ApiState>>,
    Extension(enforcer): Extension<PolicyEnforcer>,
    ctx: Option<Extension<SecurityContext>>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    body: Result<Json<serde_json::Value>, JsonRejection>,
) -> Result<Response, CanonicalError> {
    let ctx = require_authenticated(ctx)?;
    let scope = g::scope(&enforcer, &ctx, &resource_types::SKU, actions::REFERENCE).await?;
    let payload = json_body(body)?;
    let claim = replay::input(
        &state,
        &headers,
        format!("/bss-products/v1/skus/{id}/references/reserve"),
        &payload,
    )?;
    let body: ReserveRequest = serde_json::from_value(payload)
        .map_err(|e| CanonicalError::from(g::validation("body", e.to_string())))?;
    if owner(&state, &ctx) != Some(body.owner.as_str()) {
        return Err(forbidden(
            ctx.subject_id(),
            ctx.subject_tenant_id(),
            "the principal is not the registered owner the body names",
        )
        .into());
    }
    let kind = match body.kind.as_str() {
        "price_book_entry" => RefKind::PriceBookEntry,
        "plan_item" => RefKind::PlanItem,
        "sold_as" => RefKind::SoldAs,
        _ => return Err(g::validation("kind", "unknown reference kind").into()),
    };
    let db = state.db.db();
    // The one setting the attempts read, copied once rather than an `Arc<ApiState>` per attempt
    // (RS-55).
    let ttl = state.fence_ttl_minutes;
    // A unique loser rolls back before retrying the logical-reference read. A second loss is the
    // 409 after the loop, never the repository's 500 (RS-04).
    for _ in 0..2 {
        let scope_tx = scope.clone();
        let ctx_tx = ctx.clone();
        let claim_tx = claim.clone();
        let owner_tx = body.owner.clone();
        let result = db
            .transaction_with_retry(category_tx_config(&state), contention_db_err, move |tx| {
                let scope = scope_tx.clone();
                let ctx = ctx_tx.clone();
                let claim = claim_tx.clone();
                let owner = owner_tx.clone();
                Box::pin(async move {
                    let tenant = ctx.subject_tenant_id();
                    g::find(tx, &scope, tenant, id).await?;
                    if let Some(response) = replay::begin(tx, tenant, claim.as_ref()).await? {
                        return Ok(response);
                    }
                    let (row, created) = reserve_tx(
                        tx,
                        &scope,
                        Acting::subject(&ctx),
                        &owner,
                        id,
                        kind,
                        body.ref_id,
                        ttl,
                    )
                    .await?;
                    replay::finish(
                        tx,
                        tenant,
                        claim.as_ref(),
                        if created {
                            StatusCode::CREATED
                        } else {
                            StatusCode::OK
                        },
                        &ReferenceReceipt::try_from(row).map_err(TxError::Repo)?,
                    )
                    .await
                })
            })
            .await;
        match result {
            Err(TxError::Repo(RepoError::Refused(RepoRefusal::ReferenceExists))) => {}
            other => return other.map_err(tx_to_canonical),
        }
    }
    Err(tx_to_canonical(g::conflict(
        "REFERENCE_EXISTS",
        "logical reference changed; retry",
    )))
}
async fn confirm(
    Extension(state): Extension<Arc<ApiState>>,
    Extension(enforcer): Extension<PolicyEnforcer>,
    ctx: Option<Extension<SecurityContext>>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
) -> Result<Response, CanonicalError> {
    let ctx = require_authenticated(ctx)?;
    let scope = g::scope(&enforcer, &ctx, &resource_types::SKU, actions::REFERENCE).await?;
    let owner = owner(&state, &ctx)
        .ok_or_else(|| {
            CanonicalError::from(forbidden(
                ctx.subject_id(),
                ctx.subject_tenant_id(),
                "the principal owns no references",
            ))
        })?
        .to_owned();
    let claim = replay::input(
        &state,
        &headers,
        format!("/bss-products/v1/references/{id}/confirm"),
        &serde_json::json!({}),
    )?;
    let ttl = state.fence_ttl_minutes;
    let row = state
        .db
        .db()
        .transaction_with_retry(category_tx_config(&state), contention_db_err, move |tx| {
            let scope = scope.clone();
            let ctx = ctx.clone();
            let owner = owner.clone();
            let claim = claim.clone();
            Box::pin(async move {
                let tenant = ctx.subject_tenant_id();
                let row = repo::find_reference(tx, &scope, tenant, id)
                    .await
                    .map_err(TxError::Repo)?
                    .ok_or(TxError::Refused(DomainError::NotFound {
                        what: "reference",
                        id,
                    }))?;

                if row.owner_gear != owner {
                    return Err(TxError::Refused(forbidden(
                        ctx.subject_id(),
                        tenant,
                        "the reference belongs to another owner",
                    )));
                }
                if let Some(response) = replay::begin(tx, tenant, claim.as_ref()).await? {
                    return Ok(response);
                }
                let row = confirm_tx(tx, &scope, Acting::subject(&ctx), &owner, id, ttl).await?;
                replay::finish(
                    tx,
                    tenant,
                    claim.as_ref(),
                    StatusCode::OK,
                    &ReferenceReceipt::try_from(row).map_err(TxError::Repo)?,
                )
                .await
            })
        })
        .await
        .map_err(tx_to_canonical)?;
    Ok(row)
}
/// @cpt-cf-bss-products-fr-reference-registry
async fn release(
    Extension(state): Extension<Arc<ApiState>>,
    Extension(enforcer): Extension<PolicyEnforcer>,
    ctx: Option<Extension<SecurityContext>>,
    Path(id): Path<Uuid>,
    body: Result<Json<ReleaseRequest>, JsonRejection>,
) -> Result<Response, CanonicalError> {
    let ctx = require_authenticated(ctx)?;
    // Authorization precedes body/row disclosure; force explicitly selects the operator route.
    let principal_owner = owner(&state, &ctx).map(str::to_owned);
    let scope = g::scope(
        &enforcer,
        &ctx,
        &resource_types::SKU,
        if principal_owner.is_some() {
            actions::REFERENCE
        } else {
            actions::SUBMIT
        },
    )
    .await?;
    let body = json_body(body)?;
    let forced = principal_owner.is_none() || body.force;
    let scope = if body.force && principal_owner.is_some() {
        g::scope(&enforcer, &ctx, &resource_types::SKU, actions::SUBMIT).await?
    } else {
        scope
    };
    if forced && (!body.force || body.reason.as_deref().is_none_or(|s| s.trim().is_empty())) {
        return Err(g::validation(
            "force",
            "operator release requires force and a nonempty reason",
        )
        .into());
    }
    // The operator's reason goes into the audit row and the `ReferenceForceReleased` event as
    // sent: at most 2000 characters (P-D-225).
    let mut report = crate::domain::validation::ValidationReport::new();
    crate::domain::caps::check(
        &mut report,
        "reason",
        body.reason.as_deref(),
        crate::domain::caps::NOTE_MAX_CHARS,
    );
    if !report.is_empty() {
        return Err(DomainError::Validation(report).into());
    }
    let (db, sink, config) = (
        state.db.db(),
        state.sink.clone(),
        category_tx_config(&state),
    );
    let ttl = state.fence_ttl_minutes;
    let row = events::transaction(&db, &sink, config, contention_db_err, move |tx, outbox| {
        let scope = scope.clone();
        let ctx = ctx.clone();
        let principal_owner = principal_owner.clone();
        let reason = body.reason.clone();
        Box::pin(async move {
            let tenant = ctx.subject_tenant_id();
            if !forced {
                return release_tx(
                    tx,
                    &scope,
                    Acting::subject(&ctx),
                    principal_owner.as_deref().unwrap_or_default(),
                    id,
                    ttl,
                )
                .await;
            }
            let now = crate::infra::storage::stored_now();
            let row = repo::find_reference(tx, &scope, tenant, id)
                .await
                .map_err(TxError::Repo)?
                .ok_or(TxError::Refused(DomainError::NotFound {
                    what: "reference",
                    id,
                }))?;
            g::expire(tx, &scope, tenant, row.sku_id, ttl, now).await?;
            if !forced && principal_owner.as_deref() != Some(row.owner_gear.as_str()) {
                return Err(TxError::Refused(forbidden(
                    ctx.subject_id(),
                    tenant,
                    "the reference belongs to another owner",
                )));
            }
            let released = repo::release_reference(
                tx,
                &scope,
                tenant,
                id,
                ctx.subject_id(),
                reason.as_deref(),
                forced,
                now,
            )
            .await
            .map_err(TxError::Repo)?;
            let repo::HeadWrite::Written(row) = released else {
                return Ok(row);
            };
            g::audit(
                tx,
                &ctx,
                if forced {
                    "reference.force_release"
                } else {
                    "reference.release"
                },
                "sku_reference",
                id,
                reason.clone(),
                now,
                repo::LifecycleMove::NONE,
            )
            .await?;
            if forced {
                events::enqueue_typed(
                    &outbox,
                    tx,
                    broker::ReferenceForceReleased {
                        tenant_id: tenant,
                        sku_id: row.sku_id,
                        reference_id: id,
                        owner: row.owner_gear.clone(),
                        kind: row.ref_kind.clone(),
                        ref_id: row.ref_id,
                        actor_ref: ctx.subject_id(),
                        reason: reason.unwrap_or_default(),
                    },
                )
                .await
                .map_err(TxError::from)?;
            }
            Ok(row)
        })
    })
    .await
    .map_err(tx_to_canonical)?;
    let receipt = ReferenceReceipt::try_from(row).map_err(|e| repo_error_to_canonical(&e))?;
    Ok(Json(receipt).into_response())
}

// Shared transaction operations: REST adds replay envelopes, local callers add owner binding.
use crate::infra::storage::entity::sku_reference;
use toolkit_db::secure::{AccessScope, DBRunner};

/// Who acts on a reference: the caller's context, and whether the in-process registry trusted it as
/// the pricing system actor (P-D-222). A REST door always acts as a subject, whatever subject type
/// its token asserts; only the registry records the system's act on the audit row.
#[derive(Clone, Copy)]
pub(crate) struct Acting<'a> {
    pub(crate) ctx: &'a SecurityContext,
    pub(crate) system: bool,
}
impl<'a> Acting<'a> {
    /// A subject the PDP authorized: every REST door, and a registry caller that is not the
    /// pricing system actor.
    pub(crate) const fn subject(ctx: &'a SecurityContext) -> Self {
        Self { ctx, system: false }
    }
}
#[expect(
    clippy::too_many_arguments,
    reason = "Explicit reservation identity and transaction context"
)]
pub(crate) async fn reserve_tx(
    tx: &impl DBRunner,
    scope: &AccessScope,
    acting: Acting<'_>,
    owner: &str,
    id: Uuid,
    kind: RefKind,
    ref_id: Uuid,
    ttl: u32,
) -> Result<(sku_reference::Model, bool), TxError> {
    let ctx = acting.ctx;
    let tenant = ctx.subject_tenant_id();
    g::find(tx, scope, tenant, id).await?;
    let scope = AccessScope::for_tenant(tenant);
    let now = crate::infra::storage::stored_now();
    g::expire(tx, &scope, tenant, id, ttl, now).await?;
    if let Some(row) = repo::find_live_reference(tx, &scope, tenant, owner, kind, ref_id)
        .await
        .map_err(TxError::Repo)?
    {
        if row.sku_id != id {
            return Err(g::conflict(
                "REFERENCE_EXISTS",
                "logical reference already belongs to another SKU",
            ));
        }
        return Ok((row, false));
    }
    let s = g::find(tx, &scope, tenant, id).await?;
    // A retire under review refuses a new reservation as a fence does (P-D-248): SKU_FENCED.
    reservation_allowed(s.lifecycle, s.type_change_pending || s.retire_pending)
        .map_err(TxError::Refused)?;
    let row = repo::reserve_reference(
        tx,
        &scope,
        tenant,
        id,
        owner,
        kind,
        ref_id,
        ctx.subject_id(),
        now,
    )
    .await
    .map_err(TxError::Repo)?;
    reference_audit(tx, acting, owner, "reference.reserve", row.id, now).await?;
    Ok((row, true))
}
/// The reference `id` of the caller's tenant, refused unless `owner` holds it.
pub(crate) async fn owned(
    tx: &impl DBRunner,
    scope: &AccessScope,
    ctx: &SecurityContext,
    owner: &str,
    id: Uuid,
) -> Result<sku_reference::Model, TxError> {
    let tenant = ctx.subject_tenant_id();
    let row = repo::find_reference(tx, scope, tenant, id)
        .await
        .map_err(TxError::Repo)?
        .ok_or(TxError::Refused(DomainError::NotFound {
            what: "reference",
            id,
        }))?;
    if row.owner_gear != owner {
        return Err(TxError::Refused(forbidden(
            ctx.subject_id(),
            tenant,
            "the reference belongs to another owner",
        )));
    }
    Ok(row)
}
pub(crate) async fn confirm_tx(
    tx: &impl DBRunner,
    scope: &AccessScope,
    acting: Acting<'_>,
    owner: &str,
    id: Uuid,
    ttl: u32,
) -> Result<sku_reference::Model, TxError> {
    let tenant = acting.ctx.subject_tenant_id();
    let row = owned(tx, scope, acting.ctx, owner, id).await?;
    let scope = AccessScope::for_tenant(tenant);
    let now = crate::infra::storage::stored_now();
    g::expire(tx, &scope, tenant, row.sku_id, ttl, now).await?;
    match repo::confirm_reference(tx, &scope, tenant, id, now)
        .await
        .map_err(TxError::Repo)?
    {
        repo::ConfirmOutcome::Released => {
            return Err(g::conflict(
                "REFERENCE_RELEASED",
                "released attempts cannot be confirmed",
            ));
        }
        repo::ConfirmOutcome::Missing => {
            return Err(TxError::Refused(DomainError::NotFound {
                what: "reference",
                id,
            }));
        }
        repo::ConfirmOutcome::Confirmed => {
            reference_audit(tx, acting, owner, "reference.confirm", id, now).await?;
        }
        repo::ConfirmOutcome::AlreadyConfirmed => {}
    }
    owned(tx, &scope, acting.ctx, owner, id).await
}
pub(crate) async fn release_tx(
    tx: &impl DBRunner,
    scope: &AccessScope,
    acting: Acting<'_>,
    owner: &str,
    id: Uuid,
    ttl: u32,
) -> Result<sku_reference::Model, TxError> {
    let ctx = acting.ctx;
    let tenant = ctx.subject_tenant_id();
    let row = owned(tx, scope, ctx, owner, id).await?;
    let now = crate::infra::storage::stored_now();
    g::expire(tx, scope, tenant, row.sku_id, ttl, now).await?;
    if let repo::HeadWrite::Written(row) =
        repo::release_reference(tx, scope, tenant, id, ctx.subject_id(), None, false, now)
            .await
            .map_err(TxError::Repo)?
    {
        reference_audit(tx, acting, owner, "reference.release", id, now).await?;
        return Ok(row);
    }
    Ok(row)
}
async fn reference_audit(
    tx: &impl DBRunner,
    acting: Acting<'_>,
    owner: &str,
    action: &str,
    id: Uuid,
    now: time::OffsetDateTime,
) -> Result<(), TxError> {
    let ctx = acting.ctx;
    let actor_kind = if acting.system { "system" } else { "subject" };
    g::audit(
        tx,
        ctx,
        action,
        "sku_reference",
        id,
        Some(format!("owner={owner}; actor_kind={actor_kind}")),
        now,
        repo::LifecycleMove::NONE,
    )
    .await
}
