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
use toolkit_canonical_errors::{CanonicalError, Problem};
use toolkit_contract::runtime::config::{ClientConfig, RetryConfig};
use toolkit_contract::runtime::transport_error::TransportError;
use toolkit_contract::{contract, rest_contract};
use toolkit_security::SecurityContext;
use serde::{Deserialize, Serialize};
use std::pin::Pin;
use std::time::Duration;

use futures_core::Stream;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EchoRequest {
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
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

    #[get("/ticks")]
    #[streaming]
    fn ticks(&self, ctx: SecurityContext, count: u64) -> Result<Tick, DemoError>;
}

// --- Server ---------------------------------------------------------------

#[derive(Clone, Default)]
struct ServerState {
    flaky_attempts: Arc<AtomicU32>,
}

async fn echo_get_handler(Path(id): Path<String>) -> Json<EchoResponse> {
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
    axum::extract::Query(params): axum::extract::Query<TicksParams>,
) -> Sse<impl Stream<Item = Result<Event, std::convert::Infallible>>> {
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

async fn start_server() -> (String, ServerState) {
    let state = ServerState::default();
    let app = Router::new()
        .route("/api/demo/v1/echo/{id}", get(echo_get_handler))
        .route("/api/demo/v1/echo", post(echo_post_handler))
        .route(
            "/api/demo/v1/flaky/{id}",
            get(flaky_handler).with_state(state.clone()),
        )
        .route("/api/demo/v1/ticks", get(ticks_handler));

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
/// Mid-flight TCP-disconnect scenarios are intentionally covered by the
/// `runtime::sse::tests::captures_id_field_for_reconnect` unit test; an
/// HTTP-level integration test for them would require server-side
/// connection-drop machinery beyond what `axum::serve` provides natively.
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

#[tokio::test]
async fn server_problem_envelope_round_trips() {
    let (base_url, _) = start_server().await;
    let cfg = ClientConfig::new(base_url).with_retry(RetryConfig::off());
    let client = DemoApiRestClient::new(cfg).unwrap();
    let err = DemoApi::flaky_get(&client, anonymous_ctx(), "xyz".to_owned())
        .await
        .unwrap_err();
    match err {
        DemoError::Transport(TransportError::Problem(p)) => {
            assert_eq!(p.status, 503);
            assert!(p.problem_type.contains("service_unavailable"));
        }
        other @ DemoError::Transport(_) => panic!("unexpected: {other:?}"),
    }
}
