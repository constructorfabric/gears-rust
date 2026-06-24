//! Route registration for `PaymentApi` REST endpoints.
//!
//! Uses [`OperationBuilder`] so each operation contributes its `OpenAPI`
//! spec alongside the axum handler. Authentication is layered upstream by
//! the API Gateway, which populates `Extension<SecurityContext>` — handlers
//! never parse the `Authorization` header themselves.

use std::sync::Arc;

use axum::{Extension, Router};
use http::StatusCode;
use toolkit::api::OpenApiRegistry;
use toolkit::api::operation_builder::OperationBuilder;

use api_contracts_sdk::contract::PaymentApi;
use api_contracts_sdk::models::{ChargeRequest, ChargeResponse, Invoice, PaymentSummary};

use super::handlers;

const API_TAG: &str = "API Contracts \u{2014} Payments";

/// Register all `PaymentApi` REST routes on the given router.
///
/// `service` is resolved upstream from the [`toolkit::ClientHub`] as
/// `Arc<dyn PaymentApi>`, so the REST layer depends only on the SDK
/// contract — the concrete `PaymentDomainService` is invisible here.
#[allow(clippy::needless_pass_by_value)]
pub fn register_routes(
    mut router: Router,
    openapi: &dyn OpenApiRegistry,
    service: Arc<dyn PaymentApi>,
) -> Router {
    // POST /api/api-contracts/v1/payments/charge
    router = OperationBuilder::post("/api/api-contracts/v1/payments/charge")
        .operation_id("api_contracts.charge")
        .summary("Charge a payment")
        .description("Create a new payment in `pending` status and return its identifier.")
        .tag(API_TAG)
        .authenticated()
        .no_license_required()
        .json_request::<ChargeRequest>(openapi, "Charge request")
        .handler(handlers::charge_handler)
        .json_response_with_schema::<ChargeResponse>(openapi, StatusCode::OK, "Charge accepted")
        .standard_errors(openapi)
        .register(router, openapi);

    // GET /api/api-contracts/v1/invoices/{invoice_id}
    router = OperationBuilder::get("/api/api-contracts/v1/invoices/{invoice_id}")
        .operation_id("api_contracts.get_invoice")
        .summary("Get an invoice by ID")
        .description("Return the invoice record for the given payment identifier.")
        .tag(API_TAG)
        .authenticated()
        .no_license_required()
        .path_param("invoice_id", "Payment / invoice UUID")
        .handler(handlers::get_invoice_handler)
        .json_response_with_schema::<Invoice>(openapi, StatusCode::OK, "Invoice")
        .standard_errors(openapi)
        .register(router, openapi);

    // GET /api/api-contracts/v1/payments — SSE stream of PaymentSummary items.
    router = OperationBuilder::get("/api/api-contracts/v1/payments")
        .operation_id("api_contracts.list_payments")
        .summary("List payments (SSE)")
        .description(
            "Server-sent event stream of payment summaries, optionally filtered by \
             status and currency. The stream is terminated by a synthetic \
             `event: done` frame.",
        )
        .tag(API_TAG)
        .authenticated()
        .no_license_required()
        .query_param(
            "status",
            false,
            "Filter by payment status (pending|completed|failed)",
        )
        .query_param("currency", false, "Filter by ISO 4217 currency code")
        .handler(handlers::list_payments_handler)
        .sse_json::<PaymentSummary>(openapi, "SSE stream of PaymentSummary items")
        .standard_errors(openapi)
        .register(router, openapi);

    router.layer(Extension(service))
}

// `Arc<dyn PaymentApi>` is passed via axum [`Extension`] rather than via
// `ClientHub` lookups inside the handler so the handler stays trivially
// testable and the wiring is explicit at registration time.
