//! Axum handler + router for payer-closure (VHP-1852 Phase 2).
//! `POST /bss-ledger/v1/payers/{payer_tenant_id}/close` — set the payer's
//! lifecycle to CLOSED for the caller's own tenant. Closing a payer that still
//! holds a balance routes through dual-control (409 `DUAL_CONTROL_REQUIRED` →
//! approve → executor writes CLOSED); a clean payer closes inline (200).

use std::sync::Arc;

use axum::extract::{Extension, Path};
use axum::response::{IntoResponse, Response};
use axum::{Json, Router, http::StatusCode};
use toolkit::api::canonical_prelude::CanonicalError;
use toolkit::api::{OpenApiRegistry, operation_builder::OperationBuilder};
use toolkit_security::SecurityContext;
use uuid::Uuid;

use crate::api::rest::auth_context::require_authenticated;
use crate::api::rest::canonical_json::CanonicalJson;
use crate::api::rest::dto::PayerStateView;
use crate::api::rest::error::{authz_error_to_canonical, payer_state_not_found};
use crate::domain::approval::ApprovalKind;
use crate::domain::approval::intent::{ApprovalIntent, PayerClosureIntent};
use crate::domain::approval::policy::OperationFacts;
use crate::domain::error::DomainError;
use crate::domain::status::PAYER_LIFECYCLE_CLOSED;
use crate::infra::approval::service::ApprovalService;
use crate::infra::storage::repo::PayerStateRepo;

/// `OpenAPI` tag applied to the payer operations.
const TAG: &str = "BSS Ledger Payers";

/// Shared per-request state for the payer routes.
#[derive(Clone)]
pub struct ApiState {
    /// Dual-control engine (closure-with-balance routes through it).
    pub approval: Arc<ApprovalService>,
    /// Payer lifecycle + balance reads/writes.
    pub payer_state: PayerStateRepo,
}

/// Close-payer request body.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(request)]
pub struct ClosePayerRequest {
    /// Customer-balance disposition election — recorded when closing with a
    /// positive balance (design 01 §4.2).
    pub disposition: Option<String>,
}

/// Close-payer response (inline-close path).
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(response)]
pub struct ClosePayerResponse {
    pub payer_tenant_id: Uuid,
    pub lifecycle_state: String,
    pub closed_with_open_balance: bool,
}

/// Build the Axum router for the payer surface.
pub fn router(state: Arc<ApiState>, openapi: &dyn OpenApiRegistry) -> Router {
    let mut router = Router::new();
    router = OperationBuilder::post("/bss-ledger/v1/payers/{payer_tenant_id}/close")
        .operation_id("bss_ledger.close_payer")
        .summary("Close a payer's ledger lifecycle")
        .description(
            "Sets the payer lifecycle to CLOSED for the caller's tenant. Closing a \
             payer that still holds a balance routes through dual-control \
             (409 DUAL_CONTROL_REQUIRED → approve); a clean payer closes inline (200).",
        )
        .tag(TAG)
        .authenticated()
        .no_license_required()
        .path_param("payer_tenant_id", "The payer being closed.")
        .json_request::<ClosePayerRequest>(openapi, "Optional customer-balance disposition.")
        .handler(close_payer)
        .json_response_with_schema::<ClosePayerResponse>(
            openapi,
            StatusCode::OK,
            "The payer was closed inline (no outstanding balance).",
        )
        .error_400(openapi)
        .error_401(openapi)
        .error_403(openapi)
        .error_500(openapi)
        .register(router, openapi);

    router = OperationBuilder::get("/bss-ledger/v1/payers/{payer_tenant_id}/state")
        .operation_id("bss_ledger.get_payer_state")
        .summary("Read a payer's ledger lifecycle state")
        .description(
            "Returns the payer's ledger lifecycle state (`OPEN` / `CLOSED`) for the \
             caller's seller tenant, plus whether a close was approved over an \
             outstanding balance. Tenant-scoped (SQL-level BOLA): a payer with no \
             recorded state — or outside the caller's authorized subtree — yields a \
             404 (no existence leak).",
        )
        .tag(TAG)
        .authenticated()
        .no_license_required()
        .path_param(
            "payer_tenant_id",
            "The payer whose lifecycle state to read.",
        )
        .handler(get_payer_state)
        .json_response_with_schema::<PayerStateView>(
            openapi,
            StatusCode::OK,
            "The payer's lifecycle state.",
        )
        .error_401(openapi)
        .error_403(openapi)
        .error_404(openapi)
        .error_500(openapi)
        .register(router, openapi);

    router.layer(Extension(state))
}

/// `GET /payers/{payer_tenant_id}/state` (read-surface): read one payer's ledger
/// lifecycle state for the caller's seller tenant. Gates on `(entry, read)` — the
/// data-plane read action the refund / note / dispute reads use — and binds the
/// compiled scope as the SQL-level BOLA filter, so a payer outside the caller's
/// subtree resolves to `None` ⇒ 404 (no existence leak). The seller tenant is the
/// caller's own (`ctx.subject_tenant_id()`, mirroring `close_payer`).
async fn get_payer_state(
    Extension(state): Extension<Arc<ApiState>>,
    Extension(enforcer): Extension<authz_resolver_sdk::PolicyEnforcer>,
    extension_ctx: Option<Extension<SecurityContext>>,
    Path(payer_tenant_id): Path<Uuid>,
) -> Result<Json<PayerStateView>, CanonicalError> {
    let ctx = require_authenticated(extension_ctx)?;
    let tenant_id = ctx.subject_tenant_id();
    let scope = crate::authz::access_scope(
        &enforcer,
        &ctx,
        &crate::authz::resource_types::ENTRY,
        crate::authz::actions::READ,
        Some(tenant_id),
        None,
        /* require_constraints */ true,
    )
    .await
    .map_err(authz_error_to_canonical)?;

    let row = state
        .payer_state
        .read(&scope, tenant_id, payer_tenant_id)
        .await?
        .ok_or_else(|| payer_state_not_found(payer_tenant_id))?;
    Ok(Json(PayerStateView::from(row)))
}

async fn close_payer(
    Extension(state): Extension<Arc<ApiState>>,
    Extension(enforcer): Extension<authz_resolver_sdk::PolicyEnforcer>,
    extension_ctx: Option<Extension<SecurityContext>>,
    Path(payer_tenant_id): Path<Uuid>,
    CanonicalJson(body): CanonicalJson<ClosePayerRequest>,
) -> Result<Response, CanonicalError> {
    let ctx = require_authenticated(extension_ctx)?;
    let tenant_id = ctx.subject_tenant_id();
    // Closer needs data-plane write authority. MVP reuses the entry-post
    // permission; a dedicated payer-close permission is a follow-up.
    let scope = crate::authz::access_scope(
        &enforcer,
        &ctx,
        &crate::authz::resource_types::ENTRY,
        crate::authz::actions::POST,
        Some(tenant_id),
        None,
        /* require_constraints */ true,
    )
    .await
    .map_err(authz_error_to_canonical)?;

    let has_balance = state
        .payer_state
        .has_outstanding_balance(&scope, tenant_id, payer_tenant_id)
        .await
        .map_err(CanonicalError::from)?;

    // Dual-control gate: closing WITH a balance needs sign-off; a clean payer
    // closes inline.
    let intent = ApprovalIntent::PayerClosure(PayerClosureIntent {
        tenant_id,
        payer_tenant_id,
        closed_with_open_balance: has_balance,
        disposition: body.disposition.clone(),
    });
    let facts = OperationFacts {
        kind: ApprovalKind::PayerClosure,
        amount_usd_eq_minor: None,
        effective_at: None,
        has_outstanding_balance: has_balance,
    };
    if let Some(approval_id) = state
        .approval
        .gate(&ctx, &scope, intent, facts, "payer-closure".to_owned())
        .await
        .map_err(CanonicalError::from)?
    {
        return Err(CanonicalError::from(DomainError::DualControlRequired(
            format!("payer closure requires dual-control approval: {approval_id}"),
        )));
    }

    state
        .payer_state
        .close(
            &scope,
            tenant_id,
            payer_tenant_id,
            ctx.subject_id(),
            has_balance,
        )
        .await
        .map_err(CanonicalError::from)?;
    Ok((
        StatusCode::OK,
        Json(ClosePayerResponse {
            payer_tenant_id,
            lifecycle_state: PAYER_LIFECYCLE_CLOSED.to_owned(),
            closed_with_open_balance: has_balance,
        }),
    )
        .into_response())
}
