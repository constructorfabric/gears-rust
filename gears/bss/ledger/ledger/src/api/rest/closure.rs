//! Axum handler + router for fiscal-period closure (Slice 7 Group C).
//! `POST /bss-ledger/v1/legal-entities/{legal_entity_id}/periods/{period_id}/closure`
//! — Finance-initiated period close / reopen for the caller's own seller ledger.
//!
//! The body's `action` discriminates:
//! - `"close"` runs the two-phase gated close (`OPEN→CLOSED`); a clean period
//!   closes inline (200), a gate-blocked period returns 409 `PERIOD_CLOSE_BLOCKED`
//!   (the blocked reasons are recorded on the `period_close` row).
//! - `"reopen"` is **always** dual-control (design §7 / N-core-3): it never reopens
//!   inline — it creates a PENDING `PERIOD_REOPEN` approval and returns
//!   409 `DUAL_CONTROL_REQUIRED`; a distinct approver's `POST /approvals/{id}/approve`
//!   then drives the actual `CLOSED→REOPENED` flip through the executor.
//!
//! A `…/periods/{period_id}/closure` **sub-resource** (not a `{period_id}:close`
//! colon custom method — those don't route on axum 0.8 / matchit 0.8.4; design F-4).

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
use crate::api::rest::error::authz_error_to_canonical;
use crate::domain::approval::ApprovalKind;
use crate::domain::approval::intent::{ApprovalIntent, PeriodReopenIntent};
use crate::domain::approval::policy::OperationFacts;
use crate::domain::error::DomainError;
use crate::domain::status::PERIOD_STATUS_CLOSED;
use crate::infra::approval::service::ApprovalService;

/// `OpenAPI` tag applied to the period-closure operation.
const TAG: &str = "BSS Ledger Period Close";

/// Body `action` discriminator literals.
const ACTION_CLOSE: &str = "close";
const ACTION_REOPEN: &str = "reopen";

/// Shared per-request state for the closure route.
#[derive(Clone)]
pub struct ApiState {
    /// In-process ledger client — `close_period` runs its own `(fiscal_period,
    /// close)` PEP gate + the two-phase gated close (+ emits `period.closed`).
    pub client: Arc<dyn bss_ledger_sdk::api::LedgerClientV1>,
    /// Dual-control engine — a `reopen` routes through `gate()` (always over
    /// threshold for `PeriodReopen`, so always 409 `DUAL_CONTROL_REQUIRED`).
    pub approval: Arc<ApprovalService>,
}

/// Period-closure request body.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(request)]
pub struct PeriodClosureRequest {
    /// `"close"` (gated `OPEN→CLOSED`) or `"reopen"` (dual-control `CLOSED→REOPENED`).
    pub action: String,
    /// Free-text reason recorded with the operation (Finance audit context).
    pub reason: Option<String>,
}

/// Period-closure response (the inline `close` path; a `reopen` returns 409, never
/// this body).
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(response)]
pub struct PeriodClosureResponse {
    pub legal_entity_id: Uuid,
    pub period_id: String,
    /// The period's lifecycle status after the close (`CLOSED`).
    pub status: String,
    /// `true` when the period was already `CLOSED` (idempotent re-close).
    pub already_closed: bool,
}

/// Build the Axum router for the period-closure surface.
pub fn router(state: Arc<ApiState>, openapi: &dyn OpenApiRegistry) -> Router {
    let mut router = Router::new();
    router = OperationBuilder::post(
        "/bss-ledger/v1/legal-entities/{legal_entity_id}/periods/{period_id}/closure",
    )
    .operation_id("bss_ledger.period_closure")
    .summary("Close or reopen a fiscal period")
    .description(
        "Finance-initiated period close / reopen for the caller's own seller \
         ledger. `action=close` runs the two-phase gated close (a clean period \
         closes inline 200; a gate-blocked period returns 409 PERIOD_CLOSE_BLOCKED). \
         `action=reopen` is always dual-control: it never reopens inline — it \
         creates a PENDING PERIOD_REOPEN approval and returns 409 \
         DUAL_CONTROL_REQUIRED, and a distinct approver's approval drives the \
         CLOSED→REOPENED flip.",
    )
    .tag(TAG)
    .authenticated()
    .no_license_required()
    .path_param(
        "legal_entity_id",
        "The seller legal-entity that owns the period.",
    )
    .path_param("period_id", "The accounting period (YYYYMM).")
    .json_request::<PeriodClosureRequest>(openapi, "The close/reopen action + reason.")
    .handler(period_closure)
    .json_response_with_schema::<PeriodClosureResponse>(
        openapi,
        StatusCode::OK,
        "The period was closed inline (clean books).",
    )
    .error_400(openapi)
    .error_401(openapi)
    .error_403(openapi)
    // 409: a gate-blocked close (PERIOD_CLOSE_BLOCKED) and a reopen that always routes
    // through dual-control (DUAL_CONTROL_REQUIRED) both emit Aborted → 409.
    .error_409(openapi)
    .error_500(openapi)
    .register(router, openapi);

    router.layer(Extension(state))
}

/// `POST …/legal-entities/{le}/periods/{period}/closure`: dispatch on `action`.
///
/// The seller ledger is the caller's own tenant (`ctx.subject_tenant_id()`,
/// mirroring `close_payer`); the path `legal_entity_id` MUST equal it (v1 — one
/// legal entity per tenant). A path LE that is not the caller's own tenant is
/// another seller's books and resolves to a 404 (BOLA, no existence leak), never a
/// close of the caller's own period under a foreign label.
async fn period_closure(
    Extension(state): Extension<Arc<ApiState>>,
    Extension(enforcer): Extension<authz_resolver_sdk::PolicyEnforcer>,
    extension_ctx: Option<Extension<SecurityContext>>,
    Path((legal_entity_id, period_id)): Path<(Uuid, String)>,
    CanonicalJson(body): CanonicalJson<PeriodClosureRequest>,
) -> Result<Response, CanonicalError> {
    let ctx = require_authenticated(extension_ctx)?;
    let tenant_id = ctx.subject_tenant_id();
    // v1: one legal entity per tenant. A path LE outside the caller's own tenant is
    // a foreign seller's period — fail closed as not-found (no existence leak).
    if legal_entity_id != tenant_id {
        return Err(CanonicalError::from(DomainError::PeriodNotFound(format!(
            "{legal_entity_id}/{period_id}"
        ))));
    }

    match body.action.as_str() {
        ACTION_CLOSE => {
            // `close_period` runs its own `(fiscal_period, close)` PEP gate against
            // `tenant_id`, then the two-phase gated close (emitting `period.closed`).
            let outcome = state
                .client
                .close_period(&ctx, tenant_id, period_id.clone())
                .await?;
            Ok((
                StatusCode::OK,
                Json(PeriodClosureResponse {
                    legal_entity_id,
                    period_id: outcome.period_id,
                    status: PERIOD_STATUS_CLOSED.to_owned(),
                    already_closed: outcome.already_closed,
                }),
            )
                .into_response())
        }
        ACTION_REOPEN => {
            // Reopen authority = `(fiscal_period, close)` (the preparer must be able
            // to close); the compiled scope is the dual-control gate's BOLA filter.
            let scope = crate::authz::access_scope(
                &enforcer,
                &ctx,
                &crate::authz::resource_types::FISCAL_PERIOD,
                crate::authz::actions::CLOSE,
                Some(tenant_id),
                None,
                /* require_constraints */ true,
            )
            .await
            .map_err(authz_error_to_canonical)?;

            // Reopen is ALWAYS dual-control (policy `requires_dual_control` returns
            // `true` for `PeriodReopen`), so `gate()` always creates a PENDING
            // approval and we always 409. The actual `CLOSED→REOPENED` flip is driven
            // later by a distinct approver's `approve` → executor — never inline here.
            let intent = ApprovalIntent::PeriodReopen(PeriodReopenIntent {
                tenant_id,
                legal_entity_id,
                period_id: period_id.clone(),
            });
            let facts = OperationFacts {
                kind: ApprovalKind::PeriodReopen,
                amount_usd_eq_minor: None,
                effective_at: None,
                has_outstanding_balance: false,
            };
            let approval_id = state
                .approval
                .gate(&ctx, &scope, intent, facts, "period-reopen".to_owned())
                .await
                .map_err(CanonicalError::from)?
                .ok_or_else(|| {
                    // Unreachable (the policy always requires dual-control for a
                    // reopen); fail closed rather than reopen without sign-off.
                    CanonicalError::from(DomainError::Internal(
                        "period reopen must route through dual-control but the policy \
                         did not require it"
                            .to_owned(),
                    ))
                })?;
            Err(CanonicalError::from(DomainError::DualControlRequired(
                format!("period reopen requires dual-control approval: {approval_id}"),
            )))
        }
        other => Err(CanonicalError::from(DomainError::InvalidRequest(format!(
            "unknown closure action {other:?} (expected \"{ACTION_CLOSE}\" or \"{ACTION_REOPEN}\")"
        )))),
    }
}
