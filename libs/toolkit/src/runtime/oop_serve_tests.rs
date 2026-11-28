//! Tests for the `OoP` HTTP serve edge (probes, drain guard, auth wiring).

use super::*;
use axum::body::{Body, to_bytes};
use axum::http::{Method, Request};
use serde_json::Value;
use tower::ServiceExt; // for `oneshot`

use crate::healthcheck::RestHealthcheckRegistry;
use toolkit_security::{
    AuthNError, BearerAuthenticator, DynBearerAuthenticator, DynInternalAuthenticator,
    InternalAuthNError, InternalAuthenticator, PlatformIdentity, SecurityContext,
};

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
/// Registrations are captured so a test can assert the serve path forwards the
/// configured deployment labels.
#[derive(Default)]
struct E2eDirectory {
    fail_resolve: AtomicUsize,
    deregistered: AtomicUsize,
    registrations: std::sync::Mutex<Vec<RegisterInstanceInfo>>,
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
    async fn list_instances(&self, _g: &str) -> anyhow::Result<Vec<ServiceInstanceInfo>> {
        Ok(vec![])
    }
    async fn list_all_instances(&self) -> anyhow::Result<Vec<ServiceInstanceInfo>> {
        Ok(vec![])
    }
    async fn register_instance(&self, i: RegisterInstanceInfo) -> anyhow::Result<()> {
        self.registrations.lock().unwrap().push(i);
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
        ..Default::default()
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
        request_limits: None,
        directory: Arc::clone(&directory),
        bearer_authenticator: None,
        internal_authenticator: None,
        // No authenticator: the raw routes run only in development mode.
        auth_disabled: true,
        labels: std::collections::BTreeMap::new(),
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
    server
        .attach(
            gear_router,
            "{\"openapi\":\"3.1.0\"}".to_owned(),
            OopRoutePolicy::default(),
        )
        .expect("attach");
    server.start_presence();

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
async fn attach_forwards_configured_labels_to_directory_registration() {
    let addr = free_addr();
    let directory = Arc::new(E2eDirectory::default());
    let readiness = readiness(Vec::<String>::new());
    let cancel = CancellationToken::new();

    let labels: std::collections::BTreeMap<String, String> =
        [("shard".to_owned(), "7".to_owned())].into_iter().collect();

    let options = OopServeOptions {
        gear_name: "shard-gear".to_owned(),
        instance_id: "i-1".to_owned(),
        version: None,
        advertise_uri: format!("http://{addr}"),
        listen_addr: addr,
        probe_bind_addr: None,
        drain_timeout: Duration::from_secs(5),
        heartbeat_interval: Duration::from_secs(30),
        healthcheck_timeout: Duration::from_millis(500),
        request_limits: None,
        directory: Arc::clone(&directory) as Arc<dyn DirectoryClient>,
        bearer_authenticator: None,
        internal_authenticator: None,
        auth_disabled: false,
        labels: labels.clone(),
    };

    let mut server = super::OopHttpServer::start(readiness, options, cancel.clone())
        .await
        .expect("server should bind");

    // `attach` spawns the presence loop, which registers immediately.
    server
        .attach(
            Router::new().route("/ping", get(|| async { "pong" })),
            "{}".to_owned(),
            OopRoutePolicy::default(),
        )
        .expect("attach");
    server.start_presence();

    // Poll until the registration is captured (registration is async).
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    let captured = loop {
        if let Some(info) = directory.registrations.lock().unwrap().first().cloned() {
            break Some(info);
        }
        if tokio::time::Instant::now() >= deadline {
            break None;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    };

    cancel.cancel();
    let shutdown = tokio::time::timeout(Duration::from_secs(5), server.join()).await;
    assert!(shutdown.is_ok(), "server should shut down promptly");

    let captured = captured.expect("presence loop should register the instance");
    assert_eq!(
        captured.labels, labels,
        "configured deployment labels must reach the directory registration"
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
        request_limits: None,
        directory: Arc::new(E2eDirectory::default()),
        bearer_authenticator: None,
        internal_authenticator: None,
        auth_disabled: false,
        labels: std::collections::BTreeMap::new(),
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
        request_limits: None,
        directory: Arc::new(E2eDirectory::default()),
        bearer_authenticator: None,
        internal_authenticator: None,
        auth_disabled: false,
        labels: std::collections::BTreeMap::new(),
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
        request_limits: None,
        directory: Arc::new(E2eDirectory::default()),
        bearer_authenticator: None,
        internal_authenticator: None,
        // No authenticator: the raw routes run only in development mode.
        auth_disabled: true,
        labels: std::collections::BTreeMap::new(),
    };

    let mut server = super::OopHttpServer::start(readiness, options, cancel.clone())
        .await
        .expect("server should bind");

    server
        .attach(
            Router::new().route("/peer", get(peer)),
            "{}".to_owned(),
            OopRoutePolicy::default(),
        )
        .expect("attach");
    server.start_presence();

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

/// An `OoP` gear must continue the caller's W3C trace, so the wire `trace_id`
/// the canonical-error layer emits is the same id that appears in this gear's
/// own log lines. Send a request carrying a `traceparent` through
/// `trace_context_middleware` and assert the trace id live inside the handler
/// equals the inbound one.
#[cfg(feature = "otel")]
#[tokio::test]
async fn trace_context_middleware_continues_inbound_w3c_trace() {
    use std::sync::Mutex;

    use axum::routing::get;
    use opentelemetry::trace::TracerProvider as _;
    use tracing_subscriber::layer::SubscriberExt as _;

    // Install a W3C propagator so the middleware can extract the inbound parent,
    // and an OTel subscriber so the continued span carries a live context.
    opentelemetry::global::set_text_map_propagator(
        opentelemetry_sdk::propagation::TraceContextPropagator::new(),
    );
    let provider = opentelemetry_sdk::trace::SdkTracerProvider::builder().build();
    let tracer = provider.tracer("oop-serve-trace-test");
    let subscriber =
        tracing_subscriber::registry().with(tracing_opentelemetry::layer().with_tracer(tracer));
    crate::test_tracing::keep_interest_shared();
    let _guard = tracing::subscriber::set_default(subscriber);

    let inbound_trace_id = "4bf92f3577b34da6a3ce929d0e0e4736";

    let captured: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let captured_handler = Arc::clone(&captured);

    let app = Router::new()
        .route(
            "/x",
            get(move || {
                let captured_handler = Arc::clone(&captured_handler);
                async move {
                    *captured_handler.lock().unwrap() = toolkit_trace_context::current_trace_id();
                    StatusCode::OK
                }
            }),
        )
        .layer(from_fn(trace_context_middleware));

    let req = Request::builder()
        .uri("/x")
        .header(
            "traceparent",
            format!("00-{inbound_trace_id}-00f067aa0ba902b7-01"),
        )
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    assert_eq!(
        captured.lock().unwrap().as_deref(),
        Some(inbound_trace_id),
        "the gear request span must continue the inbound W3C trace"
    );
}

#[tokio::test]
async fn the_private_fallback_forwards_only_the_current_requests_single_valid_traceparent() {
    use axum::http::{HeaderMap, HeaderName, HeaderValue};
    use tokio::sync::Barrier;
    use toolkit_trace_context::TRACEPARENT;

    const FIRST: &str = "00-0123456789abcdef0123456789abcdef-1111111111111111-01";
    const SECOND: &str = "00-1123456789abcdef0123456789abcdef-2222222222222222-00";
    const ACTIVE: &str = "00-2123456789abcdef0123456789abcdef-3333333333333333-01";
    const NAME: HeaderName = HeaderName::from_static(TRACEPARENT);

    fn inspect(mut output: HeaderMap) -> axum::Extension<HeaderMap> {
        inject_inbound_traceparent(&mut output);
        axum::Extension(output)
    }

    async fn forwarded(router: Router, parents: &[&str]) -> HeaderMap {
        let mut request = Request::builder().uri("/");
        for parent in parents {
            request = request.header(TRACEPARENT, *parent);
        }
        router
            .oneshot(request.body(Body::empty()).unwrap())
            .await
            .unwrap()
            .extensions_mut()
            .remove::<HeaderMap>()
            .expect("handler captured the complete outgoing headers")
    }

    assert_eq!(inspect(HeaderMap::new()).0, HeaderMap::new());

    let barrier = Arc::new(Barrier::new(2));
    let overlapping = Router::new()
        .route(
            "/",
            get(move || {
                let barrier = Arc::clone(&barrier);
                async move {
                    barrier.wait().await;
                    inspect(HeaderMap::new())
                }
            }),
        )
        .layer(from_fn(trace_context_middleware));
    let (first, second) = tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(
            forwarded(overlapping.clone(), &[FIRST]),
            forwarded(overlapping, &[SECOND])
        )
    })
    .await
    .expect("both request handlers reach the barrier");
    assert_eq!(
        first,
        HeaderMap::from_iter([(NAME, HeaderValue::from_static(FIRST))])
    );
    assert_eq!(
        second,
        HeaderMap::from_iter([(NAME, HeaderValue::from_static(SECOND))])
    );

    let outer = Router::new()
        .route(
            "/",
            get(|| async {
                let inner = Router::new()
                    .route("/", get(|| async { inspect(HeaderMap::new()) }))
                    .layer(from_fn(trace_context_middleware));
                for parents in [
                    &[][..],
                    &["invalid"],
                    &["00-0123456789abcdef0123456789abcdef-0000000000000000-01"],
                    &["01-0123456789abcdef0123456789abcdef-1111111111111111-01"],
                    &["00-0123456789abcdef0123456789abcdef-1111111111111111-0"],
                    &["00-0123456789abcdef0123456789abcdef-1111111111111111-010"],
                    &[FIRST, SECOND],
                    &[FIRST, FIRST],
                ] {
                    assert_eq!(forwarded(inner.clone(), parents).await, HeaderMap::new());
                }
                let active = HeaderMap::from_iter([(NAME, HeaderValue::from_static(ACTIVE))]);
                assert_eq!(
                    inspect(active).0,
                    HeaderMap::from_iter([(NAME, HeaderValue::from_static(ACTIVE))])
                );
                inspect(HeaderMap::new())
            }),
        )
        .layer(from_fn(trace_context_middleware));
    assert_eq!(
        forwarded(outer, &[FIRST]).await,
        HeaderMap::from_iter([(NAME, HeaderValue::from_static(FIRST))])
    );
    assert_eq!(inspect(HeaderMap::new()).0, HeaderMap::new());
}

/// A gateway-supplied `x-request-id` must be installed as the task-local
/// carrier for the whole downstream pipeline, so a gear handler (and any
/// `toolkit-http` call it makes) sees the edge id.
#[tokio::test]
async fn trace_context_middleware_installs_request_id_carrier() {
    use std::sync::Mutex;

    use axum::routing::get;

    let captured: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let captured_handler = Arc::clone(&captured);

    let app = Router::new()
        .route(
            "/x",
            get(move || {
                let captured_handler = Arc::clone(&captured_handler);
                async move {
                    *captured_handler.lock().unwrap() = toolkit_trace_context::current_request_id();
                    StatusCode::OK
                }
            }),
        )
        .layer(from_fn(trace_context_middleware));

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/x")
                .header("x-request-id", "edge-req-1")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(
        captured.lock().unwrap().as_deref(),
        Some("edge-req-1"),
        "the inbound x-request-id must be scoped for downstream propagation"
    );
}

/// With no inbound `x-request-id` the carrier stays empty — a gear reached
/// without the gateway mints nothing.
#[tokio::test]
async fn trace_context_middleware_without_header_leaves_carrier_empty() {
    use std::sync::Mutex;

    use axum::routing::get;

    let captured: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let captured_handler = Arc::clone(&captured);

    let app = Router::new()
        .route(
            "/x",
            get(move || {
                let captured_handler = Arc::clone(&captured_handler);
                async move {
                    *captured_handler.lock().unwrap() = toolkit_trace_context::current_request_id();
                    StatusCode::OK
                }
            }),
        )
        .layer(from_fn(trace_context_middleware));

    let resp = app
        .oneshot(Request::builder().uri("/x").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert!(captured.lock().unwrap().is_none());
}

/// The fix depends on the canonical-error layer running *inside* the trace span,
/// which is a property of the layering order in [`layer_gear_router`], not of
/// `trace_context_middleware` alone. Build the router the production way and,
/// crucially, send **no** inbound `traceparent`: the gear span is then a fresh
/// root, so the trace id the canonical-error layer stamps on the problem body
/// can only be non-null if that layer sees the live span. Were the two layers
/// swapped, the error would be built outside the span with no header to fall
/// back on, and the problem would carry no `trace_id` — so this pins the order.
#[cfg(feature = "otel")]
#[tokio::test]
async fn canonical_error_layer_runs_inside_the_trace_span_when_composed() {
    use axum::routing::get;
    use opentelemetry::trace::TracerProvider as _;
    use tracing_subscriber::layer::SubscriberExt as _;

    // A gear route that fails: the canonical-error layer turns the error into a
    // problem+json and fills its `trace_id` from the id current at that point.
    async fn boom() -> crate::api::canonical_prelude::ApiResult<StatusCode> {
        Err(CanonicalError::internal("boom").create())
    }

    opentelemetry::global::set_text_map_propagator(
        opentelemetry_sdk::propagation::TraceContextPropagator::new(),
    );
    let provider = opentelemetry_sdk::trace::SdkTracerProvider::builder().build();
    let tracer = provider.tracer("oop-serve-canonical-order-test");
    let subscriber =
        tracing_subscriber::registry().with(tracing_opentelemetry::layer().with_tracer(tracer));

    let readiness = readiness(Vec::<String>::new());
    // No authenticator: the unlisted route runs only in development mode.
    let options = OopServeOptions::new(
        "order-gear".to_owned(),
        "i-1".to_owned(),
        "http://localhost".to_owned(),
        free_addr(),
        Arc::new(E2eDirectory::default()),
    )
    .with_auth_disabled(true);
    let app = layer_gear_router(
        Router::new().route("/boom", get(boom)),
        OopRoutePolicy::default(),
        options.tenant_plane(),
        DrainGuard::new(readiness),
        &options,
    );

    // Register the request span's callsite before installing the subscriber, so the
    // interest rebuild `set_default` runs covers it. Otherwise a parallel test that hits
    // the callsite first can cache it as disabled for this subscriber too.
    let warm_up = app
        .clone()
        .oneshot(Request::builder().uri("/boom").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert!(warm_up.status().is_server_error(), "the gear route failed");
    crate::test_tracing::keep_interest_shared();
    let _guard = tracing::subscriber::set_default(subscriber);

    let resp = app
        .oneshot(Request::builder().uri("/boom").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert!(resp.status().is_server_error(), "the gear route failed");

    let bytes = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    let problem: Value = serde_json::from_slice(&bytes).unwrap();
    let trace_id = problem.get("trace_id").and_then(Value::as_str);
    assert!(
        trace_id.is_some_and(|id| id.len() == 32),
        "the canonical-error layer must run inside the trace span, so the problem \
         carries the live span's 32-hex trace_id even with no inbound traceparent; got {problem}"
    );
}

// Auth assembly from OperationBuilder specs through the listener policy.

const AXIS_BEARER: &str = "Bearer tenant-token";
const AXIS_SA: &str = "good-sa-token";

/// Tenant authenticator: `tenant-token` is a real subject, the rest rejected.
struct AxisBearer;

impl BearerAuthenticator for AxisBearer {
    async fn authenticate(&self, token: &str) -> Result<SecurityContext, AuthNError> {
        if token == "tenant-token" {
            Ok(SecurityContext::builder()
                .subject_id(uuid::Uuid::from_u128(1))
                .subject_tenant_id(uuid::Uuid::from_u128(2))
                .build()
                .unwrap())
        } else {
            Err(AuthNError::InvalidToken)
        }
    }
}

/// Platform authenticator: `good-sa-token` is a workload, the rest rejected.
struct AxisInternal;

impl InternalAuthenticator for AxisInternal {
    async fn authenticate(&self, token: &str) -> Result<PlatformIdentity, InternalAuthNError> {
        if token == AXIS_SA {
            Ok(PlatformIdentity::KubernetesServiceAccount {
                namespace: "toolkit".to_owned(),
                service_account: "types-registry".to_owned(),
                pod: None,
            })
        } else {
            Err(InternalAuthNError::InvalidToken)
        }
    }
}

/// `OperationBuilder` routes: `/axis/v1/p` platform, `/anon` anonymous, `/auth` authenticated.
fn axis_gear(bearer: bool, internal: bool) -> Router {
    use crate::api::{OpenApiRegistryImpl, OperationBuilder};

    async fn ok() -> &'static str {
        "ok"
    }

    let registry = OpenApiRegistryImpl::new();
    let mut router = Router::new();
    router = OperationBuilder::get("/axis/v1/p")
        .operation_id("axis.get")
        .platform_authenticated()
        .no_license_required()
        .handler(ok)
        .json_response(StatusCode::OK, "OK")
        .register(router, &registry);
    router = OperationBuilder::post("/axis/v1/p")
        .operation_id("axis.post")
        .platform_authenticated()
        .no_license_required()
        .handler(ok)
        .json_response(StatusCode::OK, "OK")
        .register(router, &registry);
    router = OperationBuilder::get("/axis/v1/anon")
        .operation_id("axis.anon")
        .anonymous()
        .handler(ok)
        .json_response(StatusCode::OK, "OK")
        .register(router, &registry);
    router = OperationBuilder::get("/axis/v1/auth")
        .operation_id("axis.auth")
        .authenticated()
        .no_license_required()
        .handler(ok)
        .json_response(StatusCode::OK, "OK")
        .register(router, &registry);

    let mut options = OopServeOptions::new(
        "axis-gear".to_owned(),
        "i-1".to_owned(),
        "http://localhost".to_owned(),
        free_addr(),
        Arc::new(E2eDirectory::default()),
    );
    if bearer {
        options = options.with_bearer_authenticator(Some(DynBearerAuthenticator::new(AxisBearer)));
    }
    if internal {
        options =
            options.with_internal_authenticator(Some(DynInternalAuthenticator::new(AxisInternal)));
    }
    let policy = OopRoutePolicy::from_specs(registry.operation_specs.iter());
    layer_gear_router(
        router,
        policy,
        options.tenant_plane(),
        DrainGuard::new(readiness(Vec::<String>::new())),
        &options,
    )
}

async fn axis_status(
    app: Router,
    method: Method,
    path: &str,
    bearer: Option<&str>,
    token: Option<&str>,
) -> (StatusCode, Option<String>) {
    let mut builder = Request::builder().method(method).uri(path);
    if let Some(value) = bearer {
        builder = builder.header(axum::http::header::AUTHORIZATION, value);
    }
    if let Some(value) = token {
        builder = builder.header("x-toolkit-internal-token", value);
    }
    let resp = app
        .oneshot(builder.body(Body::empty()).unwrap())
        .await
        .unwrap();
    let content_type = resp
        .headers()
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    (resp.status(), content_type)
}

#[tokio::test]
async fn platform_routes_through_the_real_assembly() {
    use Method as M;
    let ok = StatusCode::OK;
    let unauthorized = StatusCode::UNAUTHORIZED;
    let p = "/axis/v1/p";
    let cases = [
        (M::GET, p, None, Some(AXIS_SA), ok, "platform GET: token"),
        (M::POST, p, None, Some(AXIS_SA), ok, "platform POST: token"),
        (
            M::POST,
            p,
            Some(AXIS_BEARER),
            Some(AXIS_SA),
            unauthorized,
            "platform: bearer + token",
        ),
        (
            M::GET,
            p,
            Some(AXIS_BEARER),
            None,
            unauthorized,
            "platform GET: bearer alone",
        ),
        (
            M::POST,
            p,
            Some(AXIS_BEARER),
            None,
            unauthorized,
            "platform POST: bearer alone",
        ),
        (M::POST, p, None, None, unauthorized, "platform: neither"),
        (
            M::POST,
            p,
            Some("Bearer forged"),
            Some(AXIS_SA),
            unauthorized,
            "platform: forged bearer + token",
        ),
        (
            M::POST,
            p,
            None,
            Some("forged"),
            unauthorized,
            "platform: forged token",
        ),
        (M::HEAD, p, None, Some(AXIS_SA), ok, "HEAD inherits: token"),
        (
            M::HEAD,
            p,
            Some(AXIS_BEARER),
            None,
            unauthorized,
            "HEAD inherits: bearer alone",
        ),
        (
            M::GET,
            "/axis/v1/anon",
            None,
            None,
            ok,
            "anonymous reaches the listener",
        ),
        (
            M::GET,
            "/axis/v1/auth",
            None,
            Some(AXIS_SA),
            unauthorized,
            "authenticated: token alone",
        ),
        (
            M::GET,
            "/axis/v1/auth",
            Some(AXIS_BEARER),
            None,
            ok,
            "authenticated: bearer",
        ),
    ];
    for (method, path, bearer, token, want, case) in cases {
        let (status, content_type) =
            axis_status(axis_gear(true, true), method, path, bearer, token).await;
        assert_eq!(status, want, "{case}");
        if want == unauthorized {
            assert_eq!(
                content_type.as_deref(),
                Some("application/problem+json"),
                "{case}"
            );
        }
    }
}

#[tokio::test]
async fn platform_routes_fail_closed_without_an_authenticator() {
    use Method as M;
    // No tenant plane: the token alone is admitted; a bearer beside it is refused as mixed
    // credentials before either plane runs.
    for (method, bearer, token, want, case) in [
        (M::GET, None, Some(AXIS_SA), StatusCode::OK, "token"),
        (
            M::POST,
            Some(AXIS_BEARER),
            Some(AXIS_SA),
            StatusCode::UNAUTHORIZED,
            "bearer + token",
        ),
    ] {
        let (status, _) =
            axis_status(axis_gear(false, true), method, "/axis/v1/p", bearer, token).await;
        assert_eq!(status, want, "no tenant plane, {case}");
    }
    // No platform plane, or neither: nothing can validate the token.
    for (bearer_plane, method, bearer) in [
        (true, M::POST, None),
        (true, M::GET, Some(AXIS_BEARER)),
        (false, M::GET, None),
        (false, M::POST, None),
    ] {
        let (status, _) = axis_status(
            axis_gear(bearer_plane, false),
            method.clone(),
            "/axis/v1/p",
            bearer,
            Some(AXIS_SA),
        )
        .await;
        assert_eq!(
            status,
            StatusCode::UNAUTHORIZED,
            "no platform plane (tenant plane {bearer_plane}), {method}"
        );
    }
}

/// `auth_disabled` is only the fallback for a missing tenant authenticator: with one
/// installed, a tenant route still needs a valid bearer, and gets its real subject.
#[tokio::test]
async fn an_installed_tenant_authenticator_wins_over_auth_disabled() {
    use crate::api::{OpenApiRegistryImpl, OperationBuilder};

    async fn subject(axum::Extension(ctx): axum::Extension<SecurityContext>) -> String {
        ctx.subject_id().to_string()
    }

    let app = || {
        let registry = OpenApiRegistryImpl::new();
        let router = OperationBuilder::get("/axis/v1/auth")
            .operation_id("axis.auth")
            .authenticated()
            .no_license_required()
            .handler(subject)
            .text_response(StatusCode::OK, "OK", "text/plain")
            .register(Router::new(), &registry);
        let options = OopServeOptions::new(
            "axis-gear".to_owned(),
            "i-1".to_owned(),
            "http://localhost".to_owned(),
            free_addr(),
            Arc::new(E2eDirectory::default()),
        )
        .with_bearer_authenticator(Some(DynBearerAuthenticator::new(AxisBearer)))
        .with_auth_disabled(true);
        let policy = OopRoutePolicy::from_specs(registry.operation_specs.iter());
        layer_gear_router(
            router,
            policy,
            options.tenant_plane(),
            DrainGuard::new(readiness(Vec::<String>::new())),
            &options,
        )
    };
    for bearer in [None, Some("Bearer forged-token")] {
        let (status, _) = axis_status(app(), Method::GET, "/axis/v1/auth", bearer, None).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "bearer {bearer:?}");
    }
    let resp = app()
        .oneshot(
            Request::builder()
                .uri("/axis/v1/auth")
                .header(axum::http::header::AUTHORIZATION, AXIS_BEARER)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    assert_eq!(
        body.as_ref(),
        uuid::Uuid::from_u128(1).to_string().as_bytes()
    );
}

/// A worker refuses to start serving tenant operations it cannot check, and says why.
/// Platform operations without an internal authenticator start: each request fails
/// closed (`platform_routes_fail_closed_without_an_authenticator`).
#[tokio::test]
async fn a_worker_refuses_to_serve_tenant_operations_it_cannot_check() {
    use toolkit_security::RouteAuth::{Anonymous, Authenticated, Platform};
    let policy = |auth: toolkit_security::RouteAuth| -> RouteAuthPolicy {
        [(Method::GET, "/op".to_owned(), auth)]
            .into_iter()
            .collect()
    };
    let start = |bearer: bool, internal: bool, auth_disabled: bool| async move {
        let options = OopServeOptions::new(
            "worker".to_owned(),
            "i-1".to_owned(),
            "http://localhost".to_owned(),
            free_addr(),
            Arc::new(E2eDirectory::default()),
        )
        .with_bearer_authenticator(bearer.then(|| DynBearerAuthenticator::new(AllowAuthN)))
        .with_internal_authenticator(internal.then(|| DynInternalAuthenticator::new(AllowInternal)))
        .with_auth_disabled(auth_disabled);
        let cancel = CancellationToken::new();
        let server =
            super::OopHttpServer::start(readiness(Vec::<String>::new()), options, cancel.clone())
                .await
                .expect("server should bind on ephemeral port");
        (server, cancel)
    };
    // (tenant authenticator, internal authenticator, auth_disabled, route, refusal names)
    let cases = [
        (
            false,
            true,
            false,
            Authenticated,
            Some("tenant authenticator"),
        ),
        (true, false, false, Platform, None),
        (true, true, false, Authenticated, None),
        (true, true, false, Platform, None),
        (false, false, false, Anonymous, None),
        (false, false, true, Authenticated, None),
        (false, false, true, Platform, None),
    ];
    for (bearer, internal, auth_disabled, auth, refusal) in cases {
        let (server, cancel) = start(bearer, internal, auth_disabled).await;
        let result = server.check_route_auth(&policy(auth));
        let case =
            format!("bearer={bearer} internal={internal} auth_disabled={auth_disabled} {auth:?}");
        match refusal {
            Some(names) => {
                let error = result.expect_err(&case).to_string();
                assert!(error.contains(names), "{case}: {error}");
            }
            None => result.unwrap_or_else(|e| panic!("{case}: {e}")),
        }
        cancel.cancel();
        tokio::time::timeout(Duration::from_secs(5), server.join())
            .await
            .expect("server should shut down promptly after cancel")
            .expect("server shuts down");
    }
}

/// Without a bearer authenticator and without `auth_disabled`, the listener's tenant plane
/// is absent, not disabled: a raw route the policy does not list is refused, never served
/// with a default context.
#[tokio::test]
async fn without_a_tenant_authenticator_an_unlisted_route_is_refused() {
    use crate::api::{OpenApiRegistryImpl, OperationBuilder};

    let registry = OpenApiRegistryImpl::new();
    let router = OperationBuilder::get("/axis/v1/anon")
        .operation_id("axis.anon")
        .anonymous()
        .handler(|| async { "anon" })
        .text_response(StatusCode::OK, "OK", "text/plain")
        .register(Router::new(), &registry)
        .route("/raw", get(|| async { "raw" }));
    let options = OopServeOptions::new(
        "axis-gear".to_owned(),
        "i-1".to_owned(),
        "http://localhost".to_owned(),
        free_addr(),
        Arc::new(E2eDirectory::default()),
    );
    let app = layer_gear_router(
        router,
        OopRoutePolicy::from_specs(registry.operation_specs.iter()),
        options.tenant_plane(),
        DrainGuard::new(readiness(Vec::<String>::new())),
        &options,
    );

    let resp = app
        .clone()
        .oneshot(Request::builder().uri("/raw").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    let body = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    let body = String::from_utf8_lossy(&body);
    assert!(body.contains("TENANT_AUTH_NOT_CONFIGURED"), "{body}");

    let (status, _) = axis_status(app, Method::GET, "/axis/v1/anon", None, None).await;
    assert_eq!(status, StatusCode::OK, "an anonymous route keeps its plane");
}

#[test]
fn request_limits_require_positive_bytes_and_duration() {
    use crate::runtime::{OopRequestLimits, OopRequestLimitsError};

    let bytes = 16 * 1024 * 1024;
    let timeout = Duration::from_secs(30);
    let limits = OopRequestLimits::new(bytes, timeout).expect("valid limits");
    assert_eq!(
        (limits.body_limit_bytes(), limits.request_timeout()),
        (16_777_216, Duration::from_secs(30))
    );
    assert_eq!(OopRequestLimits::new(bytes, timeout), Ok(limits));
    assert_eq!(
        OopRequestLimits::new(0, timeout),
        Err(OopRequestLimitsError::BodyLimitZero)
    );
    assert_eq!(
        OopRequestLimits::new(bytes, Duration::ZERO),
        Err(OopRequestLimitsError::RequestTimeoutZero)
    );
    assert_eq!(
        OopRequestLimits::new(0, Duration::ZERO),
        Err(OopRequestLimitsError::BodyLimitZero)
    );
    let minimal = OopRequestLimits::new(1, Duration::from_nanos(1)).expect("positive minimum");
    assert_eq!(
        (minimal.body_limit_bytes(), minimal.request_timeout()),
        (1, Duration::from_nanos(1))
    );
}

mod request_limits {
    use super::*;
    use axum::body::Bytes;
    use axum::http::header::{AUTHORIZATION, CONTENT_LENGTH, CONTENT_TYPE};
    use axum::routing::post;
    use futures_util::{StreamExt as _, stream};
    use serde_json::json;
    use std::convert::Infallible;
    use std::future::Future;
    use std::sync::atomic::AtomicBool;
    use tokio::sync::Notify;
    use tokio::time::{Instant, advance, timeout};
    use toolkit_gts::{gts_id, gts_uri};
    use toolkit_security::RouteAuth;

    const BOUND: Duration = Duration::from_secs(10);
    const DEADLINE: Duration = Duration::from_secs(2);
    const TRACE_ID: &str = "11111111111111111111111111111111";
    const TRACEPARENT: &str = "00-11111111111111111111111111111111-2222222222222222-01";

    async fn bounded<F: Future>(future: F) -> F::Output {
        timeout(BOUND, future).await.expect("fixture deadline")
    }

    fn options() -> OopServeOptions {
        OopServeOptions::new(
            "limits-gear".to_owned(),
            uuid::Uuid::from_u128(31).to_string(),
            "http://127.0.0.1:0".to_owned(),
            "127.0.0.1:0".parse().expect("listen address"),
            Arc::new(E2eDirectory::default()),
        )
    }

    fn drain() -> DrainGuard {
        DrainGuard::new(readiness(Vec::<String>::new()))
    }

    fn request(method: Method, path: &str) -> axum::http::request::Builder {
        Request::builder()
            .method(method)
            .uri(path)
            .header("traceparent", TRACEPARENT)
    }

    fn upload(bytes: Bytes, declared: bool) -> Request<Body> {
        let mut request = request(Method::POST, "/upload");
        let body = if declared {
            request = request.header(CONTENT_LENGTH, bytes.len());
            Body::from(bytes)
        } else {
            Body::from_stream(stream::iter((0..bytes.len()).step_by(2).map(
                move |start| Ok::<_, Infallible>(bytes.slice(start..(start + 2).min(bytes.len()))),
            )))
        };
        request.body(body).expect("upload request")
    }

    fn upload_route(calls: Arc<AtomicUsize>) -> Router {
        Router::new().route(
            "/upload",
            post(move |body: Bytes| {
                let calls = Arc::clone(&calls);
                async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    body
                }
            }),
        )
    }

    fn auth_only(routes: impl IntoIterator<Item = (Method, String, RouteAuth)>) -> OopRoutePolicy {
        OopRoutePolicy {
            auth: routes.into_iter().collect(),
            ..OopRoutePolicy::default()
        }
    }

    fn upload_app(calls: Arc<AtomicUsize>, options: &OopServeOptions) -> Router {
        layer_gear_router(
            upload_route(calls),
            auth_only([(Method::POST, "/upload".to_owned(), RouteAuth::Anonymous)]),
            options.tenant_plane(),
            drain(),
            options,
        )
    }

    async fn send(router: Router, request: Request<Body>) -> Response {
        bounded(router.oneshot(request))
            .await
            .expect("router response")
    }

    async fn read(response: Response) -> (StatusCode, Bytes) {
        let status = response.status();
        let bytes = bounded(to_bytes(response.into_body(), usize::MAX))
            .await
            .expect("response body");
        (status, bytes)
    }

    async fn problem(response: Response) -> (StatusCode, axum::http::HeaderValue, Value) {
        let content_type = response
            .headers()
            .get(CONTENT_TYPE)
            .cloned()
            .unwrap_or_else(|| axum::http::HeaderValue::from_static(""));
        let (status, body) = read(response).await;
        (
            status,
            content_type,
            serde_json::from_slice(&body)
                .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&body).into_owned())),
        )
    }

    #[tokio::test]
    async fn body_limits_accept_the_boundary_and_reject_the_next_byte() {
        let calls = Arc::new(AtomicUsize::new(0));
        let options = options().with_request_limits(OopRequestLimits::new(4, DEADLINE).unwrap());
        let router = upload_app(Arc::clone(&calls), &options);
        for declared in [true, false] {
            calls.store(0, Ordering::SeqCst);
            let oversize = upload(Bytes::from_static(b"abcde"), declared);
            if !declared {
                use axum::body::HttpBody as _;
                assert_eq!(
                    (
                        oversize.body().size_hint().upper(),
                        oversize.headers().get(CONTENT_LENGTH)
                    ),
                    (None, None)
                );
            }
            assert_eq!(
                problem(send(router.clone(), oversize).await).await,
                (
                    StatusCode::PAYLOAD_TOO_LARGE,
                    "application/problem+json".parse().unwrap(),
                    json!({
                        "type":"about:blank", "title":"Payload Too Large", "status":413,
                        "detail":"Payload Too Large", "instance":"/upload", "trace_id":TRACE_ID,
                        "context":{}
                    })
                )
            );
            assert_eq!(calls.load(Ordering::SeqCst), 0);
            let exact = upload(Bytes::from_static(b"abcd"), declared);
            assert_eq!(
                read(send(router.clone(), exact).await).await,
                (StatusCode::OK, Bytes::from_static(b"abcd"))
            );
            assert_eq!(calls.load(Ordering::SeqCst), 1);
        }
    }

    struct RecordingBearer(Arc<AtomicUsize>);

    impl BearerAuthenticator for RecordingBearer {
        async fn authenticate(&self, token: &str) -> Result<SecurityContext, AuthNError> {
            self.0.fetch_add(1, Ordering::SeqCst);
            AxisBearer.authenticate(token).await
        }
    }

    #[tokio::test]
    async fn the_body_layer_refuses_declared_oversize_before_auth_or_handler() {
        let auth_calls = Arc::new(AtomicUsize::new(0));
        let handler_calls = Arc::new(AtomicUsize::new(0));
        let options = options()
            .with_request_limits(OopRequestLimits::new(4, DEADLINE).unwrap())
            .with_bearer_authenticator(Some(DynBearerAuthenticator::new(RecordingBearer(
                Arc::clone(&auth_calls),
            ))));
        let router = layer_gear_router(
            upload_route(Arc::clone(&handler_calls)),
            auth_only([(Method::POST, "/upload".to_owned(), RouteAuth::Authenticated)]),
            options.tenant_plane(),
            drain(),
            &options,
        );
        let mut incoming = upload(Bytes::from_static(b"abcde"), true);
        incoming
            .headers_mut()
            .insert(AUTHORIZATION, AXIS_BEARER.parse().unwrap());
        assert_eq!(
            problem(send(router, incoming).await).await,
            (
                StatusCode::PAYLOAD_TOO_LARGE,
                "application/problem+json".parse().unwrap(),
                json!({
                    "type":"about:blank", "title":"Payload Too Large", "status":413,
                    "detail":"Payload Too Large", "instance":"/upload", "trace_id":TRACE_ID,
                    "context":{}
                })
            )
        );
        assert_eq!(
            (
                auth_calls.load(Ordering::SeqCst),
                handler_calls.load(Ordering::SeqCst)
            ),
            (0, 0)
        );
    }

    #[tokio::test]
    async fn an_opted_in_limit_replaces_the_default_extractor_limit() {
        const CAP: usize = 16 * 1024 * 1024;
        let calls = Arc::new(AtomicUsize::new(0));
        let options = options().with_request_limits(OopRequestLimits::new(CAP, DEADLINE).unwrap());
        let router = upload_app(Arc::clone(&calls), &options);
        let oversize = upload(Bytes::from(vec![b'x'; CAP + 1]), true);
        assert_eq!(
            problem(send(router.clone(), oversize).await).await,
            (
                StatusCode::PAYLOAD_TOO_LARGE,
                "application/problem+json".parse().unwrap(),
                json!({
                    "type":"about:blank", "title":"Payload Too Large", "status":413,
                    "detail":"Payload Too Large", "instance":"/upload", "trace_id":TRACE_ID,
                    "context":{}
                })
            )
        );
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        for size in [2 * 1024 * 1024 + 1, CAP] {
            let bytes = Bytes::from(vec![b'x'; size]);
            let outgoing = send(router.clone(), upload(bytes.clone(), true)).await;
            assert_eq!(read(outgoing).await, (StatusCode::OK, bytes));
        }
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[derive(Default)]
    struct HeldRequest {
        entered: Notify,
        release: Notify,
        dropped: AtomicBool,
        completed: AtomicUsize,
    }

    struct DropSignal<'a>(&'a AtomicBool);

    impl Drop for DropSignal<'_> {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    impl HeldRequest {
        async fn respond(self: Arc<Self>) -> &'static str {
            let _signal = DropSignal(&self.dropped);
            self.entered.notify_one();
            self.release.notified().await;
            self.completed.fetch_add(1, Ordering::SeqCst);
            "released"
        }

        fn route(self: &Arc<Self>) -> Router {
            let held = Arc::clone(self);
            Router::new().route("/slow", get(move || Arc::clone(&held).respond()))
        }
    }

    fn slow_request() -> Request<Body> {
        request(Method::GET, "/slow")
            .body(Body::empty())
            .expect("slow request")
    }

    fn slow_app(held: &Arc<HeldRequest>, drain: DrainGuard, options: &OopServeOptions) -> Router {
        layer_gear_router(
            held.route(),
            auth_only([(Method::GET, "/slow".to_owned(), RouteAuth::Anonymous)]),
            options.tenant_plane(),
            drain,
            options,
        )
    }

    #[tokio::test(start_paused = true)]
    async fn omitted_limits_preserve_existing_body_and_wait_behavior() {
        const DEFAULT: usize = 2 * 1024 * 1024;
        let options = options();
        assert_eq!(options.request_limits, None);
        let calls = Arc::new(AtomicUsize::new(0));
        let router = upload_app(Arc::clone(&calls), &options);
        let oversize = upload(Bytes::from(vec![b'x'; DEFAULT + 1]), true);
        assert_eq!(
            problem(send(router.clone(), oversize).await).await,
            (
                StatusCode::PAYLOAD_TOO_LARGE,
                "application/problem+json".parse().unwrap(),
                json!({
                    "type":"about:blank", "title":"Payload Too Large", "status":413,
                    "detail":"Payload Too Large", "instance":"/upload", "trace_id":TRACE_ID,
                    "context":{}
                })
            )
        );
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        let exact = Bytes::from(vec![b'x'; DEFAULT]);
        assert_eq!(
            read(send(router, upload(exact.clone(), true)).await).await,
            (StatusCode::OK, exact)
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        let held = std::array::from_fn(|_| Arc::new(HeldRequest::default()));
        let (gear, policy) = deadline_routes(&held);
        let router = layer_gear_router(gear, policy, options.tenant_plane(), drain(), &options);
        let [custom, ..] = &held;
        let incoming = request(Method::GET, "/limits/demo")
            .body(Body::empty())
            .unwrap();
        let mut pending = Box::pin(router.oneshot(incoming));
        assert!(futures_util::poll!(&mut pending).is_pending());
        bounded(custom.entered.notified()).await;
        advance(Duration::from_millis(4_001)).await;
        assert!(futures_util::poll!(&mut pending).is_pending());
        custom.release.notify_one();
        assert_eq!(
            read(bounded(pending).await.unwrap()).await,
            (StatusCode::OK, Bytes::from_static(b"released")),
        );
    }

    #[tokio::test(start_paused = true)]
    async fn request_timeout_returns_a_canonical_problem_and_releases_drain() {
        let options = options().with_request_limits(OopRequestLimits::new(4, DEADLINE).unwrap());
        let guard = drain();
        let held = Arc::new(HeldRequest::default());
        let mut pending =
            Box::pin(slow_app(&held, guard.clone(), &options).oneshot(slow_request()));
        let armed = Instant::now();
        assert!(futures_util::poll!(&mut pending).is_pending());
        bounded(held.entered.notified()).await;
        assert_eq!(
            (guard.in_flight(), held.dropped.load(Ordering::SeqCst)),
            (1, false)
        );
        advance(Duration::from_millis(1_999)).await;
        assert!(futures_util::poll!(&mut pending).is_pending());
        advance(Duration::from_millis(1)).await;
        let response = timeout(DEADLINE + BOUND, pending)
            .await
            .expect("response deadline")
            .unwrap();
        assert_eq!(Instant::now() - armed, DEADLINE);
        assert_eq!(
            problem(response).await,
            (
                StatusCode::GATEWAY_TIMEOUT,
                "application/problem+json".parse().unwrap(),
                json!({
                    "type":gts_uri!("cf.core.errors.err.v1~cf.core.err.deadline_exceeded.v1~"),
                    "title":"Deadline Exceeded", "status":504, "detail":"Request exceeded 2s timeout",
                    "instance":"/slow", "trace_id":TRACE_ID,
                    "context":{"resource_type":gts_id!("cf.core.http.request.v1~")}
                })
            )
        );
        assert_eq!(
            (
                guard.in_flight(),
                held.dropped.load(Ordering::SeqCst),
                held.completed.load(Ordering::SeqCst)
            ),
            (0, true, 0)
        );

        let held = Arc::new(HeldRequest::default());
        let mut pending =
            Box::pin(slow_app(&held, guard.clone(), &options).oneshot(slow_request()));
        assert!(futures_util::poll!(&mut pending).is_pending());
        bounded(held.entered.notified()).await;
        assert_eq!(
            (guard.in_flight(), held.dropped.load(Ordering::SeqCst)),
            (1, false)
        );
        drop(pending);
        assert_eq!(
            (
                guard.in_flight(),
                held.dropped.load(Ordering::SeqCst),
                held.completed.load(Ordering::SeqCst)
            ),
            (0, true, 0)
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_response_before_the_deadline_keeps_its_stream_alive() {
        let second_chunk = Arc::new(Notify::new());
        let router = Router::new().route(
            "/stream",
            get({
                let second_chunk = Arc::clone(&second_chunk);
                move || {
                    let second_chunk = Arc::clone(&second_chunk);
                    async move {
                        let chunks = stream::once(std::future::ready(Ok::<_, Infallible>(
                            Bytes::from_static(b"before"),
                        )))
                        .chain(stream::once(async move {
                            second_chunk.notified().await;
                            Ok(Bytes::from_static(b"after"))
                        }));
                        (
                            [(CONTENT_TYPE, "application/octet-stream")],
                            Body::from_stream(chunks),
                        )
                    }
                }
            }),
        );
        let options = options().with_request_limits(OopRequestLimits::new(4, DEADLINE).unwrap());
        let router = layer_gear_router(
            router,
            auth_only([(Method::GET, "/stream".to_owned(), RouteAuth::Anonymous)]),
            options.tenant_plane(),
            drain(),
            &options,
        );
        let response = send(
            router,
            request(Method::GET, "/stream").body(Body::empty()).unwrap(),
        )
        .await;
        assert_eq!(
            (response.status(), &response.headers()[CONTENT_TYPE]),
            (StatusCode::OK, &"application/octet-stream".parse().unwrap())
        );
        let mut chunks = response.into_body().into_data_stream();
        let first = bounded(chunks.next()).await.expect("first chunk").unwrap();
        assert_eq!(first, Bytes::from_static(b"before"));
        advance(DEADLINE + Duration::from_millis(1)).await;
        second_chunk.notify_one();
        let second = bounded(chunks.next()).await.expect("second chunk").unwrap();
        assert_eq!([first, second].concat(), b"beforeafter");
        assert!(bounded(chunks.next()).await.is_none());
    }

    async fn start_server(options: OopServeOptions) -> (OopHttpServer, CancellationToken) {
        let cancel = CancellationToken::new();
        let server = bounded(OopHttpServer::start(
            readiness(Vec::<String>::new()),
            options,
            cancel.clone(),
        ))
        .await
        .expect("fixture listener");
        (server, cancel)
    }

    fn deadline_routes(held: &[Arc<HeldRequest>; 6]) -> (Router, OopRoutePolicy) {
        use crate::api::{OpenApiRegistryImpl, OperationBuilder};
        use axum::routing::head;

        let [
            custom,
            disabled,
            inherited,
            static_route,
            explicit_head,
            external,
        ] = held.clone();
        let registry = OpenApiRegistryImpl::new();
        let router = OperationBuilder::get("/limits/{id}")
            .anonymous()
            .request_timeout(Duration::from_secs(4))
            .handler(move || Arc::clone(&custom).respond())
            .text_response(StatusCode::OK, "OK", "text/plain")
            .register(Router::new(), &registry);
        let router = OperationBuilder::post("/limits/{id}")
            .anonymous()
            .no_request_timeout()
            .handler(move || Arc::clone(&disabled).respond())
            .text_response(StatusCode::OK, "OK", "text/plain")
            .register(router, &registry);
        let router = OperationBuilder::put("/limits/{id}")
            .anonymous()
            .handler(move || Arc::clone(&inherited).respond())
            .text_response(StatusCode::OK, "OK", "text/plain")
            .register(router, &registry);
        let router = OperationBuilder::get("/limits/static")
            .anonymous()
            .request_timeout(Duration::from_secs(1))
            .handler(move || Arc::clone(&static_route).respond())
            .text_response(StatusCode::OK, "OK", "text/plain")
            .register(router, &registry);
        let router = OperationBuilder::new(Method::HEAD, "/limits/explicit")
            .anonymous()
            .request_timeout(Duration::from_secs(3))
            .method_router(head(move || Arc::clone(&explicit_head).respond()))
            .no_content_response(StatusCode::OK, "OK")
            .register(router, &registry)
            .route(
                "/limits-external",
                get(move || Arc::clone(&external).respond()),
            );
        (
            router,
            OopRoutePolicy::from_specs(registry.operation_specs.iter()),
        )
    }

    #[tokio::test(start_paused = true)]
    async fn published_routes_honor_their_declared_deadlines() {
        let options = options()
            .with_bearer_authenticator(Some(DynBearerAuthenticator::new(AxisBearer)))
            .with_request_limits(OopRequestLimits::new(4, DEADLINE).unwrap());
        let (mut server, cancel) = start_server(options).await;

        let verify = async {
            for bootstrap in [true, false] {
                let requests = std::array::from_fn(|_| Arc::new(HeldRequest::default()));
                let (gear, policy) = deadline_routes(&requests);
                let [custom, disabled, inherited, fixed, head, external] = &requests;
                let openapi = r#"{"openapi":"3.1.0"}"#.to_owned();
                if bootstrap {
                    server.publish_bootstrap_routes(gear, openapi, policy);
                } else {
                    server
                        .attach(gear, openapi, policy)
                        .expect("final publication");
                }
                let router = build_outer_router(Arc::clone(&server.readiness), server.late.clone());
                let expirations = [
                    (Method::GET, "/limits/demo", custom, 4),
                    (Method::GET, "/limits/static", fixed, 1),
                    (Method::PUT, "/limits/demo", inherited, 2),
                    (Method::HEAD, "/limits/demo", custom, 2),
                    (Method::HEAD, "/limits/explicit", head, 3),
                    (Method::GET, "/limits-external", external, 2),
                ];
                for (method, path, held, seconds) in expirations {
                    held.dropped.store(false, Ordering::SeqCst);
                    let expects_empty_body = method == Method::HEAD;
                    let mut incoming = request(method, path);
                    if path == "/limits-external" {
                        if bootstrap {
                            continue;
                        }
                        incoming = incoming.header(AUTHORIZATION, AXIS_BEARER);
                    }
                    let incoming = incoming.body(Body::empty()).unwrap();
                    let mut pending = Box::pin(router.clone().oneshot(incoming));
                    let armed = Instant::now();
                    assert!(futures_util::poll!(&mut pending).is_pending());
                    bounded(held.entered.notified()).await;
                    assert_eq!(
                        (
                            server.drain_guard.in_flight(),
                            held.dropped.load(Ordering::SeqCst)
                        ),
                        (1, false)
                    );
                    let deadline = Duration::from_secs(seconds);
                    let before_expiry = deadline.checked_sub(Duration::from_millis(1)).unwrap();
                    if seconds == 4 {
                        let past_default = DEADLINE + Duration::from_millis(1);
                        advance(past_default).await;
                        assert!(
                            futures_util::poll!(&mut pending).is_pending(),
                            "GET {path} must outlive the listener default (bootstrap={bootstrap})"
                        );
                        advance(before_expiry.checked_sub(past_default).unwrap()).await;
                    } else {
                        advance(before_expiry).await;
                    }
                    assert!(
                        futures_util::poll!(&mut pending).is_pending(),
                        "{path} expired before {seconds}s (bootstrap={bootstrap})"
                    );
                    advance(Duration::from_millis(1)).await;
                    let response = timeout(deadline + BOUND, pending)
                        .await
                        .expect("published deadline")
                        .unwrap();
                    assert_eq!(
                        Instant::now() - armed,
                        deadline,
                        "{path} (bootstrap={bootstrap})"
                    );
                    if expects_empty_body {
                        assert_eq!(
                            response.headers().get(CONTENT_TYPE).unwrap(),
                            "application/problem+json"
                        );
                        assert_eq!(
                            read(response).await,
                            (StatusCode::GATEWAY_TIMEOUT, Bytes::new())
                        );
                    } else {
                        assert_eq!(
                            problem(response).await,
                            (
                                StatusCode::GATEWAY_TIMEOUT,
                                "application/problem+json".parse().unwrap(),
                                json!({
                                    "type":gts_uri!("cf.core.errors.err.v1~cf.core.err.deadline_exceeded.v1~"),
                                    "title":"Deadline Exceeded", "status":504,
                                    "detail":format!("Request exceeded {seconds}s timeout"),
                                    "instance":path, "trace_id":TRACE_ID,
                                    "context":{"resource_type":gts_id!("cf.core.http.request.v1~")}
                                })
                            )
                        );
                    }
                    assert_eq!(
                        (
                            server.drain_guard.in_flight(),
                            held.dropped.load(Ordering::SeqCst),
                            held.completed.load(Ordering::SeqCst)
                        ),
                        (0, true, 0)
                    );
                }

                for (method, held, wait) in [
                    (Method::GET, custom, Duration::from_secs(3)),
                    (Method::POST, disabled, Duration::from_millis(4_001)),
                ] {
                    let incoming = request(method, "/limits/demo").body(Body::empty()).unwrap();
                    let mut pending = Box::pin(router.clone().oneshot(incoming));
                    assert!(futures_util::poll!(&mut pending).is_pending());
                    bounded(held.entered.notified()).await;
                    assert_eq!(server.drain_guard.in_flight(), 1);
                    advance(wait).await;
                    assert!(futures_util::poll!(&mut pending).is_pending());
                    held.release.notify_one();
                    assert_eq!(
                        read(bounded(pending).await.unwrap()).await,
                        (StatusCode::OK, Bytes::from_static(b"released"))
                    );
                    assert_eq!(
                        (
                            server.drain_guard.in_flight(),
                            held.completed.load(Ordering::SeqCst)
                        ),
                        (0, 1)
                    );
                }

                let oversize = request(Method::POST, "/limits/demo")
                    .header(CONTENT_LENGTH, 5)
                    .body(Body::from("abcde"))
                    .unwrap();
                assert_eq!(
                    problem(send(router.clone(), oversize).await).await,
                    (
                        StatusCode::PAYLOAD_TOO_LARGE,
                        "application/problem+json".parse().unwrap(),
                        json!({
                            "type":"about:blank", "title":"Payload Too Large", "status":413,
                            "detail":"Payload Too Large", "instance":"/limits/demo", "trace_id":TRACE_ID, "context":{}
                        })
                    )
                );
                assert_eq!(disabled.completed.load(Ordering::SeqCst), 1);

                for (path, bearer, final_status) in [
                    ("/missing", None, Some(StatusCode::UNAUTHORIZED)),
                    ("/missing", Some(AXIS_BEARER), Some(StatusCode::NOT_FOUND)),
                    ("/limits-external", None, Some(StatusCode::UNAUTHORIZED)),
                    ("/limits-external", Some(AXIS_BEARER), None),
                ] {
                    let bootstrap_status = bootstrap.then_some(StatusCode::SERVICE_UNAVAILABLE);
                    let Some(status) = bootstrap_status.or(final_status) else {
                        continue;
                    };
                    let mut incoming = request(Method::GET, path);
                    if let Some(bearer) = bearer {
                        incoming = incoming.header(AUTHORIZATION, bearer);
                    }
                    let response =
                        send(router.clone(), incoming.body(Body::empty()).unwrap()).await;
                    let retry_after = bootstrap.then_some(HeaderValue::from_static("1"));
                    assert_eq!(response.headers().get(RETRY_AFTER), retry_after.as_ref());
                    let body = match status {
                        StatusCode::SERVICE_UNAVAILABLE => json!({
                            "type":gts_uri!("cf.core.errors.err.v1~cf.core.err.service_unavailable.v1~"),
                            "title":"Service Unavailable", "status":503,
                            "detail":"tenant-plane routes are served once the application group has started",
                            "instance":path, "trace_id":TRACE_ID, "context":{"retry_after_seconds":1}
                        }),
                        StatusCode::UNAUTHORIZED => json!({
                            "type":gts_uri!("cf.core.errors.err.v1~cf.core.err.unauthenticated.v1~"),
                            "title":"Unauthenticated", "status":401, "detail":"Authentication required",
                            "instance":path, "trace_id":TRACE_ID, "context":{"reason":"MISSING_BEARER"}
                        }),
                        StatusCode::NOT_FOUND => json!({
                            "type":"about:blank", "title":"Not Found", "status":404, "detail":"Not Found",
                            "instance":path, "trace_id":TRACE_ID, "context":{}
                        }),
                        _ => unreachable!("unexpected rejection status in fixture"),
                    };
                    assert_eq!(
                        problem(response).await,
                        (status, "application/problem+json".parse().unwrap(), body)
                    );
                }
                assert_eq!(external.completed.load(Ordering::SeqCst), 0);
            }
        };
        let result = AssertUnwindSafe(verify).catch_unwind().await;
        tokio::time::resume();
        cancel.cancel();
        bounded(server.join()).await.expect("listener joined");
        if let Err(panic) = result {
            std::panic::resume_unwind(panic);
        }
    }

    #[tokio::test]
    async fn bootstrap_and_final_routes_both_receive_request_limits() {
        let options = options().with_request_limits(OopRequestLimits::new(4, DEADLINE).unwrap());
        let (mut server, cancel) = start_server(options).await;
        tokio::time::pause();
        let result = AssertUnwindSafe(async {
            for bootstrap in [true, false] {
                let held = Arc::new(HeldRequest::default());
                let calls = Arc::new(AtomicUsize::new(0));
                let gear = upload_route(Arc::clone(&calls)).merge(held.route());
                let policy = auth_only([
                    (Method::POST, "/upload".to_owned(), RouteAuth::Anonymous),
                    (Method::GET, "/slow".to_owned(), RouteAuth::Anonymous),
                ]);
                let openapi = r#"{"openapi":"3.1.0"}"#.to_owned();
                if bootstrap {
                    server.publish_bootstrap_routes(gear, openapi, policy);
                } else {
                    server.attach(gear, openapi, policy).expect("final publication");
                }
                let router = server.late.gear().expect("published routes");
                assert_eq!(
                    problem(send(router.clone(), upload(Bytes::from_static(b"abcde"), false)).await).await,
                    (StatusCode::PAYLOAD_TOO_LARGE, "application/problem+json".parse().unwrap(), json!({
                        "type":"about:blank", "title":"Payload Too Large", "status":413,
                        "detail":"Payload Too Large", "instance":"/upload", "trace_id":TRACE_ID,
                        "context":{}
                    })),
                    "bootstrap={bootstrap}"
                );
                assert_eq!(calls.load(Ordering::SeqCst), 0);
                let mut pending = Box::pin(router.oneshot(slow_request()));
                assert!(futures_util::poll!(&mut pending).is_pending());
                bounded(held.entered.notified()).await;
                advance(DEADLINE).await;
                let response = timeout(DEADLINE + BOUND, pending).await.expect("published deadline").unwrap();
                assert_eq!(
                    problem(response).await,
                    (StatusCode::GATEWAY_TIMEOUT, "application/problem+json".parse().unwrap(), json!({
                        "type":gts_uri!("cf.core.errors.err.v1~cf.core.err.deadline_exceeded.v1~"),
                        "title":"Deadline Exceeded", "status":504, "detail":"Request exceeded 2s timeout",
                        "instance":"/slow", "trace_id":TRACE_ID,
                        "context":{"resource_type":gts_id!("cf.core.http.request.v1~")}
                    })),
                    "bootstrap={bootstrap}"
                );
                assert_eq!((server.drain_guard.in_flight(), held.completed.load(Ordering::SeqCst)), (0, 0));
            }
        }).catch_unwind().await;
        tokio::time::resume();
        cancel.cancel();
        bounded(server.join()).await.expect("listener joined");
        if let Err(panic) = result {
            std::panic::resume_unwind(panic);
        }
    }

    #[tokio::test]
    async fn framework_probes_remain_outside_the_gear_limits() {
        let options = options().with_request_limits(OopRequestLimits::new(4, DEADLINE).unwrap());
        let (mut server, cancel) = start_server(options).await;
        let openapi =
            r#"{"openapi":"3.1.0","info":{"title":"Limits fixture","version":"1.0.0"},"paths":{}}"#;
        let calls = Arc::new(AtomicUsize::new(0));
        server
            .attach(
                upload_route(Arc::clone(&calls)),
                openapi.to_owned(),
                auth_only([(Method::POST, "/upload".to_owned(), RouteAuth::Anonymous)]),
            )
            .expect("publish routes");
        let outer = build_outer_router(Arc::clone(&server.readiness), server.late.clone());
        let result = AssertUnwindSafe(async {
            for (path, expected) in [("/healthz", "ok"), ("/.well-known/openapi.json", openapi)] {
                let probe = request(Method::GET, path)
                    .header(CONTENT_LENGTH, 5)
                    .body(Body::from("abcde"))
                    .unwrap();
                assert_eq!(
                    read(send(outer.clone(), probe).await).await,
                    (StatusCode::OK, Bytes::copy_from_slice(expected.as_bytes()))
                );
            }
            assert_eq!(
                problem(send(outer, upload(Bytes::from_static(b"abcde"), true)).await).await,
                (
                    StatusCode::PAYLOAD_TOO_LARGE,
                    "application/problem+json".parse().unwrap(),
                    json!({
                        "type":"about:blank", "title":"Payload Too Large", "status":413,
                        "detail":"Payload Too Large", "instance":"/upload", "trace_id":TRACE_ID,
                        "context":{}
                    })
                )
            );
            assert_eq!(calls.load(Ordering::SeqCst), 0);
        })
        .catch_unwind()
        .await;
        cancel.cancel();
        bounded(server.join()).await.expect("listener joined");
        if let Err(panic) = result {
            std::panic::resume_unwind(panic);
        }
    }

    #[tokio::test]
    async fn enabled_limits_preserve_auth_and_request_id_context() {
        use crate::api::{OpenApiRegistryImpl, OperationBuilder};
        use axum::Extension;

        let auth_hits = Arc::new(AtomicUsize::new(0));
        let handler_hits = Arc::clone(&auth_hits);
        let registry = OpenApiRegistryImpl::new();
        let router = OperationBuilder::get("/auth")
            .operation_id("limits.auth")
            .authenticated()
            .no_license_required()
            .handler(move |Extension(context): Extension<SecurityContext>| {
                let calls = Arc::clone(&handler_hits);
                async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    Json(json!({
                        "subject_id":context.subject_id(),
                        "request_id":toolkit_trace_context::current_request_id(),
                    }))
                }
            })
            .json_response(StatusCode::OK, "OK")
            .register(Router::new(), &registry);
        let router = OperationBuilder::post("/upload")
            .operation_id("limits.upload")
            .anonymous()
            .handler(|bytes: Bytes| async { bytes })
            .json_response(StatusCode::OK, "OK")
            .register(router, &registry);
        let options = options()
            .with_bearer_authenticator(Some(DynBearerAuthenticator::new(AxisBearer)))
            .with_request_limits(OopRequestLimits::new(4, DEADLINE).unwrap());
        let router = layer_gear_router(
            router,
            OopRoutePolicy::from_specs(registry.operation_specs.iter()),
            options.tenant_plane(),
            drain(),
            &options,
        );
        for (bearer, reason) in [
            (None, "MISSING_BEARER"),
            (Some("Bearer forged"), "AUTHN_FAILED"),
        ] {
            let mut incoming = request(Method::GET, "/auth");
            if let Some(bearer) = bearer {
                incoming = incoming.header(AUTHORIZATION, bearer);
            }
            assert_eq!(
                problem(send(router.clone(), incoming.body(Body::empty()).unwrap()).await).await,
                (
                    StatusCode::UNAUTHORIZED,
                    "application/problem+json".parse().unwrap(),
                    json!({
                        "type":gts_uri!("cf.core.errors.err.v1~cf.core.err.unauthenticated.v1~"),
                        "title":"Unauthenticated", "status":401, "detail":"Authentication required",
                        "instance":"/auth", "trace_id":TRACE_ID, "context":{"reason":reason}
                    })
                )
            );
            assert_eq!(auth_hits.load(Ordering::SeqCst), 0);
        }
        let incoming = request(Method::GET, "/auth")
            .header(AUTHORIZATION, AXIS_BEARER)
            .header("x-request-id", "limits-request")
            .body(Body::empty())
            .unwrap();
        assert_eq!(
            problem(send(router.clone(), incoming).await).await,
            (
                StatusCode::OK,
                "application/json".parse().unwrap(),
                json!({
                    "subject_id":"00000000-0000-0000-0000-000000000001", "request_id":"limits-request"
                })
            )
        );
        assert_eq!(auth_hits.load(Ordering::SeqCst), 1);
        for content_type in [None, Some("text/plain")] {
            let mut incoming = upload(Bytes::from_static(b"abcd"), true);
            if let Some(content_type) = content_type {
                incoming
                    .headers_mut()
                    .insert(CONTENT_TYPE, content_type.parse().unwrap());
            }
            assert_eq!(
                read(send(router.clone(), incoming).await).await,
                (StatusCode::OK, Bytes::from_static(b"abcd"))
            );
        }
    }
}
