//! Axum handlers + router for the control-feed ingest surface (Slice 7 Phase 3,
//! design §0 decision 3 / §3.4). Three POST operations under
//! `/bss-ledger/v1/ledger/control`, tenant-scoped WITHOUT a tenant in the path
//! (the vhp-core convention): each write carries `tenant_id` in the **body**.
//!
//! These are CALL-DRIVEN control feeds — the owning module (Invoice /
//! Orchestration / the PSP adapter) PUSHES into the ledger; they are never posting
//! sources (design §1.2). In v1 they feed the in-process control store the
//! `ReconciliationFramework` + the period-close gate read back; the store is empty
//! (and so every check is inert) until something is pushed. A real external
//! adapter-gear, when present, overrides the corresponding port — these endpoints
//! remain the manual / seed ingest path.
//!
//! - `POST /ledger/control/issued-invoice-manifest` — the authoritative set of
//!   issued invoiceIds a `(tenant, period)` was billed for (the invoice-completeness
//!   check, N-recon-1).
//! - `POST /ledger/control/bill-run-finished` — the owning Orchestration's bill-run
//!   completion assertion for `(tenant, period)` (the close gate's prerequisite).
//! - `POST /ledger/control/psp-settlement-report` — the PSP's net settled amount for
//!   `(tenant, period)` (the Payments↔PSP tie).
//!
//! All three gate on `(ledger, provision)` against the body's `tenant_id` — control
//! feeds are ledger reference data, the same family as the FX rate ingest (lean
//! authz reuse; no new resource). Each push is last-writer-wins (the feed is a
//! snapshot, not an append log) and returns `202 Accepted`.

use std::sync::Arc;

use axum::extract::Extension;
use axum::response::{IntoResponse, Response};
use axum::{Json, Router, http::StatusCode};
use toolkit::api::canonical_prelude::CanonicalError;
use toolkit::api::{OpenApiRegistry, operation_builder::OperationBuilder};
use toolkit_security::SecurityContext;
use uuid::Uuid;

use crate::api::rest::auth_context::require_authenticated;
use crate::api::rest::canonical_json::CanonicalJson;
use crate::api::rest::error::authz_error_to_canonical;
use crate::infra::control_feed::InProcessControlFeeds;

/// `OpenAPI` tag applied to the control-feed operations.
const TAG: &str = "BSS Ledger Control Feeds";

/// Shared per-request state for the control-feed routes. Constructed once at
/// `init()` and shared via `Extension<Arc<ApiState>>`. Carries the in-process
/// control-feed store the ingest endpoints push into.
#[derive(Clone)]
pub struct ApiState {
    /// The in-process control-feed store (the v1 default the framework + close gate
    /// read back). `Arc` — shared with the reconciliation framework + close gate.
    pub feeds: Arc<InProcessControlFeeds>,
}

/// `POST /ledger/control/issued-invoice-manifest` request body: the authoritative
/// issued-invoice manifest for `(tenant, period)` to push into the control store.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(request)]
pub struct IssuedInvoiceManifestRequest {
    /// The seller tenant whose ledger this feeds; the PEP gate target. In the body.
    pub tenant_id: Uuid,
    /// The fiscal `period_id` (`YYYYMM`) the manifest covers.
    pub period_id: String,
    /// The authoritative set of issued invoiceIds for the period.
    pub invoice_ids: Vec<String>,
    /// Control total: count of issued invoices.
    pub count: u64,
    /// Control total: summed gross amount in minor units.
    pub gross_total_minor: i64,
}

/// `POST /ledger/control/bill-run-finished` request body: the owning
/// Orchestration's bill-run completion assertion for `(tenant, period)`.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(request)]
pub struct BillRunFinishedRequest {
    /// The seller tenant whose ledger this feeds; the PEP gate target. In the body.
    pub tenant_id: Uuid,
    /// The fiscal `period_id` (`YYYYMM`) the assertion covers.
    pub period_id: String,
    /// `true` once the period's bill run has finished (the close-gate prerequisite).
    pub finished: bool,
}

/// `POST /ledger/control/psp-settlement-report` request body: the PSP's net settled
/// amount for `(tenant, period)`.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(request)]
pub struct PspSettlementReportRequest {
    /// The seller tenant whose ledger this feeds; the PEP gate target. In the body.
    pub tenant_id: Uuid,
    /// The fiscal `period_id` (`YYYYMM`) the report covers.
    pub period_id: String,
    /// External PSP report identity (idempotency grain).
    pub report_id: String,
    /// Net settled amount in minor units the PSP reports (net of refunds/returns).
    pub settled_minor: i64,
    /// ISO-4217 currency of the report.
    pub currency: String,
}

/// Ack for a control-feed push: the feed + the `(tenant, period)` grain it landed
/// on. The push is last-writer-wins, so a re-push of the same grain overwrites.
#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(response)]
pub struct ControlFeedAck {
    /// The control feed the push landed on (e.g. `issued-invoice-manifest`).
    pub feed: String,
    pub tenant_id: Uuid,
    pub period_id: String,
}

/// The three control-feed identifiers echoed in the ack.
const FEED_ISSUED_INVOICE_MANIFEST: &str = "issued-invoice-manifest";
const FEED_BILL_RUN_FINISHED: &str = "bill-run-finished";
const FEED_PSP_SETTLEMENT_REPORT: &str = "psp-settlement-report";

/// Build the Axum router for the control-feed surface and register all three
/// operations with the supplied `OpenAPI` registry. `state` is attached via an
/// `Extension` layer at the end so the registry sees the route definitions before
/// the per-request state is bound. Mirrors [`crate::api::rest::fx::router`].
pub fn router(state: Arc<ApiState>, openapi: &dyn OpenApiRegistry) -> Router {
    let mut router = Router::new();

    router = OperationBuilder::post("/bss-ledger/v1/ledger/control/issued-invoice-manifest")
        .operation_id("bss_ledger.ingest_issued_invoice_manifest")
        .summary("Push the issued-invoice manifest for a period (control feed)")
        .description(
            "Pushes the authoritative set of issued invoiceIds a `(tenant, period)` \
             was billed for into the in-process control store the \
             invoice-completeness check reads back (N-recon-1). A control feed only, \
             never a posting source; call-driven (the owning Invoice / Orchestration \
             service pushes). Last writer wins. `(ledger, provision)` PEP gate \
             against the body's `tenant_id`.",
        )
        .tag(TAG)
        .authenticated()
        .no_license_required()
        .json_request::<IssuedInvoiceManifestRequest>(
            openapi,
            "The issued-invoice manifest (tenant in the body).",
        )
        .handler(ingest_issued_invoice_manifest)
        .json_response_with_schema::<ControlFeedAck>(
            openapi,
            StatusCode::ACCEPTED,
            "The manifest was accepted into the control store.",
        )
        .error_400(openapi)
        .error_401(openapi)
        .error_403(openapi)
        .error_500(openapi)
        .register(router, openapi);

    router = OperationBuilder::post("/bss-ledger/v1/ledger/control/bill-run-finished")
        .operation_id("bss_ledger.ingest_bill_run_finished")
        .summary("Push the bill-run-finished assertion for a period (control feed)")
        .description(
            "Pushes the owning Orchestration's bill-run completion assertion for \
             `(tenant, period)` into the in-process control store the period-close \
             gate reads back. A control feed only, never a posting source; \
             call-driven. Last writer wins. `(ledger, provision)` PEP gate against \
             the body's `tenant_id`.",
        )
        .tag(TAG)
        .authenticated()
        .no_license_required()
        .json_request::<BillRunFinishedRequest>(
            openapi,
            "The bill-run-finished assertion (tenant in the body).",
        )
        .handler(ingest_bill_run_finished)
        .json_response_with_schema::<ControlFeedAck>(
            openapi,
            StatusCode::ACCEPTED,
            "The assertion was accepted into the control store.",
        )
        .error_400(openapi)
        .error_401(openapi)
        .error_403(openapi)
        .error_500(openapi)
        .register(router, openapi);

    router = OperationBuilder::post("/bss-ledger/v1/ledger/control/psp-settlement-report")
        .operation_id("bss_ledger.ingest_psp_settlement_report")
        .summary("Push the PSP settlement report for a period (control feed)")
        .description(
            "Pushes the PSP's net settled amount for `(tenant, period)` into the \
             in-process control store the Payments↔PSP reconciliation reads back. A \
             control feed only, never a posting source; call-driven (the PSP / its \
             adapter pushes). Last writer wins (the `report_id` carries the external \
             idempotency grain). `(ledger, provision)` PEP gate against the body's \
             `tenant_id`.",
        )
        .tag(TAG)
        .authenticated()
        .no_license_required()
        .json_request::<PspSettlementReportRequest>(
            openapi,
            "The PSP settlement report (tenant in the body).",
        )
        .handler(ingest_psp_settlement_report)
        .json_response_with_schema::<ControlFeedAck>(
            openapi,
            StatusCode::ACCEPTED,
            "The report was accepted into the control store.",
        )
        .error_400(openapi)
        .error_401(openapi)
        .error_403(openapi)
        .error_500(openapi)
        .register(router, openapi);

    router.layer(Extension(state))
}

// The `CanonicalJson` extractor runs (and may reject with a canonical 400) BEFORE
// the in-handler `require_authenticated` gate (standard axum extractor ordering; no
// authenticated-only data is disclosed). Mirrors `fx::ingest_fx_rate`.
async fn ingest_issued_invoice_manifest(
    Extension(state): Extension<Arc<ApiState>>,
    Extension(enforcer): Extension<authz_resolver_sdk::PolicyEnforcer>,
    extension_ctx: Option<Extension<SecurityContext>>,
    CanonicalJson(body): CanonicalJson<IssuedInvoiceManifestRequest>,
) -> Result<Response, CanonicalError> {
    let ctx = require_authenticated(extension_ctx)?;
    // (ledger, provision) gate against the TARGET tenant (the body's `tenant_id`):
    // control feeds are ledger reference data (the same family as the FX rate
    // ingest); a target outside the caller's authorized scope is denied.
    gate_provision(&enforcer, &ctx, body.tenant_id).await?;

    // Intake invariant (design §3.3 / N-recon-1): the manifest's `count` control-total
    // MUST equal its id-set size, or the feed is internally inconsistent — the
    // completeness reconciliation could then trust neither the count nor the membership.
    // Reject the malformed feed at the boundary (fail-loud) rather than store it.
    let id_count = u64::try_from(body.invoice_ids.len()).unwrap_or(u64::MAX);
    if body.count != id_count {
        return Err(CanonicalError::from(
            crate::domain::error::DomainError::InvalidRequest(format!(
                "issued-invoice manifest count {} does not match its id-set size {id_count}",
                body.count
            )),
        ));
    }

    state.feeds.ingest_manifest(
        body.tenant_id,
        &body.period_id,
        bss_ledger_sdk::IssuedInvoiceManifest {
            invoice_ids: body.invoice_ids,
            count: body.count,
            gross_total_minor: body.gross_total_minor,
        },
    );

    Ok(accepted(
        FEED_ISSUED_INVOICE_MANIFEST,
        body.tenant_id,
        body.period_id,
    ))
}

async fn ingest_bill_run_finished(
    Extension(state): Extension<Arc<ApiState>>,
    Extension(enforcer): Extension<authz_resolver_sdk::PolicyEnforcer>,
    extension_ctx: Option<Extension<SecurityContext>>,
    CanonicalJson(body): CanonicalJson<BillRunFinishedRequest>,
) -> Result<Response, CanonicalError> {
    let ctx = require_authenticated(extension_ctx)?;
    gate_provision(&enforcer, &ctx, body.tenant_id).await?;

    state
        .feeds
        .ingest_bill_run_finished(body.tenant_id, &body.period_id, body.finished);

    Ok(accepted(
        FEED_BILL_RUN_FINISHED,
        body.tenant_id,
        body.period_id,
    ))
}

async fn ingest_psp_settlement_report(
    Extension(state): Extension<Arc<ApiState>>,
    Extension(enforcer): Extension<authz_resolver_sdk::PolicyEnforcer>,
    extension_ctx: Option<Extension<SecurityContext>>,
    CanonicalJson(body): CanonicalJson<PspSettlementReportRequest>,
) -> Result<Response, CanonicalError> {
    let ctx = require_authenticated(extension_ctx)?;
    gate_provision(&enforcer, &ctx, body.tenant_id).await?;

    state.feeds.ingest_psp_report(
        body.tenant_id,
        &body.period_id,
        bss_ledger_sdk::PspSettlementReport {
            report_id: body.report_id,
            settled_minor: body.settled_minor,
            currency: body.currency,
        },
    );

    Ok(accepted(
        FEED_PSP_SETTLEMENT_REPORT,
        body.tenant_id,
        body.period_id,
    ))
}

/// The shared `(ledger, provision)` gate against the body's target tenant, common
/// to all three control-feed pushes (lean authz reuse, the FX-ingest family).
async fn gate_provision(
    enforcer: &authz_resolver_sdk::PolicyEnforcer,
    ctx: &SecurityContext,
    tenant_id: Uuid,
) -> Result<(), CanonicalError> {
    crate::authz::access_scope(
        enforcer,
        ctx,
        &crate::authz::resource_types::LEDGER,
        crate::authz::actions::PROVISION,
        Some(tenant_id),
        None,
        /* require_constraints */ true,
    )
    .await
    .map_err(authz_error_to_canonical)?;
    Ok(())
}

/// Build the `202 Accepted` ack for a control-feed push.
fn accepted(feed: &str, tenant_id: Uuid, period_id: String) -> Response {
    (
        StatusCode::ACCEPTED,
        Json(ControlFeedAck {
            feed: feed.to_owned(),
            tenant_id,
            period_id,
        }),
    )
        .into_response()
}
