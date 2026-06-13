//! Axum REST handlers for `PaymentApi`.
//!
//! Handlers receive an `Extension<SecurityContext>` populated upstream by
//! the gateway middleware (or by a test scaffold). They never parse the
//! `Authorization` header themselves — that would re-implement gateway
//! responsibilities inside the module and would diverge per-handler.
//!
//! Returns `ApiResult<JsonBody<T>>` (`canonical_prelude` shape, where the
//! error variant is `CanonicalError`). `OperationBuilder` maps the
//! `CanonicalError` into an RFC 9457 `Problem` envelope at the framework
//! boundary.

use std::convert::Infallible;
use std::sync::Arc;
use std::time::Duration;

use api_contracts_sdk::contract::PaymentApi;
use api_contracts_sdk::models::{ChargeRequest, ChargeResponse, Invoice, ListPaymentsFilter};
use axum::Extension;
use axum::extract::{Path, Query};
use axum::response::sse::{Event, KeepAlive, Sse};
use futures_util::stream::{self, StreamExt as _};
use toolkit::api::canonical_prelude::{ApiResult, JsonBody, Problem};
use toolkit_canonical_errors::CanonicalError;
use toolkit_security::SecurityContext;

/// `POST /api/api-contracts/v1/payments/charge`
///
/// # Errors
/// Returns a canonical error when the underlying `PaymentApi::charge` call fails.
pub async fn charge_handler(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<dyn PaymentApi>>,
    axum::Json(req): axum::Json<ChargeRequest>,
) -> ApiResult<JsonBody<ChargeResponse>> {
    let resp = svc.charge(ctx, req).await?;
    Ok(axum::Json(resp))
}

/// `GET /api/api-contracts/v1/invoices/{invoice_id}`
///
/// # Errors
/// Returns a canonical error when the underlying `PaymentApi::get_invoice` call fails.
pub async fn get_invoice_handler(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<dyn PaymentApi>>,
    Path(invoice_id): Path<String>,
) -> ApiResult<JsonBody<Invoice>> {
    let invoice = svc.get_invoice(ctx, invoice_id).await?;
    Ok(axum::Json(invoice))
}

/// `GET /api/api-contracts/v1/payments` — SSE stream of `PaymentSummary`.
///
/// Authentication failures bubble out as a `CanonicalError` BEFORE the
/// `text/event-stream` upgrade happens, so the client sees a proper
/// `application/problem+json` response in that case. Once the stream
/// has started, per-item errors are emitted as `event: error` frames so
/// the connection state stays consistent.
///
/// # Errors
/// Returns a canonical error before the SSE upgrade if the request is rejected (e.g. authentication failure).
///
/// # Panics
/// Panics if a `PaymentSummary` or `Problem` cannot be serialized to JSON. Both types are
/// internal owned-data shapes whose `Serialize` impl is infallible by construction.
#[allow(
    clippy::expect_used,
    reason = "PaymentSummary and Problem are local owned-data types whose derived Serialize implementations cannot fail; serde_json::to_string on them is infallible in practice. The expect message documents this invariant rather than handling an impossible Err path."
)]
pub async fn list_payments_handler(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<dyn PaymentApi>>,
    Query(filter): Query<ListPaymentsFilter>,
) -> Result<Sse<impl futures_core::Stream<Item = Result<Event, Infallible>>>, CanonicalError> {
    let item_stream = svc.list_payments(ctx, filter);

    let event_stream = item_stream
        .map(|item| {
            let event = match item {
                Ok(summary) => {
                    let data = serde_json::to_string(&summary)
                        .expect("PaymentSummary serialization is infallible");
                    Event::default().data(data)
                }
                Err(e) => {
                    let problem: Problem = e.into();
                    let data = serde_json::to_string(&problem)
                        .expect("Problem serialization is infallible");
                    Event::default().event("error").data(data)
                }
            };
            Ok(event)
        })
        .chain(stream::once(async { Ok(Event::default().event("done")) }));

    Ok(Sse::new(event_stream).keep_alive(KeepAlive::new().interval(Duration::from_secs(15))))
}
