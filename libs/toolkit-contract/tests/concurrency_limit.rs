//! Behavioural test that the client-side concurrency limiter is actually wired
//! onto the transport by
//! [`build_default_http_client`](toolkit_contract::runtime::client::build_default_http_client),
//! and that a shed request surfaces as the dedicated, non-transient
//! [`TransportError::Overloaded`].
//!
//! `HttpClient` exposes no accessor for its limiter, so the only way to pin that
//! `max_concurrent_requests` reaches the transport (and was not dropped, or
//! wired to the wrong builder method) is to observe the behaviour: with a cap of
//! 1, a second request made while the first is still in flight must be shed.

#![cfg(feature = "rest-client")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::extract::State;
use axum::response::IntoResponse;
use axum::routing::get;
use tokio::sync::Notify;

use std::sync::atomic::{AtomicUsize, Ordering};

use toolkit_contract::runtime::client::{
    StreamRequest, build_default_http_client, open_streaming, send_unary,
};
use toolkit_contract::runtime::config::{ClientConfig, ReconnectConfig};
use toolkit_contract::runtime::transport_error::TransportError;

#[derive(Clone)]
struct HoldState {
    /// Fired by the handler when a request reaches it (so the test knows the
    /// single concurrency permit is now held).
    arrived: Arc<Notify>,
    /// Awaited by the handler; the test releases it once the second request has
    /// been shed, letting the first request complete.
    release: Arc<Notify>,
}

/// Holds the connection open (occupying the single permit) until the test
/// releases it, then returns an empty JSON object.
async fn hold_handler(State(state): State<HoldState>) -> impl IntoResponse {
    state.arrived.notify_one();
    state.release.notified().await;
    axum::Json(serde_json::json!({}))
}

/// Responds immediately — used to prove a request is served (not shed).
async fn ok_handler() -> impl IntoResponse {
    axum::Json(serde_json::json!({}))
}

async fn start_hold_server() -> (String, HoldState) {
    let state = HoldState {
        arrived: Arc::new(Notify::new()),
        release: Arc::new(Notify::new()),
    };
    let app = Router::new()
        .route("/hold", get(hold_handler))
        .route("/ok", get(ok_handler))
        .with_state(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (format!("http://{addr}"), state)
}

/// With `max_concurrent_requests: Some(1)`, a second in-flight request is shed
/// with `TransportError::Overloaded` (non-transient), while the first still
/// completes normally once released.
#[tokio::test]
async fn second_request_is_shed_while_first_is_in_flight() {
    let (base_url, state) = start_hold_server().await;
    let url = format!("{base_url}/hold");

    let config = ClientConfig::new(&base_url).with_max_concurrent_requests(Some(1));
    let client = build_default_http_client("concurrency-limit-test", &config).unwrap();

    // Request A: occupies the single permit and blocks in the handler.
    let client_a = client.clone();
    let url_a = url.clone();
    let a = tokio::spawn(async move {
        send_unary::<_, serde_json::Value>(
            || Ok(client_a.get(&url_a)),
            Some(Duration::from_secs(5)),
        )
        .await
    });

    // Wait until A is actually at the server, so the permit is held.
    state.arrived.notified().await;

    // Request B: no permit available -> shed immediately.
    let b =
        send_unary::<_, serde_json::Value>(|| Ok(client.get(&url)), Some(Duration::from_secs(5)))
            .await;
    assert!(
        matches!(b, Err(TransportError::Overloaded)),
        "second request should be shed with Overloaded, got: {b:?}"
    );
    assert!(
        !TransportError::Overloaded.is_transient(),
        "Overloaded must be non-transient so the SDK retry loop does not re-issue it"
    );

    // Release A and confirm it completed normally (the permit was really held).
    state.release.notify_one();
    let a_result = a.await.unwrap();
    assert!(
        a_result.is_ok(),
        "first request should succeed, got: {a_result:?}"
    );
}

/// `max_concurrent_requests: Some(0)` must be clamped to a working limiter
/// (>= 1) by the transport — not left as a dead 0-permit semaphore that sheds
/// every request. A single request through a cap-0 client must therefore be
/// served, not shed with `Overloaded`. (Without the clamp this request fails.)
#[tokio::test]
async fn cap_zero_is_clamped_and_still_serves() {
    let (base_url, _state) = start_hold_server().await;
    let url = format!("{base_url}/ok");

    let config = ClientConfig::new(&base_url).with_max_concurrent_requests(Some(0));
    let client = build_default_http_client("concurrency-limit-zero-test", &config).unwrap();

    let result =
        send_unary::<_, serde_json::Value>(|| Ok(client.get(&url)), Some(Duration::from_secs(5)))
            .await;
    assert!(
        result.is_ok(),
        "a cap of 0 must be clamped to 1 so the request is served, got: {result:?}"
    );
}

/// The transport's own `TimeoutLayer` — wired to `config.timeout` by
/// `build_default_http_client` — fires when the SDK passes no per-call
/// deadline. Its `HttpError::Timeout` must be classified as
/// `TransportError::Timeout` (transient), not collapsed into the generic
/// `Network` variant, so a `#[retryable]` method still retries it.
#[tokio::test]
async fn transport_timeout_maps_to_timeout_variant() {
    let (base_url, state) = start_hold_server().await;
    let url = format!("{base_url}/hold");

    // Short transport timeout baked into the client; no per-call deadline, so
    // the transport's TimeoutLayer is the only timer that can fire.
    let config = ClientConfig::new(&base_url).with_timeout(Duration::from_millis(50));
    let client = build_default_http_client("transport-timeout-test", &config).unwrap();

    let result = send_unary::<_, serde_json::Value>(|| Ok(client.get(&url)), None).await;
    assert!(
        matches!(&result, Err(TransportError::Timeout(_))),
        "transport timeout should surface as TransportError::Timeout, got: {result:?}"
    );
    assert!(
        result.is_err_and(|e| e.is_transient()),
        "a transport timeout must remain transient so #[retryable] retries it"
    );

    // Let the blocked handler unwind cleanly.
    state.release.notify_one();
}

/// A stream **open** shed by the concurrency limiter must fail fast, not be
/// treated as a reconnect-eligible failure. This is verified against a
/// *non-zero* reconnect budget: the open must still surface `Overloaded`
/// immediately AND the request factory must be invoked exactly once — proving
/// no reopen was attempted despite the budget, rather than merely that a
/// no-budget stream ends (which any error would).
#[tokio::test]
async fn stream_open_shed_is_not_retried_despite_reconnect_budget() {
    let (base_url, state) = start_hold_server().await;
    let url = format!("{base_url}/hold");

    let config = ClientConfig::new(&base_url).with_max_concurrent_requests(Some(1));
    let client = build_default_http_client("stream-open-shed-test", &config).unwrap();

    // Request A: a unary call that occupies the single permit and blocks.
    let client_a = client.clone();
    let url_a = url.clone();
    let a = tokio::spawn(async move {
        send_unary::<_, serde_json::Value>(
            || Ok(client_a.get(&url_a)),
            Some(Duration::from_secs(5)),
        )
        .await
    });
    state.arrived.notified().await;

    // Count factory calls: a reopen would call it again. A budget of 3 means a
    // retryable classification WOULD reopen; a fail-fast one must not.
    let calls = Arc::new(AtomicUsize::new(0));
    let calls_factory = Arc::clone(&calls);
    let client_b = client.clone();
    let url_b = url.clone();
    let request = StreamRequest::new(move |_last: Option<&str>| {
        calls_factory.fetch_add(1, Ordering::SeqCst);
        Ok(client_b.get(&url_b))
    })
    .reconnect(ReconnectConfig::enabled(3, Duration::from_millis(10)));

    let opened = open_streaming::<_, serde_json::Value>(request).await;
    match opened {
        Err(TransportError::Overloaded) => {}
        Err(other) => panic!("expected a fail-fast Overloaded shed, got: {other:?}"),
        Ok(_) => panic!("expected the shed stream open to fail, but it succeeded"),
    }
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "an overloaded open must not be retried even with reconnect budget available"
    );

    // Release A so the held permit is proven real and the server unwinds.
    state.release.notify_one();
    assert!(a.await.unwrap().is_ok());
}
