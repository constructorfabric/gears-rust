//! Axum handler + router for the ledger's reusable-credit (wallet) REST surface
//! (architecture §5.2). ONE operation under `/bss-ledger/v1`, tenant-scoped
//! WITHOUT a tenant in the path (the vhp-core convention, matching the
//! payment / journal-entry / provisioning surfaces): the write carries
//! `tenant_id` in the **body**.
//!
//! - `POST /credit-applications` — operate a tenant's reusable-credit wallet.
//!   ONE endpoint, two `kind`s: `"grant"` parks unallocated pool cash into the
//!   wallet sub-grain (`DR UNALLOCATED` / `CR REUSABLE_CREDIT`), `"apply"` spends
//!   the wallet against the named open receivables oldest-grant-first
//!   (`N×DR REUSABLE_CREDIT` / `M×CR AR`). Idempotent on `credit_application_id`.
//!   Always `201` (the recorded posting + — for an apply — the wallet draws and
//!   the per-invoice shares). `(credit_application, write)` PEP gate against the
//!   body's `tenant_id`.
//!
//! The route registers through `OperationBuilder` so `/openapi.json` lists it
//! with its declared request / response schemas. Mirrors `payments::router`.

use std::sync::Arc;

use axum::extract::Extension;
use axum::response::{IntoResponse, Response};
use axum::{Json, Router, http::StatusCode};
use bss_ledger_sdk::api::LedgerClientV1;
use toolkit::api::canonical_prelude::CanonicalError;
use toolkit::api::{OpenApiRegistry, operation_builder::OperationBuilder};
use toolkit_security::SecurityContext;

use crate::api::rest::auth_context::require_authenticated;
use crate::api::rest::canonical_json::CanonicalJson;
use crate::api::rest::dto::{CreditApplicationRequest, CreditApplicationResponse};
use crate::api::rest::error::authz_error_to_canonical;
use crate::authz::{actions, resource_types};

/// `OpenAPI` tag applied to the credit-application operation.
const TAG: &str = "BSS Ledger Credit";

/// Shared per-request state for the credit route. Constructed once at `init()`
/// and shared via `Extension<Arc<ApiState>>`. Carries the in-process data-access
/// client (the wallet grant / apply goes through it — the client gates the PEP
/// and orchestrates the post). Mirrors [`crate::api::rest::payments::ApiState`].
#[derive(Clone)]
pub struct ApiState {
    /// In-process data-access client (the gear's own local impl).
    pub client: Arc<dyn LedgerClientV1>,
    /// Dual-control lifecycle engine (VHP-1852): the grant gate routes a
    /// high-value credit grant to the preparer→approver queue. `None` disables
    /// the gate (router unit tests without a governance DB).
    pub approval: Option<Arc<crate::infra::approval::service::ApprovalService>>,
}

/// Build the Axum router for the credit surface and register its operation with
/// the supplied `OpenAPI` registry. `state` is attached via an `Extension` layer
/// at the end so the registry sees the route definition before the per-request
/// state is bound. Mirrors [`crate::api::rest::payments::router`].
pub fn router(state: Arc<ApiState>, openapi: &dyn OpenApiRegistry) -> Router {
    let mut router = Router::new();

    router = OperationBuilder::post("/bss-ledger/v1/credit-applications")
        .operation_id("bss_ledger.post_credit_application")
        .summary("Grant or apply reusable credit (wallet)")
        .description(
            "Operates the payer's reusable-credit wallet for the seller named by \
             the body's `tenant_id`. ONE endpoint, two `kind`s. `kind = \"grant\"` \
             parks `amount_minor` of the payer's unallocated pool into the wallet \
             sub-grain `credit_grant_event_type` (DR UNALLOCATED, CR \
             REUSABLE_CREDIT), capped at the live pool. `kind = \"apply\"` spends \
             the wallet against the `targets` open receivables oldest-grant-first \
             (N×DR REUSABLE_CREDIT, M×CR AR), capped on both the receivable side \
             (open AR) and the wallet side (spendable sub-grains). Idempotent on \
             `credit_application_id`. Rejected (400) when a grant exceeds the \
             unallocated pool (GRANT_EXCEEDS_UNALLOCATED), an apply target names \
             an unknown/closed invoice or over-applies the open AR \
             (CREDIT_EXCEEDS_OPEN_AR), or the spendable wallet cannot cover the \
             apply total (CREDIT_EXCEEDS_WALLET).",
        )
        .tag(TAG)
        .authenticated()
        .no_license_required()
        .json_request::<CreditApplicationRequest>(
            openapi,
            "The wallet operation (kind: grant | apply) + idempotency key.",
        )
        .handler(post_credit_application)
        .json_response_with_schema::<CreditApplicationResponse>(
            openapi,
            StatusCode::CREATED,
            "The posting reference plus (apply only) the wallet draws + per-invoice shares",
        )
        .error_400(openapi)
        .error_401(openapi)
        .error_403(openapi)
        .error_500(openapi)
        .register(router, openapi);

    router.layer(Extension(state))
}

// The `CanonicalJson` extractor runs (and may reject with a canonical 400)
// BEFORE the in-handler `require_authenticated` gate, so a malformed body yields
// 400 even for an unauthenticated caller (standard axum extractor ordering; no
// authenticated-only data is disclosed). Mirrors `payments::allocate_payment`.
async fn post_credit_application(
    Extension(state): Extension<Arc<ApiState>>,
    Extension(enforcer): Extension<authz_resolver_sdk::PolicyEnforcer>,
    extension_ctx: Option<Extension<SecurityContext>>,
    CanonicalJson(body): CanonicalJson<CreditApplicationRequest>,
) -> Result<Response, CanonicalError> {
    let ctx = require_authenticated(extension_ctx)?;
    // The target seller is the body's `tenant_id` (tenant in body, not path).
    let tenant_id = body.tenant_id;
    // (credit_application, write) PEP gate against the TARGET tenant: a parent
    // operates a wallet in a seller in its authorized subtree; a target outside
    // the caller's scope is a cross-tenant write and is denied. The in-process
    // client gates again (defence-in-depth, matching the payment surface). The
    // gate runs on the body's `tenant_id` BEFORE `into_sdk()` — which may itself
    // return a 400 for a malformed `kind` / missing field.
    crate::authz::access_scope(
        &enforcer,
        &ctx,
        &resource_types::CREDIT_APPLICATION,
        actions::WRITE,
        Some(tenant_id),
        None,
        /* require_constraints */ true,
    )
    .await
    .map_err(authz_error_to_canonical)?;

    // Dual-control gate (VHP-1852): a high-value credit GRANT routes to the
    // preparer→approver queue (409); an apply, or a below-threshold grant, posts
    // inline (unchanged).
    if body.kind == "grant"
        && let Some(amount) = body.amount_minor
        && let Some(approval) = &state.approval
    {
        let grant_intent = crate::domain::approval::intent::ApprovalIntent::CreditGrant(
            crate::domain::approval::intent::CreditGrantIntent {
                tenant_id: body.tenant_id,
                payer_tenant_id: body.payer_tenant_id,
                credit_application_id: body.credit_application_id.clone(),
                currency: body.currency.clone(),
                amount_minor: amount,
                credit_grant_event_type: body.credit_grant_event_type.clone(),
            },
        );
        let grant_facts = crate::domain::approval::policy::OperationFacts {
            kind: crate::domain::approval::ApprovalKind::CreditGrant,
            amount_usd_eq_minor: Some(amount),
            effective_at: None,
            has_outstanding_balance: false,
        };
        let scope = crate::authz::access_scope(
            &enforcer,
            &ctx,
            &resource_types::CREDIT_APPLICATION,
            actions::WRITE,
            Some(tenant_id),
            None,
            true,
        )
        .await
        .map_err(authz_error_to_canonical)?;
        if let Some(approval_id) = approval
            .gate(
                &ctx,
                &scope,
                grant_intent,
                grant_facts,
                "credit-grant".to_owned(),
            )
            .await
            .map_err(CanonicalError::from)?
        {
            return Err(CanonicalError::from(
                crate::domain::error::DomainError::DualControlRequired(format!(
                    "credit grant requires dual-control approval: {approval_id}"
                )),
            ));
        }
    }

    let cmd = body.into_sdk()?;
    let applied = state.client.post_credit_application(&ctx, cmd).await?;
    Ok((
        StatusCode::CREATED,
        Json(CreditApplicationResponse::from(applied)),
    )
        .into_response())
}
