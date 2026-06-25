//! Axum REST handler for the **manual** `PaymentApi` route.
//!
//! Only `list_payments` (SSE) is hand-written here — it opts out of macro
//! generation via `#[server_manual]` on the projection trait. The unary
//! `charge` / `get_invoice` handlers are macro-generated inside
//! `register_payment_api_rest_routes()` and no longer live in this crate.
//!
//! The handler receives an `Extension<SecurityContext>` populated upstream by
//! the gateway middleware (or by a test scaffold). It never parses the
//! `Authorization` header itself — that would re-implement gateway
//! responsibilities inside the module.

use std::convert::Infallible;
use std::sync::Arc;
use std::time::Duration;

use api_contracts_sdk::contract::PaymentApi;
use api_contracts_sdk::models::ListPaymentsFilter;
use axum::Extension;
use axum::extract::Query;
use axum::response::sse::{Event, KeepAlive, Sse};
use futures_util::stream::{self, StreamExt as _};
use toolkit::api::canonical_prelude::Problem;
use toolkit_canonical_errors::CanonicalError;
use toolkit_security::SecurityContext;

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
