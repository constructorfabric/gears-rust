//! Axum handlers + router for the ledger's payment REST surface (architecture
//! §6, money-in / money-out). Four operations under `/bss-ledger/v1`, all
//! tenant-scoped WITHOUT a tenant in the path (the vhp-core convention, matching
//! the journal-entry / provisioning surfaces): writes carry `tenant_id` in the
//! **body**, the unallocated read takes a `?tenant_id=` **query** param.
//!
//! Writes (settle / allocate), `(payment, write)` PEP gate against the body's
//! `tenant_id`:
//! - `POST /payments` — settle a received receipt into the payer's unallocated
//!   pool. Idempotent on `payment_id`: a re-settle replays (`200`), a fresh
//!   settle is `201`.
//! - `POST /payments/{payment_id}/allocations` — drain the pool to open AR
//!   oldest-first. `{payment_id}` from the path, the rest from the body. Always
//!   `201` (the recorded splits).
//!
//! Reads (`(payment, read)`, PDP-scoped in the client — the SQL-level BOLA
//! filter, so no separate handler gate, matching the journal-entry reads):
//! - `GET /payments/{payment_id}/allocations` — the recorded splits.
//! - `GET /balances/unallocated?tenant_id=&payer_tenant_id=&currency=` — the
//!   payer's still-undrained pool.
//!
//! Routes register through `OperationBuilder` so `/openapi.json` lists each
//! operation with its declared request / response schemas.

use std::sync::Arc;

use axum::extract::{Extension, Path, Query};
use axum::response::{IntoResponse, Response};
use axum::{Json, Router, http::StatusCode};
use bss_ledger_sdk::api::LedgerClientV1;
use toolkit::api::canonical_prelude::CanonicalError;
use toolkit::api::{OpenApiRegistry, operation_builder::OperationBuilder};
use toolkit_security::SecurityContext;
use uuid::Uuid;

use crate::api::rest::auth_context::require_authenticated;
use crate::api::rest::canonical_json::CanonicalJson;
use crate::api::rest::dto::{
    AllocatePaymentRequest, AllocatePaymentResponse, AllocationQueuedResponse,
    PaymentAllocationsDto, ReturnPaymentRequest, ReturnPaymentResponse, SettlePaymentRequest,
    SettlePaymentResponse, SettlementView, UnallocatedDto,
};
use crate::api::rest::error::{authz_error_to_canonical, settlement_not_found};
use crate::infra::storage::repo::PaymentRepo;

/// `OpenAPI` tag applied to the payment operations.
const TAG: &str = "BSS Ledger Payments";

/// Shared per-request state for the payment routes. Constructed once at `init()`
/// and shared via `Extension<Arc<ApiState>>`. Carries the in-process
/// data-access client (settle / allocate / list / read-unallocated all go
/// through it — the client gates the PEP and orchestrates the post).
#[derive(Clone)]
pub struct ApiState {
    /// In-process data-access client (the gear's own local impl).
    pub client: Arc<dyn LedgerClientV1>,
    /// The payment repo — the `GET /payments/{payment_id}/settlement` by-payment
    /// read source (the `payment_settlement` counters row). A plain scoped read,
    /// mirroring [`crate::api::rest::disputes::ApiState`]'s `dispute_repo`.
    pub payment_repo: PaymentRepo,
}

/// Build the Axum router for the payment surface and register every operation
/// with the supplied `OpenAPI` registry. `state` is attached via an `Extension`
/// layer at the end so the registry sees the route definitions before the
/// per-request state is bound.
#[allow(clippy::too_many_lines)] // one builder chain per operation; flat is clearer than helpers
pub fn router(state: Arc<ApiState>, openapi: &dyn OpenApiRegistry) -> Router {
    let mut router = Router::new();

    router = OperationBuilder::post("/bss-ledger/v1/payments")
        .operation_id("bss_ledger.settle_payment")
        .summary("Settle a received payment (money-in)")
        .description(
            "Records a received receipt into the payer's unallocated pool \
             (DR CASH_CLEARING net, DR PSP_FEE_EXPENSE fee, CR UNALLOCATED \
             gross) for the seller named by the body's `tenant_id`. Idempotent \
             on `payment_id`: a re-settle returns the prior posting reference \
             (200) instead of a new one (201). A settlement records money \
             already received and lands even for a closed payer.",
        )
        .tag(TAG)
        .authenticated()
        .no_license_required()
        .json_request::<SettlePaymentRequest>(openapi, "The settled payment to record.")
        .handler(settle_payment)
        .json_response_with_schema::<SettlePaymentResponse>(
            openapi,
            StatusCode::CREATED,
            "Posting reference (201 fresh settle / 200 idempotent replay)",
        )
        .json_response_with_schema::<SettlePaymentResponse>(
            openapi,
            StatusCode::OK,
            "Idempotent replay of a prior settle",
        )
        .error_400(openapi)
        .error_401(openapi)
        .error_403(openapi)
        .error_500(openapi)
        .register(router, openapi);

    router = OperationBuilder::post("/bss-ledger/v1/payments/{payment_id}/returns")
        .operation_id("bss_ledger.return_payment")
        .summary("Return a settled payment (claw a receipt back)")
        .description(
            "Reverses a money-in: claws `{payment_id}`'s settled receipt back out \
             of the payer's unallocated pool (DR UNALLOCATED, CR CASH_CLEARING) \
             and decrements the payment's settled total, for the seller named by \
             the body's `tenant_id`. Idempotent on `psp_return_id`: a re-post \
             returns the prior posting reference (200) instead of a new one (201). \
             A return records money already moved and lands even for a closed \
             payer. Rejected when the return exceeds the still-returnable settled \
             amount (SETTLEMENT_RETURN_OVER_ALLOCATED).",
        )
        .tag(TAG)
        .authenticated()
        .no_license_required()
        .path_param("payment_id", "The settled payment to claw back.")
        .json_request::<ReturnPaymentRequest>(openapi, "The return to record + idempotency key.")
        .handler(return_payment)
        .json_response_with_schema::<ReturnPaymentResponse>(
            openapi,
            StatusCode::CREATED,
            "Posting reference (201 fresh return / 200 idempotent replay)",
        )
        .json_response_with_schema::<ReturnPaymentResponse>(
            openapi,
            StatusCode::OK,
            "Idempotent replay of a prior return",
        )
        .error_400(openapi)
        .error_401(openapi)
        .error_403(openapi)
        .error_500(openapi)
        .register(router, openapi);

    router = OperationBuilder::post("/bss-ledger/v1/payments/{payment_id}/allocations")
        .operation_id("bss_ledger.allocate_payment")
        .summary("Allocate a settled payment to open receivables (money-out)")
        .description(
            "Drains the payment's unallocated pool into the payer's open AR \
             (DR UNALLOCATED, CR AR per invoice) for `{payment_id}`. By default \
             the lump is applied by the tenant's precedence policy and \
             `hint_invoice_id` jumps one invoice to the front of the fill order; \
             supply `splits` to bypass the policy with a caller-computed split \
             (validated against the open receivables). Idempotent on \
             `allocation_id`. When the payment is already settled the allocation \
             posts inline (201). When it is NOT yet settled the request is durably \
             QUEUED for a later drain (202 `allocation-queued`, §4.7 \
             allocation-before-settlement) instead of being rejected. Rejected \
             (400) when the allocation would exceed what was settled \
             (ALLOCATION_EXCEEDS_SETTLED), spans too many invoices \
             (ALLOCATION_TOO_LARGE), mismatches the settled currency \
             (ALLOCATION_CURRENCY_MISMATCH), or carries an invalid caller split \
             (ALLOCATION_SPLIT_INVALID).",
        )
        .tag(TAG)
        .authenticated()
        .no_license_required()
        .path_param("payment_id", "The payment to allocate from.")
        .json_request::<AllocatePaymentRequest>(openapi, "The lump to allocate + idempotency key.")
        .handler(allocate_payment)
        .json_response_with_schema::<AllocatePaymentResponse>(
            openapi,
            StatusCode::CREATED,
            "The posting reference plus the per-invoice splits applied (payment settled)",
        )
        .json_response_with_schema::<AllocationQueuedResponse>(
            openapi,
            StatusCode::ACCEPTED,
            "Allocation queued for a later drain (payment not yet settled)",
        )
        .error_400(openapi)
        .error_401(openapi)
        .error_403(openapi)
        .error_500(openapi)
        .register(router, openapi);

    router = OperationBuilder::get("/bss-ledger/v1/payments/{payment_id}/allocations")
        .operation_id("bss_ledger.list_payment_allocations")
        .summary("List a payment's recorded allocations")
        .description(
            "Returns the per-invoice splits recorded against `{payment_id}` for \
             the caller's own tenant. A payment outside the caller's authorized \
             subtree yields an empty list (SQL-level BOLA, no existence leak).",
        )
        .tag(TAG)
        .authenticated()
        .no_license_required()
        .path_param("payment_id", "The payment whose allocations to list.")
        .handler(list_payment_allocations)
        .json_response_with_schema::<PaymentAllocationsDto>(
            openapi,
            StatusCode::OK,
            "The recorded allocation splits for the payment",
        )
        .error_401(openapi)
        .error_403(openapi)
        .error_500(openapi)
        .register(router, openapi);

    router = OperationBuilder::get("/bss-ledger/v1/balances/unallocated")
        .operation_id("bss_ledger.read_unallocated")
        .summary("Read a payer's unallocated pool balance")
        .description(
            "Returns the still-undrained portion of the payer's settled receipts \
             for one currency — `?tenant_id=` (the caller's own by default), \
             `?payer_tenant_id=`, `?currency=`. A payer outside the caller's \
             authorized subtree yields a zero balance (SQL-level BOLA).",
        )
        .tag(TAG)
        .authenticated()
        .no_license_required()
        .query_param(
            "tenant_id",
            false,
            "Target tenant (defaults to the caller's own)",
        )
        .query_param("payer_tenant_id", true, "The payer whose pool to read")
        .query_param("currency", true, "The pool currency (ISO 4217 code)")
        .handler(read_unallocated)
        .json_response_with_schema::<UnallocatedDto>(
            openapi,
            StatusCode::OK,
            "The payer's unallocated pool balance for the currency",
        )
        .error_400(openapi)
        .error_401(openapi)
        .error_403(openapi)
        .error_500(openapi)
        .register(router, openapi);

    router = OperationBuilder::get("/bss-ledger/v1/payments/{payment_id}/settlement")
        .operation_id("bss_ledger.get_payment_settlement")
        .summary("Read a payment's settlement counters")
        .description(
            "Returns the per-payment money-out serialization counters recorded for \
             `{payment_id}` — the settled / fee / allocated / refunded / \
             clawed-back running totals the money-out caps serialize against (drawn \
             from the `payment_settlement` row). The owning seller `tenant_id` is \
             required in the query (the settlement PK is `(tenant_id, \
             payment_id)`). Tenant-scoped (SQL-level BOLA): a payment that was \
             never settled — or one outside the caller's authorized subtree — yields \
             a 404 (no existence leak). Mirrors `get_refund`.",
        )
        .tag(TAG)
        .authenticated()
        .no_license_required()
        .path_param(
            "payment_id",
            "The payment whose settlement counters to read.",
        )
        .query_param(
            "tenant_id",
            true,
            "The payment's owning seller tenant (the settlement PK's tenant half).",
        )
        .handler(get_payment_settlement)
        .json_response_with_schema::<SettlementView>(
            openapi,
            StatusCode::OK,
            "The payment's settlement counters",
        )
        .error_400(openapi)
        .error_401(openapi)
        .error_403(openapi)
        .error_404(openapi)
        .error_500(openapi)
        .register(router, openapi);

    router.layer(Extension(state))
}

/// A settlement posting reference rendered with the right status: `201 Created`
/// for a fresh settle, `200 OK` for an idempotent replay of a prior settle.
fn settle_response(reference: bss_ledger_sdk::PostingRef) -> Response {
    let status = if reference.replayed {
        StatusCode::OK
    } else {
        StatusCode::CREATED
    };
    (status, Json(SettlePaymentResponse::from(reference))).into_response()
}

// The `CanonicalJson` extractor runs (and may reject with a canonical 400)
// BEFORE the in-handler `require_authenticated` gate, so a malformed body yields
// 400 even for an unauthenticated caller (standard axum extractor ordering; no
// authenticated-only data is disclosed).
async fn settle_payment(
    Extension(state): Extension<Arc<ApiState>>,
    Extension(enforcer): Extension<authz_resolver_sdk::PolicyEnforcer>,
    extension_ctx: Option<Extension<SecurityContext>>,
    CanonicalJson(body): CanonicalJson<SettlePaymentRequest>,
) -> Result<Response, CanonicalError> {
    let ctx = require_authenticated(extension_ctx)?;
    // The target seller is the body's `tenant_id` (tenant in body, not path).
    let tenant_id = body.tenant_id;
    // (payment, write) PEP gate against the TARGET tenant: a parent settles into
    // a seller in its authorized subtree; a target outside the caller's scope is
    // a cross-tenant write and is denied. The in-process client gates again
    // (defence-in-depth, matching the provisioning surface).
    crate::authz::access_scope(
        &enforcer,
        &ctx,
        &crate::authz::resource_types::PAYMENT,
        crate::authz::actions::WRITE,
        Some(tenant_id),
        None,
        /* require_constraints */ true,
    )
    .await
    .map_err(authz_error_to_canonical)?;
    let cmd = body.into_sdk()?;
    let reference = state.client.settle_payment(&ctx, cmd).await?;
    Ok(settle_response(reference))
}

/// A settlement-return posting reference rendered with the right status: `201
/// Created` for a fresh return, `200 OK` for an idempotent replay of a prior
/// return (mirrors `settle_response`).
fn return_response(reference: bss_ledger_sdk::PostingRef) -> Response {
    let status = if reference.replayed {
        StatusCode::OK
    } else {
        StatusCode::CREATED
    };
    (status, Json(ReturnPaymentResponse::from(reference))).into_response()
}

async fn return_payment(
    Extension(state): Extension<Arc<ApiState>>,
    Extension(enforcer): Extension<authz_resolver_sdk::PolicyEnforcer>,
    extension_ctx: Option<Extension<SecurityContext>>,
    Path(payment_id): Path<String>,
    CanonicalJson(body): CanonicalJson<ReturnPaymentRequest>,
) -> Result<Response, CanonicalError> {
    let ctx = require_authenticated(extension_ctx)?;
    // The target seller is the body's `tenant_id`; `payment_id` is the path id of
    // the original settled payment being clawed back.
    let tenant_id = body.tenant_id;
    crate::authz::access_scope(
        &enforcer,
        &ctx,
        &crate::authz::resource_types::PAYMENT,
        crate::authz::actions::WRITE,
        Some(tenant_id),
        None,
        /* require_constraints */ true,
    )
    .await
    .map_err(authz_error_to_canonical)?;
    let cmd = body.into_sdk(payment_id)?;
    let reference = state.client.return_payment(&ctx, cmd).await?;
    Ok(return_response(reference))
}

async fn allocate_payment(
    Extension(state): Extension<Arc<ApiState>>,
    Extension(enforcer): Extension<authz_resolver_sdk::PolicyEnforcer>,
    extension_ctx: Option<Extension<SecurityContext>>,
    Path(payment_id): Path<String>,
    CanonicalJson(body): CanonicalJson<AllocatePaymentRequest>,
) -> Result<Response, CanonicalError> {
    let ctx = require_authenticated(extension_ctx)?;
    // The target seller is the body's `tenant_id`; `payment_id` is the path id.
    let tenant_id = body.tenant_id;
    crate::authz::access_scope(
        &enforcer,
        &ctx,
        &crate::authz::resource_types::PAYMENT,
        crate::authz::actions::WRITE,
        Some(tenant_id),
        None,
        true,
    )
    .await
    .map_err(authz_error_to_canonical)?;
    // The allocation's `payment_id` comes from the PATH; the body carries the
    // tenant / payer / lump / idempotency key.
    let cmd = body.into_sdk(payment_id)?;
    let outcome = state.client.allocate_payment(&ctx, cmd).await?;
    Ok(allocate_response(outcome))
}

/// An allocate outcome rendered with the right status: `201 Created` + the
/// posting handle + splits when the payment was settled (the allocation posted
/// inline), or `202 Accepted` + the `allocation-queued` body when the payment was
/// not yet settled (the allocation was durably queued for a later drain, §4.7).
/// Mirrors `settle_response`'s status-varying rendering.
fn allocate_response(outcome: bss_ledger_sdk::AllocateOutcome) -> Response {
    match outcome {
        bss_ledger_sdk::AllocateOutcome::Applied(applied) => (
            StatusCode::CREATED,
            Json(AllocatePaymentResponse::from(applied)),
        )
            .into_response(),
        bss_ledger_sdk::AllocateOutcome::Queued(queued) => (
            StatusCode::ACCEPTED,
            Json(AllocationQueuedResponse::from(queued)),
        )
            .into_response(),
    }
}

async fn list_payment_allocations(
    Extension(state): Extension<Arc<ApiState>>,
    extension_ctx: Option<Extension<SecurityContext>>,
    Path(payment_id): Path<String>,
) -> Result<Json<PaymentAllocationsDto>, CanonicalError> {
    let ctx = require_authenticated(extension_ctx)?;
    // Tenant from the context; the client's PDP `In` read scope is the SQL-level
    // BOLA filter (a foreign payment resolves to an empty list, no leak), so
    // there is no separate target-anchored gate here (matching the journal-entry
    // reads). `(payment, read)` is enforced inside the client.
    let tenant_id = ctx.subject_tenant_id();
    let rows = state
        .client
        .list_payment_allocations(&ctx, tenant_id, payment_id)
        .await?;
    Ok(Json(PaymentAllocationsDto::from(rows)))
}

/// `GET /balances/unallocated` query parameters. `tenant_id` defaults to the
/// caller's own; `payer_tenant_id` + `currency` are required (they identify the
/// pool grain). The client's PDP read scope is the SQL-level BOLA filter.
#[derive(Debug, serde::Deserialize)]
struct UnallocatedQuery {
    tenant_id: Option<Uuid>,
    payer_tenant_id: Uuid,
    currency: String,
}

async fn read_unallocated(
    Extension(state): Extension<Arc<ApiState>>,
    extension_ctx: Option<Extension<SecurityContext>>,
    Query(query): Query<UnallocatedQuery>,
) -> Result<Json<UnallocatedDto>, CanonicalError> {
    let ctx = require_authenticated(extension_ctx)?;
    // Reject a malformed currency code at the boundary: an unvalidated code would
    // silently match zero rows instead of a clean 400. Non-ISO/crypto codes are
    // admitted by the scale registry, so accept any non-empty ASCII code (≤10
    // chars) rather than strict ISO-4217.
    if query.currency.is_empty() || query.currency.len() > 10 || !query.currency.is_ascii() {
        return Err(crate::domain::error::DomainError::InvalidRequest(format!(
            "currency must be a non-empty ASCII code of at most 10 chars, got {:?}",
            query.currency
        ))
        .into());
    }
    let tenant_id = query.tenant_id.unwrap_or_else(|| ctx.subject_tenant_id());
    let view = state
        .client
        .read_unallocated(&ctx, tenant_id, query.payer_tenant_id, query.currency)
        .await?;
    Ok(Json(UnallocatedDto::from(view)))
}

/// `GET /payments/{payment_id}/settlement` query parameters: the payment's owning
/// seller `tenant_id` (the settlement PK is `(tenant_id, payment_id)`, so the
/// tenant is REQUIRED in the query — unlike the unallocated read, which defaults it
/// to the caller's own). `payment_id` is the path param.
#[derive(Debug, serde::Deserialize)]
struct SettlementQuery {
    tenant_id: Uuid,
}

async fn get_payment_settlement(
    Extension(state): Extension<Arc<ApiState>>,
    Extension(enforcer): Extension<authz_resolver_sdk::PolicyEnforcer>,
    extension_ctx: Option<Extension<SecurityContext>>,
    Path(payment_id): Path<String>,
    Query(query): Query<SettlementQuery>,
) -> Result<Json<SettlementView>, CanonicalError> {
    let ctx = require_authenticated(extension_ctx)?;
    let tenant_id = query.tenant_id;
    // (payment, read) PEP gate against the payment's owning tenant — the settlement
    // record IS payment-cache data (money-out counters), so it gates on the `payment`
    // resource like the allocations / unallocated reads, NOT the `entry` data-plane
    // read. The returned scope is the SQL-level BOLA filter the repo binds, so a
    // foreign-tenant settlement resolves to None ⇒ 404 (no existence leak).
    let scope = crate::authz::access_scope(
        &enforcer,
        &ctx,
        &crate::authz::resource_types::PAYMENT,
        crate::authz::actions::READ,
        Some(tenant_id),
        None,
        /* require_constraints */ true,
    )
    .await
    .map_err(authz_error_to_canonical)?;

    let settlement = state
        .payment_repo
        .read_settlement(&scope, tenant_id, &payment_id)
        .await
        .map_err(|e| {
            crate::domain::error::DomainError::Internal(format!("read payment settlement: {e}"))
        })?
        .ok_or_else(|| settlement_not_found(&payment_id))?;
    Ok(Json(SettlementView::from(settlement)))
}
