//! End-to-end test for `#[toolkit::rest_contract]` REST client codegen.
//!
//! Spins up an Axum server, points the generated client at it, and exercises
//! the unary + streaming + retry paths.

#![cfg(feature = "rest-client")]
#![allow(clippy::unwrap_used)]

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use axum::extract::{Path, State};
use axum::response::sse::{Event, Sse};
use axum::routing::{get, post};
use axum::{Json, Router};
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use std::pin::Pin;
use std::time::Duration;
use toolkit_canonical_errors::{CanonicalError, Problem};
use toolkit_contract::runtime::config::{ClientConfig, RetryConfig};
use toolkit_contract::runtime::transport_error::TransportError;
use toolkit_contract::{contract, rest_contract};
use toolkit_security::SecurityContext;

use futures_core::Stream;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, utoipa::ToSchema)]
pub struct EchoRequest {
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, utoipa::ToSchema)]
pub struct EchoResponse {
    pub echoed: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Tick {
    pub seq: u64,
}

#[derive(Debug, thiserror::Error)]
pub enum DemoError {
    #[error("transport error: {0}")]
    Transport(#[from] TransportError),
}

// The generated server routes (feature = "rest-server") require the handler's
// error type to be `IntoResponse` and the request/response DTOs to be
// `RequestApiDto`/`ResponseApiDto` (+ `ToSchema`). These impls exist only so
// the server codegen compiles in this crate's own tests; the tests exercise
// the client against hand-written Axum servers.
impl axum::response::IntoResponse for DemoError {
    fn into_response(self) -> axum::response::Response {
        (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            self.to_string(),
        )
            .into_response()
    }
}

mod _api_dto_markers {
    use super::{EchoRequest, EchoResponse};
    use toolkit::api::api_dto::{RequestApiDto, ResponseApiDto};

    impl RequestApiDto for EchoRequest {}
    impl ResponseApiDto for EchoResponse {}
}

pub type DemoStream<T> = Pin<Box<dyn Stream<Item = Result<T, DemoError>> + Send + 'static>>;

#[contract(gear = "demo", version = "v1")]
pub trait DemoApi: Send + Sync {
    #[idempotency(SafeRead)]
    async fn echo_get(&self, ctx: SecurityContext, id: String) -> Result<EchoResponse, DemoError>;

    #[idempotency(NonIdempotentWrite)]
    async fn echo_post(
        &self,
        ctx: SecurityContext,
        req: EchoRequest,
    ) -> Result<EchoResponse, DemoError>;

    #[idempotency(SafeRead)]
    async fn flaky_get(&self, ctx: SecurityContext, id: String) -> Result<EchoResponse, DemoError>;

    #[idempotency(SafeRead)]
    #[streaming]
    fn ticks(&self, ctx: SecurityContext, count: u64) -> Result<Tick, DemoError>;

    // Unit `Ok` type: exercises the empty/204 success-body path (M-4).
    #[idempotency(NonIdempotentWrite)]
    async fn remove(&self, ctx: SecurityContext, id: String) -> Result<(), DemoError>;
}

#[rest_contract(base_path = "/api/demo/v1")]
pub trait DemoApiRest: DemoApi {
    #[get("/echo/{id}")]
    async fn echo_get(&self, ctx: SecurityContext, id: String) -> Result<EchoResponse, DemoError>;

    #[post("/echo")]
    async fn echo_post(
        &self,
        ctx: SecurityContext,
        req: EchoRequest,
    ) -> Result<EchoResponse, DemoError>;

    #[get("/flaky/{id}")]
    #[retryable]
    async fn flaky_get(&self, ctx: SecurityContext, id: String) -> Result<EchoResponse, DemoError>;

    // `#[server_manual]`: streaming (SSE) server routes are not auto-generated;
    // the client codegen still covers `ticks`. Registered by hand where needed.
    #[get("/ticks")]
    #[streaming]
    #[server_manual]
    fn ticks(&self, ctx: SecurityContext, count: u64) -> Result<Tick, DemoError>;

    #[delete("/thing/{id}")]
    async fn remove(&self, ctx: SecurityContext, id: String) -> Result<(), DemoError>;
}

// --- Server ---------------------------------------------------------------

#[derive(Clone, Default)]
struct ServerState {
    flaky_attempts: Arc<AtomicU32>,
    /// Records the `Accept` header seen by the streaming handler, so tests can
    /// assert the client advertised `text/event-stream`.
    last_accept: Arc<std::sync::Mutex<Option<String>>>,
}

async fn echo_get_handler(Path(id): Path<String>) -> Json<EchoResponse> {
    // `id == "slow"` sleeps past a tight client timeout, exercising the
    // per-attempt unary deadline.
    if id == "slow" {
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    Json(EchoResponse {
        echoed: format!("get:{id}"),
    })
}

async fn echo_post_handler(Json(req): Json<EchoRequest>) -> Json<EchoResponse> {
    Json(EchoResponse {
        echoed: format!("post:{}", req.message),
    })
}

async fn flaky_handler(
    State(state): State<ServerState>,
    Path(id): Path<String>,
) -> Result<Json<EchoResponse>, (http::StatusCode, Json<Problem>)> {
    let n = state.flaky_attempts.fetch_add(1, Ordering::SeqCst);
    if n < 1 {
        // Canonical Problem (RFC 9457 + GTS URI in `type`).
        let problem = Problem::from(CanonicalError::service_unavailable().create());
        Err((http::StatusCode::SERVICE_UNAVAILABLE, Json(problem)))
    } else {
        Ok(Json(EchoResponse {
            echoed: format!("flaky:{id}"),
        }))
    }
}

async fn ticks_handler(
    State(state): State<ServerState>,
    headers: axum::http::HeaderMap,
    axum::extract::Query(params): axum::extract::Query<TicksParams>,
) -> Sse<impl Stream<Item = Result<Event, std::convert::Infallible>>> {
    if let Some(v) = headers
        .get(axum::http::header::ACCEPT)
        .and_then(|h| h.to_str().ok())
    {
        *state.last_accept.lock().unwrap() = Some(v.to_owned());
    }
    let count = params.count;
    let stream = futures_util::stream::iter(0..count)
        .map(|seq| {
            let tick = Tick { seq };
            let data = serde_json::to_string(&tick).unwrap();
            Ok(Event::default().data(data))
        })
        .chain(futures_util::stream::once(async {
            Ok(Event::default().event("done"))
        }));
    Sse::new(stream)
}

#[derive(Deserialize)]
struct TicksParams {
    count: u64,
}

// Returns `204 No Content` with an empty body — the client method returns
// `Result<(), _>`, exercising the empty-success-body decode path (M-4).
async fn remove_handler(Path(_id): Path<String>) -> http::StatusCode {
    http::StatusCode::NO_CONTENT
}

async fn start_server() -> (String, ServerState) {
    let state = ServerState::default();
    let app = Router::new()
        .route("/api/demo/v1/echo/{id}", get(echo_get_handler))
        .route("/api/demo/v1/echo", post(echo_post_handler))
        .route(
            "/api/demo/v1/flaky/{id}",
            get(flaky_handler).with_state(state.clone()),
        )
        .route(
            "/api/demo/v1/ticks",
            get(ticks_handler).with_state(state.clone()),
        )
        .route(
            "/api/demo/v1/thing/{id}",
            axum::routing::delete(remove_handler),
        );

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (format!("http://{addr}"), state)
}

/// SSE handler that sleeps ~150ms before each event, so the inter-event gap
/// exceeds a tight unary timeout but stays under a generous SSE idle timeout.
async fn delayed_ticks_handler(
    axum::extract::Query(params): axum::extract::Query<TicksParams>,
) -> Sse<impl Stream<Item = Result<Event, std::convert::Infallible>>> {
    let count = params.count;
    let stream = futures_util::stream::iter(0..count)
        .then(|seq| async move {
            tokio::time::sleep(Duration::from_millis(150)).await;
            let data = serde_json::to_string(&Tick { seq }).unwrap();
            Ok(Event::default().data(data))
        })
        .chain(futures_util::stream::once(async {
            Ok(Event::default().event("done"))
        }));
    Sse::new(stream)
}

async fn start_delayed_ticks_server() -> String {
    let app = Router::new().route("/api/demo/v1/ticks", get(delayed_ticks_handler));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    format!("http://{addr}")
}

#[derive(Clone, Default)]
struct ReconnectState {
    /// 0 on the first connection, >=1 on every reconnect attempt.
    attempt: Arc<AtomicU32>,
    /// The `Last-Event-ID` header value seen on the most recent connection.
    last_event_id_seen: Arc<std::sync::Mutex<Option<String>>>,
}

/// Simulates a mid-stream disconnect: the FIRST connection sends one event
/// (with an `id:`) then the stream simply ENDS with no `event: done` — the
/// case the reconnect fix (see `runtime::sse::SseStream::saw_done_event`)
/// exists for. The SECOND connection (the reconnect) records the incoming
/// `Last-Event-ID` header and completes normally.
async fn reconnecting_ticks_handler(
    State(state): State<ReconnectState>,
    headers: axum::http::HeaderMap,
) -> Sse<Pin<Box<dyn Stream<Item = Result<Event, std::convert::Infallible>> + Send>>> {
    let attempt = state.attempt.fetch_add(1, Ordering::SeqCst);
    if let Some(v) = headers.get("last-event-id").and_then(|h| h.to_str().ok()) {
        *state.last_event_id_seen.lock().unwrap() = Some(v.to_owned());
    }

    if attempt == 0 {
        let stream = futures_util::stream::once(async {
            Ok(Event::default()
                .id("1")
                .data(serde_json::to_string(&Tick { seq: 0 }).unwrap()))
        });
        Sse::new(Box::pin(stream))
    } else {
        let stream = futures_util::stream::once(async {
            Ok(Event::default()
                .id("2")
                .data(serde_json::to_string(&Tick { seq: 1 }).unwrap()))
        })
        .chain(futures_util::stream::once(async {
            Ok(Event::default().event("done"))
        }));
        Sse::new(Box::pin(stream))
    }
}

async fn start_reconnect_server() -> (String, ReconnectState) {
    let state = ReconnectState::default();
    let app = Router::new().route(
        "/api/demo/v1/ticks",
        get(reconnecting_ticks_handler).with_state(state.clone()),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (format!("http://{addr}"), state)
}

fn anonymous_ctx() -> SecurityContext {
    SecurityContext::anonymous()
}

// --- Tests ----------------------------------------------------------------

/// PRD #1536 D3: the generated client implements both the base trait
/// (real method bodies) and the projection trait (delegating defaults).
/// This test asserts both views are reachable through `Arc<dyn _>`.
#[tokio::test]
async fn projection_trait_is_implementable_for_generated_client() {
    let (base_url, _) = start_server().await;
    let client = std::sync::Arc::new(DemoApiRestClient::new(ClientConfig::new(base_url)).unwrap());

    let as_base: std::sync::Arc<dyn DemoApi> = client.clone();
    let as_projection: std::sync::Arc<dyn DemoApiRest> = client.clone();

    // Calling through the projection delegates through the base trait — it
    // must produce the same result as calling the base directly.
    let via_projection = DemoApiRest::echo_get(&*as_projection, anonymous_ctx(), "abc".to_owned())
        .await
        .unwrap();
    let via_base = DemoApi::echo_get(&*as_base, anonymous_ctx(), "abc".to_owned())
        .await
        .unwrap();
    assert_eq!(via_projection.echoed, via_base.echoed);
}

#[tokio::test]
async fn unary_delete_empty_body_decodes_to_unit() {
    // A `204 No Content` (empty body) on a `Result<(), _>` method must decode
    // to `Ok(())` rather than failing JSON deserialization (M-4).
    let (base_url, _) = start_server().await;
    let client = DemoApiRestClient::new(ClientConfig::new(base_url)).unwrap();
    DemoApi::remove(&client, anonymous_ctx(), "abc".to_owned())
        .await
        .expect("204/empty body should decode to Ok(())");
}

#[tokio::test]
async fn unary_get_round_trip() {
    let (base_url, _) = start_server().await;
    let client = DemoApiRestClient::new(ClientConfig::new(base_url)).unwrap();
    let resp = DemoApi::echo_get(&client, anonymous_ctx(), "abc".to_owned())
        .await
        .unwrap();
    assert_eq!(resp.echoed, "get:abc");
}

#[tokio::test]
async fn unary_post_round_trip() {
    let (base_url, _) = start_server().await;
    let client = DemoApiRestClient::new(ClientConfig::new(base_url)).unwrap();
    let resp = DemoApi::echo_post(
        &client,
        anonymous_ctx(),
        EchoRequest {
            message: "hi".into(),
        },
    )
    .await
    .unwrap();
    assert_eq!(resp.echoed, "post:hi");
}

#[tokio::test]
async fn retryable_recovers_after_transient_failure() {
    let (base_url, state) = start_server().await;
    let cfg = ClientConfig::new(base_url).with_retry(RetryConfig {
        max_attempts: 4,
        base_delay: Duration::from_millis(0),
        max_delay: Duration::from_millis(0),
        multiplier: 1.0,
    });
    let client = DemoApiRestClient::new(cfg).unwrap();
    let resp = DemoApi::flaky_get(&client, anonymous_ctx(), "xyz".to_owned())
        .await
        .unwrap();
    assert_eq!(resp.echoed, "flaky:xyz");
    assert!(state.flaky_attempts.load(Ordering::SeqCst) >= 2);
}

#[tokio::test]
async fn unary_timeout_is_enforced() {
    // A tight `ClientConfig::timeout` must bound the unary call; the slow
    // handler sleeps 500ms while the client deadline is 50ms.
    let (base_url, _) = start_server().await;
    let cfg = ClientConfig::new(base_url).with_timeout(Duration::from_millis(50));
    let client = DemoApiRestClient::new(cfg).unwrap();
    let err = DemoApi::echo_get(&client, anonymous_ctx(), "slow".to_owned())
        .await
        .unwrap_err();
    assert!(
        matches!(err, DemoError::Transport(TransportError::Timeout(_))),
        "expected a timeout, got {err:?}"
    );
}

#[tokio::test]
async fn require_tls_rejects_plaintext_endpoint() {
    // `ClientConfig::with_require_tls(true)` must make the generated client
    // refuse a plaintext `http://` endpoint rather than silently sending the
    // (bearer-carrying) request over it.
    let (base_url, _) = start_server().await;
    let cfg = ClientConfig::new(base_url).with_require_tls(true);
    let client = DemoApiRestClient::new(cfg).unwrap();
    let err = DemoApi::echo_get(&client, anonymous_ctx(), "abc".to_owned())
        .await
        .unwrap_err();
    assert!(
        matches!(err, DemoError::Transport(TransportError::Network(_))),
        "expected the plaintext request to be rejected as a network error, got {err:?}"
    );
}

#[tokio::test]
async fn default_config_still_allows_plaintext_endpoint() {
    // `require_tls` defaults to `false` — existing in-mesh plaintext usage
    // must be unaffected by its introduction.
    let (base_url, _) = start_server().await;
    let cfg = ClientConfig::new(base_url);
    assert!(!cfg.require_tls);
    let client = DemoApiRestClient::new(cfg).unwrap();
    DemoApi::echo_get(&client, anonymous_ctx(), "abc".to_owned())
        .await
        .expect("plaintext must still work by default");
}

#[tokio::test]
async fn streaming_uses_idle_timeout_not_unary_timeout() {
    // A tight unary timeout (30ms) must NOT kill a healthy stream whose events
    // are 150ms apart; the generous SSE idle timeout (5s) governs instead.
    let base_url = start_delayed_ticks_server().await;
    let cfg = ClientConfig::new(base_url)
        .with_timeout(Duration::from_millis(30))
        .with_sse_idle_timeout(Duration::from_secs(5));
    let client = DemoApiRestClient::new(cfg).unwrap();
    let stream = DemoApi::ticks(&client, anonymous_ctx(), 2);
    let items: Vec<Tick> = stream
        .collect::<Vec<_>>()
        .await
        .into_iter()
        .collect::<Result<_, _>>()
        .expect("slow stream must not be killed by the unary timeout");
    assert_eq!(items.len(), 2);
}

#[tokio::test]
async fn streaming_sends_accept_event_stream_header() {
    // PRD §5.6: the generated streaming client must advertise the SSE media
    // type on the connect request.
    let (base_url, state) = start_server().await;
    let client = DemoApiRestClient::new(ClientConfig::new(base_url)).unwrap();
    let stream = DemoApi::ticks(&client, anonymous_ctx(), 1);
    let _items: Vec<_> = stream.collect().await;
    let accept = state.last_accept.lock().unwrap().clone();
    assert_eq!(accept.as_deref(), Some("text/event-stream"));
}

#[tokio::test]
async fn streaming_yields_typed_items() {
    let (base_url, _) = start_server().await;
    let client = DemoApiRestClient::new(ClientConfig::new(base_url)).unwrap();
    let stream = DemoApi::ticks(&client, anonymous_ctx(), 3);
    let items: Vec<Tick> = stream
        .collect::<Vec<_>>()
        .await
        .into_iter()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(items.len(), 3);
    assert_eq!(items[0].seq, 0);
    assert_eq!(items[2].seq, 2);
}

/// Exercises the SSE reconnect codegen on the happy path. With reconnect
/// enabled but the server delivering all events without interruption, the
/// stream completes normally — the factory closure is invoked exactly once
/// (no retries needed).
///
/// A genuine mid-stream disconnect (no `event: done`, connection just ends) is
/// covered end-to-end by `streaming_reconnects_and_sends_last_event_id_after_doneless_disconnect`
/// below, which drives an actual reconnect + `Last-Event-ID` round-trip
/// through a real HTTP server (no connection-drop machinery needed: an SSE
/// response that simply ends without `done` IS the disconnect case, per the
/// `saw_done_event()` distinction in `runtime::sse`).
#[tokio::test]
async fn streaming_with_reconnect_config_happy_path() {
    let (base_url, _) = start_server().await;
    let cfg = ClientConfig::new(base_url).with_sse_reconnect(
        toolkit_contract::runtime::config::ReconnectConfig::enabled(3, Duration::from_millis(1)),
    );
    let client = DemoApiRestClient::new(cfg).unwrap();
    let stream = DemoApi::ticks(&client, anonymous_ctx(), 2);
    let items: Vec<Tick> = stream
        .collect::<Vec<_>>()
        .await
        .into_iter()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(items.len(), 2);
    assert_eq!(items[0].seq, 0);
    assert_eq!(items[1].seq, 1);
}

/// A real, HTTP-level mid-stream disconnect: the first connection ends
/// WITHOUT an `event: done` frame (the anomalous case fixed alongside #14 —
/// previously this was silently treated as a successful, complete stream).
/// Asserts the client (a) reconnects instead of reporting success, (b) sends
/// `Last-Event-ID` on the reconnect request carrying the id captured from the
/// first connection, and (c) delivers every item across both connections.
#[tokio::test]
async fn streaming_reconnects_and_sends_last_event_id_after_doneless_disconnect() {
    let (base_url, state) = start_reconnect_server().await;
    let cfg = ClientConfig::new(base_url).with_sse_reconnect(
        toolkit_contract::runtime::config::ReconnectConfig::enabled(3, Duration::from_millis(1)),
    );
    let client = DemoApiRestClient::new(cfg).unwrap();
    let stream = DemoApi::ticks(&client, anonymous_ctx(), 0);
    let items: Vec<Tick> = stream
        .collect::<Vec<_>>()
        .await
        .into_iter()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(items.len(), 2, "expected one item from each connection");
    assert_eq!(items[0].seq, 0);
    assert_eq!(items[1].seq, 1);
    assert_eq!(
        state.last_event_id_seen.lock().unwrap().as_deref(),
        Some("1"),
        "reconnect request must carry Last-Event-ID from the first connection"
    );
}

#[tokio::test]
async fn server_problem_envelope_round_trips() {
    let (base_url, _) = start_server().await;
    let cfg = ClientConfig::new(base_url).with_retry(RetryConfig::off());
    let client = DemoApiRestClient::new(cfg).unwrap();
    let err = DemoApi::flaky_get(&client, anonymous_ctx(), "xyz".to_owned())
        .await
        .unwrap_err();
    match err {
        DemoError::Transport(TransportError::Problem { problem: p, .. }) => {
            assert_eq!(p.status, 503);
            assert!(p.problem_type.contains("service_unavailable"));
        }
        other @ DemoError::Transport(_) => panic!("unexpected: {other:?}"),
    }
}
