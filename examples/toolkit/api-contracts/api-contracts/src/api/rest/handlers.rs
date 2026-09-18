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
use api_contracts_sdk::models::{ListPaymentsFilter, PaymentSummary};
use axum::Extension;
use axum::response::sse::{Event, KeepAlive, Sse};
use futures_util::stream::{self, StreamExt as _};
use toolkit::api::canonical_prelude::Problem;
use toolkit::http::multipart::MultipartJsonStream;
use toolkit_canonical_errors::CanonicalError;
use toolkit_contract::query::QueryParamsExtractor;
use toolkit_security::SecurityContext;

/// `GET /api-contracts/v1/payments` — SSE stream of `PaymentSummary`.
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
    // The contract layer's extractor, not `axum::extract::Query`: it decodes
    // with the same `serde_html_form` codec the generated client encodes with,
    // so this manual route stays wire-compatible with the generated ones.
    QueryParamsExtractor(filter): QueryParamsExtractor<ListPaymentsFilter>,
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

/// `GET /api-contracts/v1/payments/feed` — `multipart/mixed` stream of
/// `PaymentSummary`, with a fallible open.
///
/// Read against [`list_payments_handler`] above, this is the whole shape
/// difference:
///
/// - the open is `.await?`ed, so an unacceptable filter becomes a real
///   `4xx` + `application/problem+json` response and **no** stream is started.
///   The SSE handler cannot do that for a filter error, because its service
///   call hands back a stream that has already begun;
/// - items are framed by [`MultipartJsonStream`], one JSON part each with an
///   exact `Content-Length`, and the boundary is generated per response;
/// - there is no synthetic terminal frame. A `multipart/mixed` stream ends with
///   the closing delimiter, and a graceful end is a clean end either way — the
///   reader manufactures no missing-terminator error, unlike SSE.
///
/// A per-item failure cannot become an HTTP status once the `200` is on the
/// wire, so the service's `Err(CanonicalError)` is framed by
/// [`MultipartJsonStream`] as a typed `application/problem+json` **error part**
/// followed by the close delimiter. The client surfaces it as a typed
/// `Err(CanonicalError)` (category preserved) and a clean end — not a silently
/// short stream, and not an abort. The body is only ever aborted for a genuine
/// transport fault — an `Ok` data item that cannot serialize or exceeds the
/// maximum part size — never for a domain error (an oversized error `Problem` is
/// trimmed to its category rather than truncating the stream).
///
/// # Errors
/// Returns a [`Problem`] from the **open** — authentication, or a filter the
/// service rejects — before any part is written.
///
/// The handler returns `Problem`: `stream_payments` fails with a
/// `CanonicalError` (the open's rejected filter or missing partition position),
/// which `From<CanonicalError> for Problem` renders as
/// `application/problem+json`. `IntoResponse for Problem` takes the status
/// straight off `problem.status`. A richer typed open error — one whose payload
/// fields survive the wire so a client can branch on them — is deferred to a
/// future canonical-error-macro change (#4734).
pub async fn payment_feed_handler(
    Extension(ctx): Extension<SecurityContext>,
    Extension(svc): Extension<Arc<dyn PaymentApi>>,
    QueryParamsExtractor(filter): QueryParamsExtractor<ListPaymentsFilter>,
) -> Result<
    MultipartJsonStream<
        impl futures_core::Stream<Item = Result<PaymentSummary, CanonicalError>> + Send + 'static,
    >,
    Problem,
> {
    // The open. Everything it can reject is rejected here, as a normal
    // `application/problem+json` response with a real status — no stream has
    // been started, so there is nothing to truncate.
    let items = svc
        .stream_payments(ctx, filter)
        .await
        .map_err(Problem::from)?;

    // The service already yields `Result<PaymentSummary, CanonicalError>`: an
    // `Ok` becomes a JSON data part, and a post-open `Err` is framed by
    // `MultipartJsonStream` as a typed `application/problem+json` error part
    // (then a clean close). No per-item shim — the `Result` flows straight
    // through, and `CanonicalError: Into<Problem>` renders the error part.
    Ok(MultipartJsonStream::new(items))
}
