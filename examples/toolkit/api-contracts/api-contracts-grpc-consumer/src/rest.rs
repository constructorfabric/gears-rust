//! REST routes of the gRPC consumer. Each one makes one gRPC call to the
//! provider: a unary write, a unary read, and a server stream.

use std::sync::Arc;

use axum::body::Body;
use axum::extract::{Path, Query};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse as _, Response};
use axum::{Extension, Json, Router};
use futures_util::StreamExt as _;
use toolkit::api::OpenApiRegistry;
use toolkit::api::operation_builder::OperationBuilder;
use toolkit_canonical_errors::{CanonicalError, Problem};
use toolkit_security::SecurityContext;

use api_contracts_sdk::contract::PaymentStream;
use api_contracts_sdk::models::{
    ChargeRequest, ChargeResponse, Invoice, ListPaymentsFilter, PaymentSummary,
};

use crate::domain::PaymentProxyService;

const API_TAG: &str = "API Contracts \u{2014} gRPC Consumer";
const BASE: &str = "/api-contracts-grpc-consumer/v1";

/// Register the routes. All three are exposed: the edge publishes them.
#[allow(clippy::needless_pass_by_value)]
pub fn register_routes(
    router: Router,
    openapi: &dyn OpenApiRegistry,
    service: Arc<PaymentProxyService>,
) -> Router {
    let router = OperationBuilder::post(format!("{BASE}/charge"))
        .operation_id("api_contracts_grpc_consumer.charge")
        .summary("Charge through PaymentApi over gRPC")
        .tag(API_TAG)
        .authenticated()
        .exposed()
        .no_license_required()
        .json_request::<ChargeRequest>(openapi, "")
        .handler({
            let service = Arc::clone(&service);
            move |Extension(ctx): Extension<SecurityContext>, Json(req): Json<ChargeRequest>| {
                let service = Arc::clone(&service);
                async move { service.charge(ctx, req).await.map(Json) }
            }
        })
        .json_response_with_schema::<ChargeResponse>(openapi, StatusCode::OK, "")
        .standard_errors(openapi)
        .register(router, openapi);

    let router = OperationBuilder::get(format!("{BASE}/invoices/{{invoice_id}}"))
        .operation_id("api_contracts_grpc_consumer.get_invoice")
        .summary("Fetch an invoice through PaymentApi over gRPC")
        .tag(API_TAG)
        .authenticated()
        .exposed()
        .no_license_required()
        .path_param("invoice_id", "Invoice id (the payment id)")
        .handler({
            let service = Arc::clone(&service);
            move |Extension(ctx): Extension<SecurityContext>, Path(invoice_id): Path<String>| {
                let service = Arc::clone(&service);
                async move { service.invoice(ctx, invoice_id).await.map(Json) }
            }
        })
        .json_response_with_schema::<Invoice>(openapi, StatusCode::OK, "")
        .standard_errors(openapi)
        .register(router, openapi);

    let router = OperationBuilder::get(format!("{BASE}/payments"))
        .operation_id("api_contracts_grpc_consumer.list_payments")
        .summary("List the caller's payments, read from a gRPC server stream")
        .tag(API_TAG)
        .authenticated()
        .exposed()
        .no_license_required()
        .handler({
            let service = Arc::clone(&service);
            move |Extension(ctx): Extension<SecurityContext>| {
                let service = Arc::clone(&service);
                async move { service.payments(ctx).await.map(Json) }
            }
        })
        .json_array_response_with_schema::<PaymentSummary>(openapi, StatusCode::OK, "")
        .standard_errors(openapi)
        .register(router, openapi);

    OperationBuilder::get(format!("{BASE}/payments/feed"))
        .operation_id("api_contracts_grpc_consumer.payment_feed")
        .summary("Relay the provider's gRPC payment stream as NDJSON")
        .description(
            "Opens `stream_payments` over gRPC and writes each payment as one JSON \
             line as it arrives. A filter the provider rejects at open fails the \
             request with a Problem before any line is written; an error after \
             the open ends the body with an `{\"error\": <Problem>}` line.",
        )
        .tag(API_TAG)
        .authenticated()
        .exposed()
        .no_license_required()
        .query_params_from::<ListPaymentsFilter>()
        .handler(
            move |Extension(ctx): Extension<SecurityContext>,
                  Query(filter): Query<ListPaymentsFilter>| {
                let service = Arc::clone(&service);
                async move {
                    let stream = service.open_feed(ctx, filter).await?;
                    Ok::<_, CanonicalError>(ndjson(stream))
                }
            },
        )
        .text_response(StatusCode::OK, "one payment per line", NDJSON)
        .standard_errors(openapi)
        .register(router, openapi)
}

const NDJSON: &str = "application/x-ndjson";

/// One JSON line per item. The status is already sent when the stream fails,
/// so a failure becomes a last `{"error": <Problem>}` line.
fn ndjson(stream: PaymentStream<PaymentSummary>) -> Response {
    let lines = stream.map(|item| {
        let value = match item {
            Ok(summary) => serde_json::to_value(summary),
            Err(err) => serde_json::to_value(Problem::from(err))
                .map(|problem| serde_json::json!({ "error": problem })),
        };
        let mut line = value.map_or_else(
            |err| serde_json::json!({ "error": err.to_string() }).to_string(),
            |value| value.to_string(),
        );
        line.push('\n');
        Ok::<_, std::convert::Infallible>(line)
    });
    ([(header::CONTENT_TYPE, NDJSON)], Body::from_stream(lines)).into_response()
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "rest_tests.rs"]
mod rest_tests;
