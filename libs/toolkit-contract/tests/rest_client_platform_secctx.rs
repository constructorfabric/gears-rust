//! End-to-end test for platform-plane (`PlatformSecurityContext`) REST client
//! codegen (issue #4577).
//!
//! A method whose first argument is `&PlatformSecurityContext` is a
//! platform-plane call: the generated client attaches the process's internal
//! credential as the `X-ToolKit-Internal-Token` header — sourced from the
//! runtime [`InternalTokenProvider`] on `ClientConfig`, never from the argument
//! — and **never** `Authorization`. With no provider configured it attaches
//! nothing (permissive; enforcement is server-side). See
//! `cpt-cf-adr-two-plane-auth` / `cpt-cf-adr-platform-plane-auth`.

#![cfg(feature = "rest-client")]
#![allow(clippy::unwrap_used)]

use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use axum::extract::{Path, State};
use axum::response::IntoResponse;
use axum::response::sse::{Event, Sse};
use axum::routing::get;
use axum::{Json, Router};
use futures_util::{Stream, StreamExt as _};
use secrecy::SecretString;
use serde::{Deserialize, Serialize};
use toolkit_contract::runtime::config::{
    ClientConfig, CredentialState, InternalTokenProvider, ReconnectConfig,
};
use toolkit_contract::runtime::transport_error::TransportError;
use toolkit_contract::{contract, rest_contract};
use toolkit_security::constants::INTERNAL_TOKEN_HEADER;
use toolkit_security::{PlatformIdentity, PlatformSecurityContext, SecurityContext};

/// Stream item for the platform-plane streaming method.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Tick {
    pub seq: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HeaderEcho {
    /// The `X-ToolKit-Internal-Token` header value the server observed, if any.
    pub internal_token: Option<String>,
    /// The `Authorization` header value the server observed, if any. Must stay
    /// `None` for platform-plane calls — the credential must never ride
    /// `Authorization`.
    pub authorization: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum DemoError {
    #[error("transport error: {0}")]
    Transport(#[from] TransportError),
}

#[contract(gear = "sys", version = "v1")]
pub trait SysApi: Send + Sync {
    #[idempotency(SafeRead)]
    async fn whoami(
        &self,
        ctx: &PlatformSecurityContext,
        id: String,
    ) -> Result<HeaderEcho, DemoError>;

    /// Tenant-plane sibling: must forward the caller's bearer token on
    /// `Authorization` and must NOT attach the platform internal token, even
    /// when a provider is configured on the client.
    #[idempotency(SafeRead)]
    async fn whoami_tenant(
        &self,
        ctx: &SecurityContext,
        id: String,
    ) -> Result<HeaderEcho, DemoError>;

    /// Retryable platform-plane method: exercises per-attempt credential
    /// freshness across the client's retry loop.
    #[idempotency(IdempotentWrite)]
    async fn whoami_retry(
        &self,
        ctx: &PlatformSecurityContext,
        id: String,
    ) -> Result<HeaderEcho, DemoError>;

    /// Streaming platform-plane method: exercises the SSE reconnect factory's
    /// credential attachment (a separate code path from the unary attempt).
    #[idempotency(SafeRead)]
    #[streaming]
    fn watch(&self, ctx: PlatformSecurityContext, id: String) -> Result<Tick, DemoError>;
}

#[rest_contract(base_path = "/api/sys/v1")]
pub trait SysApiRest: SysApi {
    // `#[server_manual]`: this test drives a hand-written Axum server and only
    // exercises the CLIENT (outbound) side, so no server route is generated.
    #[get("/whoami/{id}")]
    #[server_manual]
    async fn whoami(
        &self,
        ctx: &PlatformSecurityContext,
        id: String,
    ) -> Result<HeaderEcho, DemoError>;

    #[get("/whoami-tenant/{id}")]
    #[server_manual]
    async fn whoami_tenant(
        &self,
        ctx: &SecurityContext,
        id: String,
    ) -> Result<HeaderEcho, DemoError>;

    #[get("/whoami-retry/{id}")]
    #[server_manual]
    #[retryable]
    async fn whoami_retry(
        &self,
        ctx: &PlatformSecurityContext,
        id: String,
    ) -> Result<HeaderEcho, DemoError>;

    #[get("/watch/{id}")]
    #[server_manual]
    #[streaming]
    fn watch(&self, ctx: PlatformSecurityContext, id: String) -> Result<Tick, DemoError>;
}

fn echo_headers(headers: &axum::http::HeaderMap) -> HeaderEcho {
    let internal_token = headers
        .get(INTERNAL_TOKEN_HEADER)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    let authorization = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    HeaderEcho {
        internal_token,
        authorization,
    }
}

async fn whoami_handler(
    Path(_id): Path<String>,
    headers: axum::http::HeaderMap,
) -> Json<HeaderEcho> {
    Json(echo_headers(&headers))
}

/// Retry handler: returns `503` on the first request and `200` afterward, so a
/// `#[retryable]` client makes two attempts. Records the observed
/// `X-ToolKit-Internal-Token` of every attempt into shared state so the test can
/// assert each attempt re-resolved the (rotating) credential.
async fn whoami_retry_handler(
    State(state): State<RetryState>,
    Path(_id): Path<String>,
    headers: axum::http::HeaderMap,
) -> axum::response::Response {
    let observed = headers
        .get(INTERNAL_TOKEN_HEADER)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    state.seen_tokens.lock().unwrap().push(observed);

    let attempt = state.attempts.fetch_add(1, Ordering::SeqCst);
    if attempt == 0 {
        // Transient failure on the first attempt → client retries.
        axum::http::StatusCode::SERVICE_UNAVAILABLE.into_response()
    } else {
        Json(echo_headers(&headers)).into_response()
    }
}

#[derive(Clone, Default)]
struct RetryState {
    attempts: Arc<AtomicU32>,
    seen_tokens: Arc<std::sync::Mutex<Vec<Option<String>>>>,
}

#[derive(Clone, Default)]
struct WatchState {
    connections: Arc<AtomicU32>,
    seen_tokens: Arc<std::sync::Mutex<Vec<Option<String>>>>,
}

/// SSE handler that records the `X-ToolKit-Internal-Token` of every connection.
/// The FIRST connection emits one event then ends WITHOUT `event: done` — a
/// reconnect-eligible anomaly — forcing the client's reconnect factory to run;
/// the SECOND connection completes cleanly. This exercises the platform-plane
/// credential attach on the reconnect path (distinct from the unary attempt).
async fn watch_handler(
    State(state): State<WatchState>,
    Path(_id): Path<String>,
    headers: axum::http::HeaderMap,
) -> Sse<Pin<Box<dyn Stream<Item = Result<Event, std::convert::Infallible>> + Send>>> {
    let observed = headers
        .get(INTERNAL_TOKEN_HEADER)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    state.seen_tokens.lock().unwrap().push(observed);

    let n = state.connections.fetch_add(1, Ordering::SeqCst);
    if n == 0 {
        // Deliver one event, then vanish without `done` → reconnect.
        let stream = futures_util::stream::once(async {
            Ok(Event::default()
                .id("1")
                .data(serde_json::to_string(&Tick { seq: 0 }).unwrap()))
        });
        Sse::new(Box::pin(stream))
    } else {
        // Reconnect: deliver + terminate cleanly.
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

async fn start_server() -> String {
    let app = Router::new()
        .route("/api/sys/v1/whoami/{id}", get(whoami_handler))
        .route("/api/sys/v1/whoami-tenant/{id}", get(whoami_handler));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    format!("http://{addr}")
}

async fn start_retry_server(state: RetryState) -> String {
    let app = Router::new()
        .route("/api/sys/v1/whoami-retry/{id}", get(whoami_retry_handler))
        .with_state(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    format!("http://{addr}")
}

async fn start_watch_server(state: WatchState) -> String {
    let app = Router::new()
        .route("/api/sys/v1/watch/{id}", get(watch_handler))
        .with_state(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    format!("http://{addr}")
}

fn tenant_ctx() -> SecurityContext {
    SecurityContext::builder()
        .subject_id(uuid::Uuid::nil())
        .subject_tenant_id(uuid::Uuid::nil())
        .bearer_token("tenant.jwt.token".to_owned())
        .build()
        .unwrap()
}

fn platform_ctx() -> PlatformSecurityContext {
    PlatformSecurityContext::new(PlatformIdentity::Shared {
        name: "test-caller".to_owned(),
    })
}

#[tokio::test]
async fn platform_call_attaches_internal_token_from_provider() {
    let base_url = start_server().await;
    let cfg = ClientConfig::new(base_url).with_internal_token_provider(
        InternalTokenProvider::from_token(SecretString::from("sa.jwt.token")),
    );
    let client = SysApiRestClient::new(cfg).unwrap();

    let echo = SysApi::whoami(&client, &platform_ctx(), "abc".to_owned())
        .await
        .unwrap();

    assert_eq!(
        echo.internal_token.as_deref(),
        Some("sa.jwt.token"),
        "platform-plane call must carry the runtime credential as X-ToolKit-Internal-Token"
    );
    assert_eq!(
        echo.authorization, None,
        "platform-plane credential must never ride Authorization"
    );
}

#[tokio::test]
async fn platform_call_without_provider_attaches_nothing() {
    // No provider configured (Profile 1 / `InternalCredential::None`): the
    // client is permissive and attaches no credential. The requirement is
    // enforced server-side, not by the client.
    let base_url = start_server().await;
    let client = SysApiRestClient::new(ClientConfig::new(base_url)).unwrap();

    let echo = SysApi::whoami(&client, &platform_ctx(), "abc".to_owned())
        .await
        .unwrap();

    assert_eq!(echo.internal_token, None);
    assert_eq!(echo.authorization, None);
}

/// The rotating-provider form ([`InternalTokenProvider::new`]) is consulted on
/// each call, so a credential rotated after client construction is observed.
///
/// Rotation is made **explicit** — the backing value is mutated once, between
/// the two calls — rather than counting closure invocations. This asserts the
/// actual behaviour (pre-rotation value on the first request, post-rotation on
/// the second) and stays correct even if the request builder resolves the
/// provider more than once per request (retry, redirect, reconnect).
#[tokio::test]
async fn platform_call_reads_current_token_from_rotating_provider() {
    use std::sync::{Arc, Mutex};

    let base_url = start_server().await;
    let token = Arc::new(Mutex::new(String::from("token-before")));
    let provider_token = Arc::clone(&token);
    let cfg = ClientConfig::new(base_url).with_internal_token_provider(InternalTokenProvider::new(
        move || {
            let current = provider_token.lock().unwrap().clone();
            CredentialState::Available(SecretString::from(current))
        },
    ));
    let client = SysApiRestClient::new(cfg).unwrap();

    let first = SysApi::whoami(&client, &platform_ctx(), "a".to_owned())
        .await
        .unwrap();

    // Rotate the credential between calls.
    *token.lock().unwrap() = String::from("token-after");

    let second = SysApi::whoami(&client, &platform_ctx(), "b".to_owned())
        .await
        .unwrap();

    assert_eq!(
        first.internal_token.as_deref(),
        Some("token-before"),
        "first request must carry the pre-rotation credential"
    );
    assert_eq!(
        second.internal_token.as_deref(),
        Some("token-after"),
        "second request must carry the post-rotation credential"
    );
}

/// Tenant-plane direction: a `SecurityContext` method forwards the caller's
/// bearer on `Authorization` and does NOT attach `X-ToolKit-Internal-Token`,
/// even though the client has an internal-token provider configured. This
/// guards against the rewritten `capture_auth_credentials` silently dropping
/// tenant auth or leaking the platform credential onto a tenant call.
#[tokio::test]
async fn tenant_call_sends_bearer_and_not_internal_token() {
    let base_url = start_server().await;
    let cfg = ClientConfig::new(base_url).with_internal_token_provider(
        InternalTokenProvider::from_token(SecretString::from("sa.jwt.token")),
    );
    let client = SysApiRestClient::new(cfg).unwrap();

    let echo = SysApi::whoami_tenant(&client, &tenant_ctx(), "abc".to_owned())
        .await
        .unwrap();

    assert_eq!(
        echo.authorization.as_deref(),
        Some("Bearer tenant.jwt.token"),
        "tenant-plane call must forward the caller's bearer on Authorization"
    );
    assert_eq!(
        echo.internal_token, None,
        "tenant-plane call must NOT attach the platform internal token"
    );
}

/// Per-attempt credential freshness: a `#[retryable]` platform method whose
/// first attempt gets `503` retries; each attempt re-resolves the rotating
/// provider, so the two requests carry different tokens. Hoisting the
/// resolution above the retry loop would make both attempts identical and fail
/// this test.
#[tokio::test]
async fn retryable_platform_call_reresolves_token_each_attempt() {
    let state = RetryState::default();
    let base_url = start_retry_server(state.clone()).await;

    // A provider that yields a fresh token on every resolution.
    let counter = Arc::new(AtomicU32::new(0));
    let provider_counter = Arc::clone(&counter);
    let cfg = ClientConfig::new(base_url).with_internal_token_provider(InternalTokenProvider::new(
        move || {
            let n = provider_counter.fetch_add(1, Ordering::SeqCst);
            CredentialState::Available(SecretString::from(format!("token-{n}")))
        },
    ));
    let client = SysApiRestClient::new(cfg).unwrap();

    let echo = SysApi::whoami_retry(&client, &platform_ctx(), "abc".to_owned())
        .await
        .unwrap();

    let seen = state.seen_tokens.lock().unwrap().clone();
    assert_eq!(
        seen.len(),
        2,
        "expected exactly two attempts (503 then 200), saw {seen:?}"
    );
    assert_eq!(seen[0].as_deref(), Some("token-0"));
    assert_eq!(seen[1].as_deref(), Some("token-1"));
    assert_ne!(
        seen[0], seen[1],
        "each retry attempt must re-resolve the credential"
    );
    // The successful (second) attempt is the one whose headers are echoed back.
    assert_eq!(echo.internal_token.as_deref(), Some("token-1"));
}

/// SSE reconnect path: a `#[streaming]` platform-plane method whose first
/// connection ends without `done` triggers a reconnect. The reconnect factory
/// re-resolves the rotating provider, so the initial request and the reconnect
/// each carry an internal token — proving the reconnect branch attaches the
/// credential (it is a separate code path from the unary attempt).
#[tokio::test]
async fn streaming_platform_reconnect_attaches_internal_token_each_connection() {
    let state = WatchState::default();
    let base_url = start_watch_server(state.clone()).await;

    let counter = Arc::new(AtomicU32::new(0));
    let provider_counter = Arc::clone(&counter);
    let cfg = ClientConfig::new(base_url)
        .with_sse_reconnect(ReconnectConfig::enabled(
            3,
            std::time::Duration::from_millis(1),
        ))
        .with_internal_token_provider(InternalTokenProvider::new(move || {
            let n = provider_counter.fetch_add(1, Ordering::SeqCst);
            CredentialState::Available(SecretString::from(format!("token-{n}")))
        }));
    let client = SysApiRestClient::new(cfg).unwrap();

    let mut stream = SysApi::watch(&client, platform_ctx(), "abc".to_owned());
    let mut items = Vec::new();
    while let Some(item) = stream.next().await {
        items.push(item.unwrap());
    }

    // Two events delivered across the initial connection + the reconnect.
    assert_eq!(items, vec![Tick { seq: 0 }, Tick { seq: 1 }]);

    let seen = state.seen_tokens.lock().unwrap().clone();
    assert_eq!(
        seen.len(),
        2,
        "expected an initial connection and one reconnect, saw {seen:?}"
    );
    assert!(
        seen.iter().all(Option::is_some),
        "both the initial and reconnect requests must carry an internal token, saw {seen:?}"
    );
    assert_ne!(
        seen[0], seen[1],
        "the reconnect must re-resolve the credential (rotating provider)"
    );
}
