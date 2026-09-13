//! End-to-end test for `#[toolkit::rest_contract]` REST client codegen.
//!
//! Spins up an Axum server, points the generated client at it, and exercises
//! the unary + streaming + retry paths.

#![cfg(feature = "rest-client")]
#![allow(clippy::unwrap_used)]

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use axum::extract::{Path, State};
use axum::response::IntoResponse as _;
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

    // Fallible open (#4734 D2): `async fn` makes the open a distinct, awaited
    // operation returning `Result<Stream, DemoError>`, so an open-time failure
    // reaches the caller before any item exists.
    #[idempotency(SafeRead)]
    #[streaming(open = fallible)]
    async fn frames(&self, ctx: SecurityContext, since: u64) -> Result<Tick, DemoError>;

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

    #[get("/frames")]
    #[streaming(open = fallible)]
    #[server_manual]
    async fn frames(&self, ctx: SecurityContext, since: u64) -> Result<Tick, DemoError>;

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

#[derive(Deserialize)]
struct FramesParams {
    since: u64,
}

/// `409 PositionsNotSet`-shaped RFC 9457 body, as a domain problem an
/// open-time state machine would act on. Written as a literal rather than
/// built from `CanonicalError` so the test pins the *wire* shape the client
/// parses.
const POSITIONS_NOT_SET_PROBLEM: &str = concat!(
    r#"{"type":"https://example.test/probs/positions-not-set","#,
    r#""title":"PositionsNotSet","status":409,"#,
    r#""detail":"cursors are unseeded; seek before streaming"}"#
);

/// Backs the fallible-open (`async fn`) method. `since` selects the case:
///
/// - `409` — the open fails with a `Problem` body, before any item exists
/// - `1` — the open succeeds, then an item fails to decode
/// - anything else — `since` well-formed frames followed by `event: done`
async fn frames_handler(
    axum::extract::Query(params): axum::extract::Query<FramesParams>,
) -> axum::response::Response {
    match params.since {
        409 => (
            http::StatusCode::CONFLICT,
            [(http::header::CONTENT_TYPE, "application/problem+json")],
            POSITIONS_NOT_SET_PROBLEM,
        )
            .into_response(),
        // Open succeeds; the SECOND frame's `data` is not a valid `Tick`, so
        // the failure can only be an item of an already-opened stream.
        1 => {
            let stream = futures_util::stream::iter(vec![
                Ok::<Event, std::convert::Infallible>(
                    Event::default().data(serde_json::to_string(&Tick { seq: 0 }).unwrap()),
                ),
                Ok(Event::default().data("{\"seq\":\"not-a-number\"}")),
                Ok(Event::default().event("done")),
            ]);
            Sse::new(stream).into_response()
        }
        n => {
            let stream = futures_util::stream::iter(0..n)
                .map(|seq| {
                    Ok::<Event, std::convert::Infallible>(
                        Event::default().data(serde_json::to_string(&Tick { seq }).unwrap()),
                    )
                })
                .chain(futures_util::stream::once(async {
                    Ok(Event::default().event("done"))
                }));
            Sse::new(stream).into_response()
        }
    }
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
        .route("/api/demo/v1/frames", get(frames_handler))
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

/// Accepts the request and then NEVER returns a response — the handler awaits
/// forever, so no status line or headers are ever sent. Models a peer that
/// completes the socket handshake and then goes silent. Without a distinct open
/// deadline the client's stream open waits here forever (#4740 HIGH).
async fn never_responds_handler() -> axum::http::StatusCode {
    std::future::pending::<()>().await;
    axum::http::StatusCode::OK
}

async fn start_silent_open_server() -> String {
    let app = Router::new().route("/api/demo/v1/frames", get(never_responds_handler));
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

/// Delivers one event per connection, holds the connection open for `hold`,
/// then drops the stream with no `event: done`, over and over, until `blips`
/// connections have happened.
///
/// Models a long-lived subscription across a series of unrelated blips —
/// rolling deploys, LB idle timeouts — each separated by a period of delivery.
/// `hold` controls how long each connection stays up: a connection that both
/// delivers an item and stays up at least `min_healthy_uptime` is "healthy" and
/// resets the burst budget; a `hold` of zero is the one-item-then-drop peer of
/// #4740 that must NOT reset it.
async fn flapping_ticks_handler(
    State(state): State<FlappingState>,
) -> Sse<Pin<Box<dyn Stream<Item = Result<Event, std::convert::Infallible>> + Send>>> {
    let n = state.connections.fetch_add(1, Ordering::SeqCst);
    let event = futures_util::stream::once(async move {
        Ok(Event::default()
            .id(n.to_string())
            .data(serde_json::to_string(&Tick { seq: u64::from(n) }).unwrap()))
    });

    if n + 1 >= state.blips {
        // Last connection: finish cleanly so the stream terminates.
        Sse::new(Box::pin(event.chain(futures_util::stream::once(async {
            Ok(Event::default().event("done"))
        }))))
    } else {
        // Deliver, hold the connection open for `hold`, then vanish without
        // `done` — a reconnect-eligible anomaly. The held tail yields no event;
        // it only keeps the byte stream alive so the client measures a non-zero
        // connection uptime.
        let hold = state.hold;
        let held_tail = futures_util::stream::once(async move {
            tokio::time::sleep(hold).await;
            None::<Result<Event, std::convert::Infallible>>
        })
        .filter_map(|x| async move { x });
        Sse::new(Box::pin(event.chain(held_tail)))
    }
}

#[derive(Clone)]
struct FlappingState {
    connections: Arc<AtomicU32>,
    blips: u32,
    /// How long each non-final connection stays up after delivering its item.
    hold: Duration,
}

async fn start_flapping_server(blips: u32, hold: Duration) -> (String, FlappingState) {
    let state = FlappingState {
        connections: Arc::new(AtomicU32::new(0)),
        blips,
        hold,
    };
    let app = Router::new().route(
        "/api/demo/v1/ticks",
        get(flapping_ticks_handler).with_state(state.clone()),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (format!("http://{addr}"), state)
}

/// Answers `200` with SSE headers and then ends the body immediately, WITHOUT
/// `event: done` — the reconnect-eligible anomaly — on every connection,
/// counting connections as it goes.
///
/// Used by the D6 pair: whether the client re-opens after this is exactly what
/// the derived reconnect policy decides. Note it delivers **no** event: a
/// connection that delivers one resets the reconnect budget (see
/// `reconnect_budget_resets_after_a_connection_delivers_events`), so a handler
/// that both delivers and disconnects would re-open forever and never let the
/// `Immediate` half of the assertion terminate.
async fn doneless_handler(
    State(connections): State<Arc<AtomicU32>>,
) -> Sse<Pin<Box<dyn Stream<Item = Result<Event, std::convert::Infallible>> + Send>>> {
    connections.fetch_add(1, Ordering::SeqCst);
    Sse::new(Box::pin(futures_util::stream::empty()))
}

/// One server serving the same doneless-disconnect behaviour on both the
/// `Immediate` (`/ticks`) and `Awaited` (`/frames`) routes, with a separate
/// connection counter per route.
async fn start_doneless_server() -> (String, Arc<AtomicU32>, Arc<AtomicU32>) {
    let ticks_connections = Arc::new(AtomicU32::new(0));
    let frames_connections = Arc::new(AtomicU32::new(0));
    let app = Router::new()
        .route(
            "/api/demo/v1/ticks",
            get(doneless_handler).with_state(Arc::clone(&ticks_connections)),
        )
        .route(
            "/api/demo/v1/frames",
            get(doneless_handler).with_state(Arc::clone(&frames_connections)),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (
        format!("http://{addr}"),
        ticks_connections,
        frames_connections,
    )
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
        .with_stream_idle_timeout(Duration::from_secs(5));
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
async fn streaming_open_is_bounded_by_open_timeout_not_idle_timeout() {
    // #4740 (HIGH): a peer that accepts the socket and never answers must not
    // hang the open forever. The open is bounded by the unary `timeout` (50ms
    // here), which generated code wires as the stream's `open_timeout` — NOT by
    // the generous stream idle timeout (5s), which bounds only the item loop.
    // Asserting the returned `Timeout` carries the 50ms deadline pins which of
    // the two applied to the open.
    let base_url = start_silent_open_server().await;
    let cfg = ClientConfig::new(base_url)
        .with_timeout(Duration::from_millis(50))
        .with_stream_idle_timeout(Duration::from_secs(5));
    let client = DemoApiRestClient::new(cfg).unwrap();
    let opened = DemoApi::frames(&client, anonymous_ctx(), 0).await;
    // The `Err` variant is itself the proof no stream was produced; the boxed
    // stream `Ok` type is not `Debug`, so match rather than `unwrap_err`.
    let Err(DemoError::Transport(err)) = opened else {
        panic!("a silent peer must fail the open, not produce a stream");
    };
    assert!(
        matches!(err, TransportError::Timeout(d) if d == Duration::from_millis(50)),
        "expected the open to time out at the 50ms open deadline, got {err:?}"
    );
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
    let cfg = ClientConfig::new(base_url).with_stream_reconnect(
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
    let cfg = ClientConfig::new(base_url).with_stream_reconnect(
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

/// `max_attempts` bounds a *burst* of consecutive failures, not the number a
/// subscription may survive in total.
///
/// The budget used to increment on every reconnect and never reset, so a
/// long-lived stream died on the `max_attempts + 1`-th blip no matter how much
/// healthy traffic separated them — contradicting the indefinitely-reconnecting
/// behaviour the streaming client advertises. Here four blips run against a
/// budget of one: each connection delivers an event AND stays up past
/// `min_healthy_uptime` (20ms) before dropping, so each such *healthy*
/// connection resets the budget and the stream survives every blip.
#[tokio::test]
async fn reconnect_budget_resets_after_a_healthy_connection() {
    const BLIPS: u32 = 4;
    const MAX_ATTEMPTS: u32 = 1;

    // Hold each connection open (40ms) longer than the healthy threshold (20ms).
    let (base_url, state) = start_flapping_server(BLIPS, Duration::from_millis(40)).await;
    let cfg = ClientConfig::new(base_url).with_stream_reconnect(
        toolkit_contract::runtime::config::ReconnectConfig::enabled(
            MAX_ATTEMPTS,
            Duration::from_millis(1),
        )
        .with_min_healthy_uptime(Duration::from_millis(20)),
    );
    let client = DemoApiRestClient::new(cfg).unwrap();

    let items: Vec<Tick> = DemoApi::ticks(&client, anonymous_ctx(), 0)
        .collect::<Vec<_>>()
        .await
        .into_iter()
        .collect::<Result<_, _>>()
        .expect("a stream of healthy connections must not exhaust its reconnect budget");

    assert_eq!(items.len(), BLIPS as usize, "one item per connection");
    assert_eq!(state.connections.load(Ordering::SeqCst), BLIPS);
}

/// #4740: a peer that delivers one item and immediately drops must NOT reset the
/// budget. Delivering an item is necessary but not sufficient for "healthy" —
/// without a minimum uptime the old code reset the budget every cycle and
/// reopened forever, re-sending the auth token each time. With a `hold` of zero
/// (instant drop) against the default 5s `min_healthy_uptime`, no connection
/// qualifies as healthy, so the burst budget caps the loop at `max_attempts + 1`
/// connections and then surfaces the error rather than looping indefinitely.
#[tokio::test]
async fn reconnect_budget_is_not_reset_by_a_brief_one_item_connection() {
    // Effectively endless flapping (100 » the few connections we expect), each
    // connection delivering one item then dropping instantly.
    const BLIPS: u32 = 100;
    const MAX_ATTEMPTS: u32 = 2;

    let (base_url, state) = start_flapping_server(BLIPS, Duration::ZERO).await;
    let cfg = ClientConfig::new(base_url).with_stream_reconnect(
        // Default 5s min_healthy_uptime: an instant connection never qualifies.
        toolkit_contract::runtime::config::ReconnectConfig::enabled(
            MAX_ATTEMPTS,
            Duration::from_millis(1),
        ),
    );
    let client = DemoApiRestClient::new(cfg).unwrap();

    let results: Vec<_> = DemoApi::ticks(&client, anonymous_ctx(), 0)
        .collect::<Vec<_>>()
        .await;

    let items = results.iter().filter(|r| r.is_ok()).count();
    assert!(
        results.last().is_some_and(std::result::Result::is_err),
        "a one-item-then-drop peer must eventually surface an error, got {results:?}"
    );
    assert_eq!(
        items,
        (MAX_ATTEMPTS + 1) as usize,
        "the burst budget must cap delivery at max_attempts + 1 connections"
    );
    assert_eq!(
        state.connections.load(Ordering::SeqCst),
        MAX_ATTEMPTS + 1,
        "the client must stop reopening, not loop against the endless flapper"
    );
}

/// #4740 backstop: even a connection that games the uptime threshold — stays up
/// just past `min_healthy_uptime`, delivers an item, and drops on a loop, so it
/// resets the burst budget every cycle — cannot reopen forever. The absolute
/// `max_total_reopens` cap survives the resets and stops the stream after a
/// bounded number of reopens.
#[tokio::test]
async fn reconnect_absolute_cap_bounds_a_healthy_looking_flapping_peer() {
    const BLIPS: u32 = 100;
    const MAX_TOTAL_REOPENS: u32 = 3;

    // Hold (15ms) exceeds the healthy threshold (5ms), so every connection
    // resets the burst budget — only the absolute cap can end the loop.
    let (base_url, state) = start_flapping_server(BLIPS, Duration::from_millis(15)).await;
    let cfg = ClientConfig::new(base_url).with_stream_reconnect(
        toolkit_contract::runtime::config::ReconnectConfig::enabled(2, Duration::from_millis(1))
            .with_min_healthy_uptime(Duration::from_millis(5))
            .with_max_total_reopens(MAX_TOTAL_REOPENS),
    );
    let client = DemoApiRestClient::new(cfg).unwrap();

    let results: Vec<_> = DemoApi::ticks(&client, anonymous_ctx(), 0)
        .collect::<Vec<_>>()
        .await;

    assert!(
        results.last().is_some_and(std::result::Result::is_err),
        "the absolute reopen cap must surface an error, got {results:?}"
    );
    assert_eq!(
        state.connections.load(Ordering::SeqCst),
        MAX_TOTAL_REOPENS + 1,
        "connections stop at the initial open plus max_total_reopens reopens"
    );
}

/// The reset must not defeat the budget's actual purpose. When a connection
/// fails *without* delivering anything, the attempts keep accumulating and the
/// stream gives up as configured.
#[tokio::test]
async fn reconnect_budget_still_caps_a_burst_with_no_progress() {
    // No route is registered, so every attempt fails at connect time and no
    // connection ever delivers an event.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    drop(listener); // Nothing is listening: connection refused on every attempt.

    let cfg = ClientConfig::new(format!("http://{addr}")).with_stream_reconnect(
        toolkit_contract::runtime::config::ReconnectConfig::enabled(2, Duration::from_millis(1)),
    );
    let client = DemoApiRestClient::new(cfg).unwrap();

    let results: Vec<_> = DemoApi::ticks(&client, anonymous_ctx(), 0)
        .collect::<Vec<_>>()
        .await;

    assert!(
        results.iter().any(std::result::Result::is_err),
        "a burst with no progress must still exhaust the budget and surface an error"
    );
}

// --- Fallible open (#4734 Phase B) ----------------------------------------

/// The point of the fallible-open shape: an open-time `409` carrying a domain
/// `Problem` is an `Err` from the *awaited call*, and **no stream is
/// produced**.
///
/// Before this shape existed the only place to put such a failure was the
/// stream's first item, which put it past the caller's open-time handling —
/// the recovery decision for `PositionsNotSet` (re-seek and retry) differs
/// from every other failure (re-JOIN), so it has to be observable at the open.
#[tokio::test]
async fn awaited_open_that_fails_yields_err_and_no_stream() {
    let (base_url, _) = start_server().await;
    let client = DemoApiRestClient::new(ClientConfig::new(base_url)).unwrap();

    // `409` selects the PositionsNotSet-shaped Problem response.
    let opened = DemoApi::frames(&client, anonymous_ctx(), 409).await;

    // The `Err` variant *is* the assertion that no stream was produced: the
    // method's return type gives back either a stream or an error, never both.
    let Err(DemoError::Transport(err)) = opened else {
        panic!("a 409 at open must fail the awaited call, not produce a stream");
    };
    match err {
        TransportError::Problem { problem, .. } => {
            assert_eq!(problem.title, "PositionsNotSet");
            assert_eq!(problem.status, Some(409));
        }
        other => panic!("expected the domain Problem to survive the open, got {other:?}"),
    }
}

/// The complement: once the open has succeeded the caller holds a stream, and
/// a later failure is an *item* of it. Both halves use the same declared `E`.
#[tokio::test]
async fn awaited_open_that_succeeds_reports_a_failing_item_on_the_stream() {
    let (base_url, _) = start_server().await;
    let client = DemoApiRestClient::new(ClientConfig::new(base_url)).unwrap();

    // `since == 1` sends one good frame, then a frame whose `data` is not a
    // valid `Tick`.
    let stream = DemoApi::frames(&client, anonymous_ctx(), 1)
        .await
        .expect("the open itself must succeed");

    let items: Vec<Result<Tick, DemoError>> = stream.collect::<Vec<_>>().await;
    assert_eq!(items.len(), 2, "one good item, then the failing one");
    assert_eq!(
        items[0].as_ref().expect("first item decodes").seq,
        0,
        "the item before the failure is still delivered"
    );
    assert!(
        items[1].is_err(),
        "a post-open failure must arrive as a stream item"
    );
}

/// A plain `#[streaming] async fn` open that succeeds delivers its items
/// normally — the fallible shape is not a special-cased error path.
#[tokio::test]
async fn awaited_open_yields_typed_items() {
    let (base_url, _) = start_server().await;
    let client = DemoApiRestClient::new(ClientConfig::new(base_url)).unwrap();

    let items: Vec<Tick> = DemoApi::frames(&client, anonymous_ctx(), 3)
        .await
        .expect("open must succeed")
        .collect::<Vec<_>>()
        .await
        .into_iter()
        .collect::<Result<_, _>>()
        .unwrap();

    assert_eq!(items.len(), 3);
    assert_eq!(items[0].seq, 0);
    assert_eq!(items[2].seq, 2);
}

/// D6, and the test that makes D6 mean anything: the reconnect policy is
/// **derived from the declared open shape**, not from client config.
///
/// One client, one `stream_reconnect` setting, one server behaving identically on
/// both routes — and the two methods diverge. The `Immediate` method re-opens
/// until its budget is spent; the `Awaited` one opens exactly once, because a
/// fallible open carries domain semantics the client must not blindly repeat
/// (a held exclusion lease, a cursor-based resume, and open-time errors the
/// caller must see at the open rather than as stream items).
#[tokio::test]
async fn reconnect_is_derived_from_the_open_shape_not_from_client_config() {
    const MAX_ATTEMPTS: u32 = 2;

    let (base_url, ticks_connections, frames_connections) = start_doneless_server().await;
    let cfg = ClientConfig::new(base_url).with_stream_reconnect(
        toolkit_contract::runtime::config::ReconnectConfig::enabled(
            MAX_ATTEMPTS,
            Duration::from_millis(1),
        ),
    );
    let client = DemoApiRestClient::new(cfg).unwrap();

    // `Immediate`: reconnect enabled, so the doneless close is retried until
    // the budget is exhausted — the initial connection plus MAX_ATTEMPTS.
    let ticks: Vec<_> = DemoApi::ticks(&client, anonymous_ctx(), 0)
        .collect::<Vec<_>>()
        .await;
    assert!(
        ticks.iter().any(std::result::Result::is_err),
        "the budget must eventually be exhausted and surface an error"
    );
    assert_eq!(
        ticks_connections.load(Ordering::SeqCst),
        MAX_ATTEMPTS + 1,
        "an immediate open must honour the client's stream_reconnect policy"
    );

    // `Awaited`: same client, same config, same server behaviour — but the
    // derived policy is `ReconnectConfig::disabled()`, so exactly one open.
    let frames: Vec<_> = DemoApi::frames(&client, anonymous_ctx(), 0)
        .await
        .expect("the open itself succeeds; the failure is mid-stream")
        .collect::<Vec<_>>()
        .await;
    assert!(
        frames.iter().any(std::result::Result::is_err),
        "the doneless close must still surface as a stream error"
    );
    assert_eq!(
        frames_connections.load(Ordering::SeqCst),
        1,
        "a fallible open must NOT reconnect, even with stream_reconnect enabled"
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
            assert_eq!(p.status, Some(503));
            assert!(p.problem_type.contains("service_unavailable"));
        }
        other @ DemoError::Transport(_) => panic!("unexpected: {other:?}"),
    }
}
