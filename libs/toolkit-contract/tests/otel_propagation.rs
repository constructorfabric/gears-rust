//! End-to-end proof of ADR-0006's central guarantee: the generated client's
//! per-method `tracing` span is the parent of `toolkit-http`'s `outgoing_http`
//! span, so W3C `traceparent` propagates the CLIENT SPAN's `trace_id` — not
//! some unrelated ID — to the server.
//!
//! No collector/emulator is needed: `trace_id` generation happens when a span
//! is created, independent of whether anything exports it. We build an
//! in-process `SdkTracerProvider` with zero exporters purely so
//! `tracing-opentelemetry` allocates real W3C-shaped trace/span IDs and
//! `toolkit-http`'s `OtelLayer` (gated by the `otel` feature) has a live
//! `opentelemetry::Context` to inject from.

#![cfg(all(feature = "rest-client", feature = "otel"))]
#![allow(clippy::unwrap_used)]

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use axum::extract::State;
use axum::routing::get;
use axum::{Router, http::HeaderMap};
use opentelemetry::global;
use opentelemetry_sdk::propagation::TraceContextPropagator;
use opentelemetry_sdk::trace::SdkTracerProvider;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use tracing_opentelemetry::OpenTelemetrySpanExt;
use tracing_subscriber::layer::SubscriberExt;

use toolkit_contract::runtime::config::ClientConfig;
use toolkit_contract::runtime::transport_error::TransportError;
use toolkit_contract::{contract, rest_contract};
use toolkit_http::otel::parse_trace_id;
use toolkit_security::SecurityContext;

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct Pong {
    pub ok: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum PingError {
    #[error("transport error: {0}")]
    Transport(#[from] TransportError),
}

// The generated server routes (feature = "rest-server", enabled under
// `--all-features`) require the handler's error type to be `IntoResponse` and
// the response DTO to be `ResponseApiDto` (+ `ToSchema`). These impls exist so
// the server codegen compiles; this test only exercises the client path.
impl axum::response::IntoResponse for PingError {
    fn into_response(self) -> axum::response::Response {
        (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            self.to_string(),
        )
            .into_response()
    }
}

mod _api_dto_markers {
    use super::Pong;
    use toolkit::api::api_dto::ResponseApiDto;

    impl ResponseApiDto for Pong {}
}

#[contract(gear = "otel-demo", version = "v1")]
pub trait PingApi: Send + Sync {
    async fn ping(&self, ctx: SecurityContext) -> Result<Pong, PingError>;
}

#[rest_contract(base_path = "/api/otel/v1")]
pub trait PingApiRest: PingApi {
    #[get("/ping")]
    async fn ping(&self, ctx: SecurityContext) -> Result<Pong, PingError>;
}

/// Shared slot the handler stashes the incoming `traceparent` header into.
#[derive(Clone, Default)]
struct Captured(Arc<Mutex<Option<String>>>);

async fn ping_handler(State(captured): State<Captured>, headers: HeaderMap) -> axum::Json<Pong> {
    let traceparent = headers
        .get("traceparent")
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    *captured.0.lock() = traceparent;
    axum::Json(Pong { ok: true })
}

async fn start_server() -> (String, Captured) {
    let captured = Captured::default();
    let app = Router::new()
        .route("/api/otel/v1/ping", get(ping_handler))
        .with_state(captured.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (format!("http://{addr}"), captured)
}

/// Ensures the global W3C propagator is installed exactly once across the
/// crate's test binary. Without it, `toolkit-http`'s `inject_current_span` is
/// a silent no-op (the default global propagator injects nothing).
fn ensure_propagator() {
    static DONE: AtomicBool = AtomicBool::new(false);
    if DONE
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_ok()
    {
        global::set_text_map_propagator(TraceContextPropagator::new());
    }
}

#[tokio::test]
async fn client_span_trace_id_propagates_to_server() {
    ensure_propagator();

    // In-process tracer with NO exporter: trace/span ID generation and
    // Context::current() propagation work regardless of export — we only need
    // real W3C-shaped IDs, not anywhere to send them.
    let provider = SdkTracerProvider::builder().build();
    let tracer = {
        use opentelemetry::trace::TracerProvider as _;
        provider.tracer("otel-propagation-test")
    };
    let otel_layer = tracing_opentelemetry::layer().with_tracer(tracer);
    let subscriber = tracing_subscriber::registry().with(otel_layer);
    // `#[tokio::test]` defaults to a single-threaded (current-thread) runtime,
    // so a thread-local default subscriber is valid across the awaits below.
    let _subscriber_guard = tracing::subscriber::set_default(subscriber);

    let (base_url, captured) = start_server().await;
    let client = PingApiRestClient::new(ClientConfig::new(base_url)).unwrap();

    // Root span for this call. The macro's per-method span (and, beneath it,
    // toolkit-http's `outgoing_http` span) become its children, so all three
    // share this trace_id — proving the propagation chain end to end.
    let root = tracing::info_span!("test_root");
    let client_trace_id = {
        let cx = root.context();
        let otel_span = opentelemetry::trace::TraceContextExt::span(&cx);
        otel_span.span_context().trace_id().to_string()
    };
    // Guard against a false-positive pass where propagation is silently broken
    // and both sides independently observe the OTel "invalid" all-zero
    // trace_id — which would make the equality assertion below pass for the
    // wrong reason.
    assert_ne!(
        client_trace_id,
        opentelemetry::trace::TraceId::INVALID.to_string(),
        "client root span must have a real (non-invalid) trace_id"
    );

    {
        use tracing::Instrument as _;
        PingApi::ping(&client, SecurityContext::anonymous())
            .instrument(root)
            .await
            .unwrap();
    }

    let traceparent = captured
        .0
        .lock()
        .clone()
        .expect("server must have received a traceparent header");
    let server_trace_id =
        parse_trace_id(&traceparent).expect("traceparent must be a valid W3C header");

    assert_eq!(
        server_trace_id, client_trace_id,
        "the trace_id the server observed must be the same trace the client's root span started \
         (client span -> outgoing_http span -> wire), not an unrelated ID"
    );
}
