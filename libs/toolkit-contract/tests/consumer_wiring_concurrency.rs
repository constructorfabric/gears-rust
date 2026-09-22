//! Behavioural test that a `ClientTuning` carried by consumer wiring actually
//! reaches the transport of a directory-resolving client — the
//! `#[toolkit::consumes]` path.
//!
//! Previously the resolving client wired by `#[toolkit::consumes]` was always
//! built with `ClientTuning::default()`, so `consumer_wiring.<dep>` knobs like
//! `max_concurrent_requests` could never take effect. This test pins the fix at
//! the layer the runtime hands off to: a [`DirectoryResolvingClient`]
//! constructed with `max_concurrent_requests: Some(1)` must apply that cap when
//! it (re)builds the underlying HTTP client, so a second in-flight request is
//! shed with the non-transient [`TransportError::Overloaded`] — mirroring the
//! `ClientConfig`-level proof in `concurrency_limit.rs`.

#![cfg(feature = "rest-client")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use axum::Router;
use axum::extract::State;
use axum::response::IntoResponse;
use axum::routing::get;
use tokio::sync::Notify;

use toolkit_contract::runtime::client::{build_default_http_client, send_unary};
use toolkit_contract::runtime::config::ClientConfig;
use toolkit_contract::runtime::resolving::{
    DirectoryResolvingClient, EndpointResolver, ResolveError,
};
use toolkit_contract::runtime::transport_error::TransportError;
use toolkit_contract::wiring::ClientTuning;

#[derive(Clone)]
struct HoldState {
    arrived: Arc<Notify>,
    release: Arc<Notify>,
}

async fn hold_handler(State(state): State<HoldState>) -> impl IntoResponse {
    state.arrived.notify_one();
    state.release.notified().await;
    axum::Json(serde_json::json!({}))
}

async fn start_hold_server() -> (String, HoldState) {
    let state = HoldState {
        arrived: Arc::new(Notify::new()),
        release: Arc::new(Notify::new()),
    };
    let app = Router::new()
        .route("/hold", get(hold_handler))
        .with_state(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (format!("http://{addr}"), state)
}

/// Resolver that always yields one fixed endpoint (the test server), standing
/// in for the runtime's directory/static resolver.
struct FixedResolver(String);

#[async_trait]
impl EndpointResolver for FixedResolver {
    async fn resolve_endpoint(&self, _gear: &str) -> Result<Option<String>, ResolveError> {
        Ok(Some(self.0.clone()))
    }
}

/// A `ClientTuning` with `max_concurrent_requests: 1` — as a consumer would set
/// in `consumer_wiring.<dep>` — must be honoured by the resolving client: with
/// the single permit held by a first in-flight request, a second is shed with
/// `TransportError::Overloaded`.
#[tokio::test]
async fn consumer_tuning_concurrency_cap_sheds_second_request() {
    let (base_url, state) = start_hold_server().await;
    let url = format!("{base_url}/hold");

    // The knob a deployment would put under `consumer_wiring.<dep>`.
    let tuning = ClientTuning {
        max_concurrent_requests: Some(1),
        ..ClientTuning::default()
    };

    // The resolving client builds the underlying HTTP client from
    // `tuning.apply_to(endpoint)` on first use — the exact hand-off the
    // `#[toolkit::consumes]`-wired client performs at runtime.
    let resolving: DirectoryResolvingClient<toolkit_http::HttpClient> =
        DirectoryResolvingClient::new(
            Arc::new(FixedResolver(base_url.clone())),
            "billing",
            tuning,
            |cfg: ClientConfig| {
                build_default_http_client("consumer-wiring-concurrency-test", &cfg)
                    .map_err(TransportError::network)
            },
        );

    // Resolve once; both requests share this single tuned client (and its
    // single-permit limiter), just as concurrent business calls would.
    let client = resolving
        .resolved()
        .await
        .expect("resolves to fixed endpoint");

    // Request A occupies the only permit and blocks in the handler.
    let client_a = Arc::clone(&client);
    let url_a = url.clone();
    let a = tokio::spawn(async move {
        send_unary::<_, serde_json::Value>(
            || Ok(client_a.get(&url_a)),
            Some(Duration::from_secs(5)),
        )
        .await
    });

    state.arrived.notified().await;

    // Request B finds no permit → shed immediately.
    let b =
        send_unary::<_, serde_json::Value>(|| Ok(client.get(&url)), Some(Duration::from_secs(5)))
            .await;
    assert!(
        matches!(b, Err(TransportError::Overloaded)),
        "second request under a consumer cap of 1 should be shed with Overloaded, got: {b:?}"
    );
    assert!(
        !TransportError::Overloaded.is_transient(),
        "Overloaded must be non-transient so the SDK retry loop does not re-issue it"
    );

    state.release.notify_one();
    assert!(
        a.await.unwrap().is_ok(),
        "first request should succeed once released (the permit was really held)"
    );
}
