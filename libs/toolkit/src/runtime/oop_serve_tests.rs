//! Tests for the `OoP` HTTP serve edge (probes, drain guard, auth wiring).

use super::*;
use axum::body::{Body, to_bytes};
use axum::http::Request;
use serde_json::Value;
use tower::ServiceExt; // for `oneshot`

use crate::healthcheck::RestHealthcheckRegistry;

/// Build a readiness state with an empty healthcheck registry (no gear checks).
fn readiness<I, S>(deps: I) -> Arc<ReadinessState>
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
{
    ReadinessState::new(deps, Arc::new(RestHealthcheckRegistry::new()))
}

fn probe_router() -> Router {
    let readiness = readiness(Vec::<String>::new());
    let late = LateRoutes::default();
    late.publish(Router::new(), Arc::from("{\"openapi\":\"3.1.0\"}"));
    build_probe_router(readiness, late)
}

#[tokio::test]
async fn healthz_is_always_ok() {
    let app = probe_router();
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/healthz")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    assert_eq!(bytes, "ok");
}

#[tokio::test]
async fn readyz_reports_503_until_deps_resolved_then_200() {
    let readiness = readiness(["billing"]);
    let app = build_probe_router(Arc::clone(&readiness), LateRoutes::default());

    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/readyz")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    let bytes = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    let body: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(body["state"], "starting");
    assert_eq!(body["ready"], false);

    readiness.mark_dep_resolved("billing");
    readiness.mark_startup_complete();

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/readyz")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    let body: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(body["state"], "ready");
    assert_eq!(body["ready"], true);
}

#[tokio::test]
async fn health_returns_full_report() {
    let app = probe_router();
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/health")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    let body: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(body["status"], "healthy");
    assert_eq!(body["components"], serde_json::json!([]));
}

#[tokio::test]
async fn readyz_503_when_draining() {
    let readiness = readiness(Vec::<String>::new());
    readiness.mark_startup_complete();
    readiness.set_draining(true);
    let app = build_probe_router(Arc::clone(&readiness), LateRoutes::default());

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/readyz")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
}

#[tokio::test]
async fn well_known_openapi_is_served() {
    let app = probe_router();
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/.well-known/openapi.json")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(
        resp.headers()
            .get(axum::http::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok()),
        Some("application/json")
    );
}

#[tokio::test]
async fn drain_guard_rejects_new_requests_while_draining() {
    let readiness = readiness(Vec::<String>::new());
    let guard = DrainGuard::new(Arc::clone(&readiness));

    let gear = Router::new().route("/work", get(|| async { "done" }));
    let app = gear.layer(from_fn_with_state(guard.clone(), drain_guard_middleware));

    // Not draining: request succeeds.
    let resp = app
        .clone()
        .oneshot(Request::builder().uri("/work").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // Draining: request rejected with 503 + Retry-After.
    readiness.set_draining(true);
    let resp = app
        .oneshot(Request::builder().uri("/work").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert!(resp.headers().get(RETRY_AFTER).is_some());
}

#[tokio::test]
async fn drain_guard_tracks_in_flight_and_resets() {
    let readiness = readiness(Vec::<String>::new());
    let guard = DrainGuard::new(readiness);
    assert_eq!(guard.in_flight(), 0);

    let gear = Router::new().route("/work", get(|| async { "done" }));
    let app = gear.layer(from_fn_with_state(guard.clone(), drain_guard_middleware));

    let resp = app
        .oneshot(Request::builder().uri("/work").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    // After completion the counter returns to zero.
    assert_eq!(guard.in_flight(), 0);
}

#[tokio::test]
async fn drain_in_flight_returns_when_zero() {
    let readiness = readiness(Vec::<String>::new());
    let guard = DrainGuard::new(readiness);
    // No in-flight requests: must return promptly.
    drain_in_flight(&guard, Duration::from_secs(5)).await;
    assert_eq!(guard.in_flight(), 0);
}

async fn panicking_handler() -> &'static str {
    panic!("handler exploded");
}

#[tokio::test]
async fn drain_guard_catches_handler_panic_without_leaking_in_flight() {
    let readiness = readiness(Vec::<String>::new());
    let guard = DrainGuard::new(readiness);

    let gear = Router::new().route("/boom", get(panicking_handler));
    let app = gear.layer(from_fn_with_state(guard.clone(), drain_guard_middleware));

    let resp = app
        .oneshot(Request::builder().uri("/boom").body(Body::empty()).unwrap())
        .await
        .unwrap();

    // The panic is converted to a 500 (client gets a response, not a dropped
    // connection)...
    assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);
    // ...and, crucially, the in-flight counter returns to zero so graceful drain
    // cannot stall for the full timeout.
    assert_eq!(guard.in_flight(), 0);
}

// --- auth adapter smoke tests ---

struct AllowAuthN;
impl BearerAuthenticator for AllowAuthN {
    async fn authenticate(&self, _token: &str) -> Result<SecurityContext, AuthNError> {
        Ok(SecurityContext::anonymous())
    }
}

#[tokio::test]
async fn dyn_bearer_adapter_delegates() {
    let dynamic = DynBearerAuthenticator::new(AllowAuthN);
    let result = BearerAuthenticator::authenticate(&dynamic, "token").await;
    assert!(result.is_ok());
}

struct AllowInternal;
impl InternalAuthenticator for AllowInternal {
    async fn authenticate(&self, _token: &str) -> Result<PlatformIdentity, InternalAuthNError> {
        Ok(PlatformIdentity::KubernetesServiceAccount {
            namespace: "ns".to_owned(),
            service_account: "sa".to_owned(),
            pod: None,
        })
    }
}

#[tokio::test]
async fn dyn_internal_adapter_delegates() {
    let dynamic = DynInternalAuthenticator::new(AllowInternal);
    let result = InternalAuthenticator::authenticate(&dynamic, "token").await;
    assert!(result.is_ok());
}

// --- end-to-end: real bound server, acceptance criteria ---

use std::net::SocketAddr;
use std::sync::atomic::AtomicUsize;

use async_trait::async_trait;
use cf_system_sdks::directory::{
    DirectoryClient, RegisterInstanceInfo, ServiceEndpoint, ServiceInstanceInfo,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// Stub directory that fails `resolve_rest_service` a fixed number of times
/// before succeeding, so `/readyz` can be observed transitioning 503 → 200.
struct E2eDirectory {
    fail_resolve: AtomicUsize,
    deregistered: AtomicUsize,
}

#[async_trait]
impl DirectoryClient for E2eDirectory {
    async fn resolve_grpc_service(&self, _s: &str) -> anyhow::Result<ServiceEndpoint> {
        Ok(ServiceEndpoint::new("http://grpc"))
    }
    async fn resolve_rest_service(&self, gear: &str) -> anyhow::Result<ServiceEndpoint> {
        if self.fail_resolve.load(Ordering::SeqCst) > 0 {
            self.fail_resolve.fetch_sub(1, Ordering::SeqCst);
            anyhow::bail!("not yet");
        }
        Ok(ServiceEndpoint::new(format!("http://{gear}:8080")))
    }
    async fn get_openapi_spec(&self, _g: &str) -> anyhow::Result<String> {
        Ok(String::new())
    }
    async fn list_instances(&self, _g: &str) -> anyhow::Result<Vec<ServiceInstanceInfo>> {
        Ok(vec![])
    }
    async fn register_instance(&self, _i: RegisterInstanceInfo) -> anyhow::Result<()> {
        Ok(())
    }
    async fn deregister_instance(&self, _g: &str, _i: &str) -> anyhow::Result<()> {
        self.deregistered.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
    async fn send_heartbeat(&self, _g: &str, _i: &str) -> anyhow::Result<()> {
        Ok(())
    }
}

/// Reserve an ephemeral port and return it (closing the listener so the server
/// can rebind it).
fn free_addr() -> SocketAddr {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.local_addr().unwrap()
}

/// Minimal raw HTTP/1.1 GET returning the status code. Uses `Connection: close`
/// so the read completes on EOF.
async fn http_get(addr: SocketAddr, path: &str) -> Option<u16> {
    let mut stream = tokio::net::TcpStream::connect(addr).await.ok()?;
    let req = format!("GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n");
    stream.write_all(req.as_bytes()).await.ok()?;
    let mut buf = Vec::new();
    stream.read_to_end(&mut buf).await.ok()?;
    let text = String::from_utf8_lossy(&buf);
    text.lines()
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|s| s.parse().ok())
}

/// Poll `path` until it returns `want` or the deadline elapses.
async fn poll_status(addr: SocketAddr, path: &str, want: u16, timeout: Duration) -> bool {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if http_get(addr, path).await == Some(want) {
            return true;
        }
        if tokio::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

#[tokio::test]
async fn e2e_startup_readiness_transition_and_graceful_shutdown() {
    let addr = free_addr();
    let directory: Arc<dyn DirectoryClient> = Arc::new(E2eDirectory {
        fail_resolve: AtomicUsize::new(3),
        deregistered: AtomicUsize::new(0),
    });

    let readiness = readiness(["dep"]);
    let cancel = CancellationToken::new();

    let gear_router = Router::new().route("/ping", get(|| async { "pong" }));

    let options = OopServeOptions {
        gear_name: "test-gear".to_owned(),
        instance_id: "i-1".to_owned(),
        version: Some("1.0.0".to_owned()),
        advertise_uri: format!("http://{addr}"),
        listen_addr: addr,
        probe_bind_addr: None,
        drain_timeout: Duration::from_secs(5),
        heartbeat_interval: Duration::from_secs(30),
        healthcheck_timeout: Duration::from_millis(500),
        directory: Arc::clone(&directory),
        bearer_authenticator: None,
        internal_authenticator: None,
    };

    let mut server = super::OopHttpServer::start(Arc::clone(&readiness), options, cancel.clone())
        .await
        .expect("server should bind");

    // 1. Liveness is up BEFORE gear routes are attached (probe-first bind).
    assert!(
        poll_status(addr, "/healthz", 200, Duration::from_secs(3)).await,
        "/healthz should return 200 as soon as the listener binds, before start()"
    );

    // 2. Gear routes reply 503 "starting" until attached.
    assert_eq!(
        http_get(addr, "/ping").await,
        Some(503),
        "gear route should be 503 starting before attach"
    );

    // 3. Attach the composed gear routes (simulates post-start()).
    server.attach(gear_router, "{\"openapi\":\"3.1.0\"}".to_owned());

    // 4. Gear routes now serve.
    assert!(
        poll_status(addr, "/ping", 200, Duration::from_secs(3)).await,
        "gear route should serve after attach"
    );

    // 5. /readyz is 503 while the consumed dependency is unresolved. Dependency
    // resolution now happens in the proxy-wiring phase (typed
    // `#[toolkit::consumes]` clients feeding the shared DependencyChecker); here
    // we simulate that resolution to prove the probe flips 503 → 200.
    assert_eq!(
        http_get(addr, "/readyz").await,
        Some(503),
        "/readyz should be 503 while `dep` is unresolved"
    );
    readiness.mark_dep_resolved("dep");
    assert!(
        poll_status(addr, "/readyz", 200, Duration::from_secs(3)).await,
        "/readyz should transition to 200 after dep resolves"
    );
    assert!(readiness.all_deps_resolved());

    // 6. Well-known OpenAPI is served (published at attach).
    assert_eq!(http_get(addr, "/.well-known/openapi.json").await, Some(200));

    // 7. Graceful shutdown completes.
    cancel.cancel();
    let result = tokio::time::timeout(Duration::from_secs(5), server.join())
        .await
        .expect("server should finish promptly after cancel");
    assert!(
        result.is_ok(),
        "graceful shutdown should succeed: {result:?}"
    );
}

#[tokio::test]
async fn ephemeral_listen_port_updates_advertise_uri() {
    let readiness = readiness(Vec::<String>::new());
    let cancel = CancellationToken::new();

    let options = OopServeOptions {
        gear_name: "ephemeral-gear".to_owned(),
        instance_id: "i-1".to_owned(),
        version: None,
        advertise_uri: "http://127.0.0.1:0".to_owned(),
        listen_addr: "127.0.0.1:0".parse().unwrap(),
        probe_bind_addr: None,
        drain_timeout: Duration::from_secs(5),
        heartbeat_interval: Duration::from_secs(30),
        healthcheck_timeout: Duration::from_millis(500),
        directory: Arc::new(E2eDirectory {
            fail_resolve: AtomicUsize::new(0),
            deregistered: AtomicUsize::new(0),
        }),
        bearer_authenticator: None,
        internal_authenticator: None,
    };

    let server = super::OopHttpServer::start(readiness, options, cancel.clone())
        .await
        .expect("server should bind on ephemeral port");

    let actual_port = server.options().listen_addr.port();
    assert_ne!(actual_port, 0, "ephemeral port should be assigned");

    let advertise_url =
        url::Url::parse(&server.options().advertise_uri).expect("advertise_uri should parse");
    assert_eq!(
        advertise_url.port_or_known_default(),
        Some(actual_port),
        "advertise_uri should use the actually bound port, got {}",
        server.options().advertise_uri
    );

    cancel.cancel();
    let result = tokio::time::timeout(Duration::from_secs(5), server.join())
        .await
        .expect("server should shut down promptly after cancel");
    assert!(result.is_ok(), "server shutdown should succeed: {result:?}");
}

#[tokio::test]
async fn user_advertise_uri_is_not_overwritten_by_ephemeral_bind_port() {
    let readiness = readiness(Vec::<String>::new());
    let cancel = CancellationToken::new();

    let options = OopServeOptions {
        gear_name: "ephemeral-gear".to_owned(),
        instance_id: "i-1".to_owned(),
        version: None,
        advertise_uri: "http://load-balancer.example.com:8080".to_owned(),
        listen_addr: "127.0.0.1:0".parse().unwrap(),
        probe_bind_addr: None,
        drain_timeout: Duration::from_secs(5),
        heartbeat_interval: Duration::from_secs(30),
        healthcheck_timeout: Duration::from_millis(500),
        directory: Arc::new(E2eDirectory {
            fail_resolve: AtomicUsize::new(0),
            deregistered: AtomicUsize::new(0),
        }),
        bearer_authenticator: None,
        internal_authenticator: None,
    };

    let server = super::OopHttpServer::start(readiness, options, cancel.clone())
        .await
        .expect("server should bind on ephemeral port");

    assert_eq!(
        server.options().advertise_uri,
        "http://load-balancer.example.com:8080",
        "user-provided advertise_uri with a non-matching port must be preserved"
    );

    cancel.cancel();
    let result = tokio::time::timeout(Duration::from_secs(5), server.join())
        .await
        .expect("server should shut down promptly after cancel");
    assert!(result.is_ok(), "server shutdown should succeed: {result:?}");
}

#[tokio::test]
async fn gear_handler_receives_connect_info_through_fallback() {
    use axum::extract::ConnectInfo;

    async fn peer(ConnectInfo(peer): ConnectInfo<SocketAddr>) -> String {
        peer.to_string()
    }

    let addr = free_addr();
    let readiness = readiness(Vec::<String>::new());
    let cancel = CancellationToken::new();

    let options = OopServeOptions {
        gear_name: "peer-gear".to_owned(),
        instance_id: "i-1".to_owned(),
        version: None,
        advertise_uri: format!("http://{addr}"),
        listen_addr: addr,
        probe_bind_addr: None,
        drain_timeout: Duration::from_secs(5),
        heartbeat_interval: Duration::from_secs(30),
        healthcheck_timeout: Duration::from_millis(500),
        directory: Arc::new(E2eDirectory {
            fail_resolve: AtomicUsize::new(0),
            deregistered: AtomicUsize::new(0),
        }),
        bearer_authenticator: None,
        internal_authenticator: None,
    };

    let mut server = super::OopHttpServer::start(readiness, options, cancel.clone())
        .await
        .expect("server should bind");

    server.attach(Router::new().route("/peer", get(peer)), "{}".to_owned());

    // A `200` proves the gear handler's `ConnectInfo<SocketAddr>` extractor found
    // the connect-info in the request extensions *through* the swap fallback — a
    // missing extension would surface as `500`.
    assert!(
        poll_status(addr, "/peer", 200, Duration::from_secs(3)).await,
        "gear handler extracting ConnectInfo must succeed through the fallback"
    );

    cancel.cancel();
    let joined = tokio::time::timeout(Duration::from_secs(5), server.join()).await;
    assert!(
        joined.is_ok(),
        "server should shut down promptly after cancel"
    );
}
