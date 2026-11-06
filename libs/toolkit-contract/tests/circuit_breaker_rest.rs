//! The circuit breaker in the REST transport built by
//! [`build_default_http_client`]: which attempts it counts, that an open
//! breaker refuses without sending, and that a timed-out attempt reaches it,
//! also when a slow auth layer started the transport timer late.

#![cfg(feature = "rest-client")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use axum::Router;
use axum::extract::State;
use axum::http::{StatusCode, header};
use axum::response::IntoResponse;
use axum::routing::get;
use toolkit_contract::runtime::breaker::{EndpointFailureHook, SkipCircuitBreaker};
use toolkit_contract::runtime::client::{
    StreamRequest, build_default_http_client, open_streaming, send_unary,
};
use toolkit_contract::runtime::config::ClientConfig;
use toolkit_contract::runtime::transport_error::TransportError;
use toolkit_resilience::circuit_breaker::{CircuitBreaker, CircuitBreakerConfig, State as Breaker};

#[derive(Clone, Default)]
struct Hits(Arc<AtomicUsize>);

impl Hits {
    fn get(&self) -> usize {
        self.0.load(Ordering::SeqCst)
    }
}

async fn bad_gateway(State(hits): State<Hits>) -> impl IntoResponse {
    hits.0.fetch_add(1, Ordering::SeqCst);
    (StatusCode::BAD_GATEWAY, "upstream gone")
}

async fn problem_503(State(hits): State<Hits>) -> impl IntoResponse {
    hits.0.fetch_add(1, Ordering::SeqCst);
    (
        StatusCode::SERVICE_UNAVAILABLE,
        [(header::CONTENT_TYPE, "application/problem+json")],
        r#"{"type":"gts://x","title":"Unavailable","status":503,"detail":"db down"}"#,
    )
}

async fn slow(State(hits): State<Hits>) -> impl IntoResponse {
    hits.0.fetch_add(1, Ordering::SeqCst);
    tokio::time::sleep(Duration::from_secs(5)).await;
    axum::Json(serde_json::json!({}))
}

async fn server() -> (String, Hits) {
    let hits = Hits::default();
    let app = Router::new()
        .route("/bad-gateway", get(bad_gateway))
        .route("/problem", get(problem_503))
        .route("/slow", get(slow))
        .with_state(hits.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (format!("http://{addr}"), hits)
}

fn breaker() -> Arc<CircuitBreaker> {
    Arc::new(
        CircuitBreaker::new(
            CircuitBreakerConfig::default()
                .with_minimum_calls(2)
                .with_open_for(Duration::from_secs(30)),
        )
        .unwrap(),
    )
}

async fn call(config: &ClientConfig, url: &str) -> Result<serde_json::Value, TransportError> {
    let client = build_default_http_client("breaker-test", config).unwrap();
    send_unary(|| Ok(client.get(url)), Some(config.timeout)).await
}

#[tokio::test]
async fn gateway_errors_open_the_breaker_and_it_refuses_without_sending() {
    let (base, hits) = server().await;
    let b = breaker();
    let config = ClientConfig::new(&base).with_circuit_breaker(Arc::clone(&b));
    let url = format!("{base}/bad-gateway");

    for _ in 0..2 {
        assert!(matches!(
            call(&config, &url).await,
            Err(TransportError::HttpStatus { status: 502, .. })
        ));
    }
    assert_eq!(b.state(), Breaker::Open);

    let refused = call(&config, &url).await.unwrap_err();
    let TransportError::CircuitOpen { retry_after } = refused else {
        panic!("expected CircuitOpen, got {refused:?}");
    };
    assert!(retry_after > Duration::from_secs(25));
    assert!(!refused.is_transient());
    assert_eq!(hits.get(), 2, "the refused call never reached the server");
}

#[tokio::test]
async fn a_problem_503_from_the_backend_is_not_counted() {
    let (base, hits) = server().await;
    let b = breaker();
    let config = ClientConfig::new(&base).with_circuit_breaker(Arc::clone(&b));
    let url = format!("{base}/problem");

    for _ in 0..5 {
        assert!(call(&config, &url).await.is_err());
    }
    assert_eq!(b.state(), Breaker::Closed);
    assert_eq!(hits.get(), 5);
}

#[tokio::test]
async fn a_refused_connection_counts_and_runs_the_failure_hook() {
    let addr = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap()
    };
    let hooks = Arc::new(AtomicUsize::new(0));
    let hook = {
        let hooks = Arc::clone(&hooks);
        EndpointFailureHook::new(move || {
            hooks.fetch_add(1, Ordering::SeqCst);
        })
    };
    let b = breaker();
    let config = ClientConfig::new(format!("http://{addr}"))
        .with_circuit_breaker(Arc::clone(&b))
        .with_on_endpoint_failure(hook);
    let url = format!("http://{addr}/x");

    for _ in 0..2 {
        assert!(matches!(
            call(&config, &url).await,
            Err(TransportError::Network(_))
        ));
    }
    assert_eq!(hooks.load(Ordering::SeqCst), 2);
    assert!(matches!(
        call(&config, &url).await,
        Err(TransportError::CircuitOpen { .. })
    ));
}

#[tokio::test]
async fn a_timed_out_attempt_reaches_the_breaker() {
    let (base, _hits) = server().await;
    let b = breaker();
    let config = ClientConfig::new(&base)
        .with_timeout(Duration::from_millis(200))
        .with_circuit_breaker(Arc::clone(&b));
    let url = format!("{base}/slow");

    for _ in 0..2 {
        assert!(matches!(
            call(&config, &url).await,
            Err(TransportError::Timeout(_))
        ));
    }
    assert_eq!(
        b.state(),
        Breaker::Open,
        "the transport timeout, not the outer one, must end the attempt"
    );
}

/// The breaker sits below the auth layer, so an attempt whose token took a
/// while starts its transport timer late. `send_unary`'s own timer, started
/// before auth, must still leave the transport time to report the timeout:
/// if it fired first, the dropped call would never be counted.
#[tokio::test]
async fn a_timeout_after_slow_auth_still_reaches_the_breaker() {
    let (base, _hits) = server().await;
    let b = breaker();
    let timeout = Duration::from_millis(200);
    let client = slow_auth_client(&b, timeout);
    let url = format!("{base}/slow");

    for _ in 0..2 {
        let r: Result<serde_json::Value, _> =
            send_unary(|| Ok(client.get(&url)), Some(timeout)).await;
        assert!(matches!(r, Err(TransportError::Timeout(_))), "{r:?}");
    }
    assert_eq!(b.state(), Breaker::Open);
}

/// The same for a stream open: its deadline also starts before auth.
#[tokio::test]
async fn a_stream_open_timeout_after_slow_auth_still_reaches_the_breaker() {
    let (base, _hits) = server().await;
    let b = breaker();
    let timeout = Duration::from_millis(200);
    let client = slow_auth_client(&b, timeout);
    let url = format!("{base}/slow");

    for _ in 0..2 {
        let (client, url) = (client.clone(), url.clone());
        let opened = open_streaming::<_, serde_json::Value>(
            StreamRequest::new(move |_: Option<&str>| Ok(client.get(&url))).open_timeout(timeout),
        )
        .await;
        assert!(
            matches!(opened, Err(TransportError::Timeout(_))),
            "{:?}",
            opened.err()
        );
    }
    assert_eq!(b.state(), Breaker::Open);
}

/// A client whose auth layer takes 20 ms, above the breaker and the
/// transport timeout.
fn slow_auth_client(b: &Arc<CircuitBreaker>, timeout: Duration) -> toolkit_http::HttpClient {
    use toolkit_contract::runtime::breaker::HttpEndpointFailure;
    use toolkit_resilience::circuit_breaker::{BoxError, CircuitBreakerLayer};
    use tower::{Layer, ServiceExt};

    let layer = CircuitBreakerLayer::new(Arc::clone(b), HttpEndpointFailure);
    toolkit_http::HttpClient::builder()
        .retry(None)
        .transport(toolkit_http::TransportSecurity::AllowInsecureHttp)
        .timeout(timeout)
        .with_auth_layer(|inner| {
            tower::util::BoxCloneService::new(tower::service_fn(move |req| {
                let inner = inner.clone();
                async move {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                    inner.oneshot(req).await
                }
            }))
        })
        .with_attempt_layer(move |inner| {
            layer
                .layer(inner)
                .map_err(
                    |e: BoxError| match e.downcast::<toolkit_http::HttpError>() {
                        Ok(http) => *http,
                        Err(other) => toolkit_http::HttpError::Transport(other),
                    },
                )
                .boxed_clone()
        })
        .build()
        .unwrap()
}

#[tokio::test]
async fn a_marked_request_bypasses_the_breaker() {
    let (base, hits) = server().await;
    let b = breaker();
    let config = ClientConfig::new(&base).with_circuit_breaker(Arc::clone(&b));
    let client = build_default_http_client("breaker-test", &config).unwrap();
    let url = format!("{base}/bad-gateway");

    for _ in 0..4 {
        let r: Result<serde_json::Value, _> = send_unary(
            || Ok(client.get(&url).extension(SkipCircuitBreaker)),
            Some(config.timeout),
        )
        .await;
        assert!(r.is_err());
    }
    assert_eq!(b.state(), Breaker::Closed);
    assert_eq!(hits.get(), 4);
}
