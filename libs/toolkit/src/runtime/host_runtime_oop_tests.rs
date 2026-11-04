//! Full-stack end-to-end test for an `OoP` gear booted through
//! [`HostRuntime::run_oop_serving`] (`cpt-cf-component-oop-bootstrap`).
//!
//! Unlike the serve-edge test in `oop_serve_tests.rs` (which drives
//! `OopHttpServer` directly with a hand-built router), this test exercises the
//! *entire* lifecycle a real gear goes through:
//!
//! - gear discovery via a manually-built [`RegistryBuilder`],
//! - the full phase sequence (`pre_init` → `init` → proxy-wiring → `post_init`
//!   → `grpc` → `start`), matching the in-process order,
//! - [`HostRuntime::compose_oop_router`] building routes + `OpenAPI` from an
//!   actual [`RestApiCapability`] gear (not a synthetic router),
//! - a real bound HTTP server serving the gear's route,
//! - background self-registration with a live `DirectoryClient`,
//! - graceful shutdown draining and deregistration,
//! - the `stop` phase running after the server exits.

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use axum::Router;
use axum::routing::get;
use parking_lot::Mutex;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use cf_system_sdks::directory::{
    DirectoryClient, RegisterInstanceInfo, ServiceEndpoint, ServiceInstanceInfo,
};

use super::{DbOptions, HostRuntime};
use crate::client_hub::ClientHub;
use crate::config::ConfigProvider;
use crate::context::GearCtx;
use crate::contracts::{Gear, OpenApiRegistry, RestApiCapability};
use crate::registry::RegistryBuilder;
use crate::runtime::OopServeOptions;

/// A minimal gear that serves a single REST route. Implements both [`Gear`]
/// (lifecycle) and [`RestApiCapability`] (routing) so it is composed into the
/// host-less router exactly like a production gear.
#[derive(Default)]
struct PingGear;

#[async_trait]
impl Gear for PingGear {
    async fn init(&self, _ctx: &GearCtx) -> anyhow::Result<()> {
        Ok(())
    }
}

impl RestApiCapability for PingGear {
    fn register_rest(
        &self,
        _ctx: &GearCtx,
        router: Router,
        _openapi: &dyn OpenApiRegistry,
    ) -> anyhow::Result<Router> {
        Ok(router.route("/ping", get(|| async { "pong" })))
    }
}

/// In-memory `DirectoryClient` that records registration / deregistration and
/// captures the REST endpoint the gear advertised.
#[derive(Default)]
struct RecordingDirectory {
    register_calls: AtomicUsize,
    deregister_calls: AtomicUsize,
    registered_endpoint: Mutex<Option<String>>,
    registered_gears: Mutex<Vec<String>>,
    /// The `OpenAPI` document of each registration, in order.
    registered_openapi: Mutex<Vec<Option<String>>>,
}

#[async_trait]
impl DirectoryClient for RecordingDirectory {
    async fn resolve_grpc_service(&self, _service: &str) -> anyhow::Result<ServiceEndpoint> {
        Ok(ServiceEndpoint::new("http://grpc"))
    }
    async fn resolve_rest_service(&self, gear: &str) -> anyhow::Result<ServiceEndpoint> {
        Ok(ServiceEndpoint::new(format!("http://{gear}:8080")))
    }
    async fn get_openapi_spec(&self, _gear: &str) -> anyhow::Result<String> {
        Ok(String::new())
    }
    async fn list_instances(&self, _gear: &str) -> anyhow::Result<Vec<ServiceInstanceInfo>> {
        Ok(vec![])
    }
    async fn list_all_instances(&self) -> anyhow::Result<Vec<ServiceInstanceInfo>> {
        Ok(vec![])
    }
    async fn register_instance(&self, info: RegisterInstanceInfo) -> anyhow::Result<()> {
        if let Some(ep) = &info.rest_endpoint {
            *self.registered_endpoint.lock() = Some(ep.uri.clone());
        }
        self.registered_openapi
            .lock()
            .push(info.openapi_spec.clone());
        self.registered_gears.lock().push(info.gear);
        self.register_calls.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
    async fn deregister_instance(&self, _gear: &str, _instance: &str) -> anyhow::Result<()> {
        self.deregister_calls.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
    async fn send_heartbeat(&self, _gear: &str, _instance: &str) -> anyhow::Result<()> {
        Ok(())
    }
}

struct EmptyConfigProvider;
impl ConfigProvider for EmptyConfigProvider {
    fn get_gear_config(&self, _gear_name: &str) -> Option<&serde_json::Value> {
        None
    }
}

/// Reserve an ephemeral port and return it (closing the listener so the server
/// can rebind it).
fn free_addr() -> SocketAddr {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port");
    listener.local_addr().expect("local_addr")
}

/// Minimal raw HTTP/1.1 GET returning `(status, body)`. Uses `Connection: close`
/// so the read completes on EOF.
async fn http_get(addr: SocketAddr, path: &str) -> Option<(u16, String)> {
    let mut stream = tokio::net::TcpStream::connect(addr).await.ok()?;
    let req = format!("GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n");
    stream.write_all(req.as_bytes()).await.ok()?;
    let mut buf = Vec::new();
    stream.read_to_end(&mut buf).await.ok()?;
    let text = String::from_utf8_lossy(&buf);
    let status = text
        .lines()
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|s| s.parse().ok())?;
    let body = text
        .split_once("\r\n\r\n")
        .map_or(String::new(), |(_, b)| b.to_owned());
    Some((status, body))
}

/// Poll `path` until it returns `want` or the deadline elapses.
async fn poll_status(addr: SocketAddr, path: &str, want: u16, timeout: Duration) -> bool {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if let Some((status, _)) = http_get(addr, path).await
            && status == want
        {
            return true;
        }
        if tokio::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

#[tokio::test]
async fn e2e_oop_gear_boots_serves_registers_and_shuts_down() {
    let addr = free_addr();
    let directory = Arc::new(RecordingDirectory::default());

    // 1. Build a registry with a single REST-capable gear, exactly as discovery
    //    would produce it.
    let gear = Arc::new(PingGear);
    let mut builder = RegistryBuilder::default();
    builder.register_core_with_meta("ping-gear", &[], gear.clone() as Arc<dyn Gear>);
    builder.register_rest_with_meta("ping-gear", gear as Arc<dyn RestApiCapability>);
    let registry = builder.build_topo_sorted().unwrap();

    let hub = Arc::new(ClientHub::new());
    let cancel = CancellationToken::new();
    let config: Arc<dyn ConfigProvider> = Arc::new(EmptyConfigProvider);

    let host = HostRuntime::new(
        registry,
        config,
        DbOptions::None,
        hub,
        cancel.clone(),
        Uuid::new_v4(),
        None,
    );

    let options = OopServeOptions {
        gear_name: "ping-gear".to_owned(),
        instance_id: "i-1".to_owned(),
        version: Some("1.2.3".to_owned()),
        advertise_uri: format!("http://{addr}"),
        listen_addr: addr,
        probe_bind_addr: None,
        drain_timeout: Duration::from_secs(5),
        heartbeat_interval: Duration::from_secs(30),
        healthcheck_timeout: Duration::from_millis(500),
        directory: Arc::clone(&directory) as Arc<dyn DirectoryClient>,
        bearer_authenticator: None,
        internal_authenticator: None,
        // No authenticator: the raw `/ping` route runs only in development mode.
        auth_disabled: true,
        labels: std::collections::BTreeMap::new(),
    };

    // 2. Drive the full OoP serving lifecycle on a background task.
    let server = tokio::spawn(host.run_oop_serving(options));

    // 3. Liveness comes up once phases complete and the server binds.
    assert!(
        poll_status(addr, "/healthz", 200, Duration::from_secs(5)).await,
        "/healthz should return 200 once the gear has booted"
    );

    // 4. The gear's OWN route is served — proves compose_oop_router wired a real
    //    RestApiCapability gear into the host-less router.
    let ping = http_get(addr, "/ping").await;
    assert_eq!(
        ping,
        Some((200, "pong".to_owned())),
        "the gear's REST route should serve its response body"
    );

    // 5. No external dependencies → /readyz becomes 200.
    assert!(
        poll_status(addr, "/readyz", 200, Duration::from_secs(3)).await,
        "/readyz should be 200 when the gear has no unresolved deps"
    );

    // 6. The gear's OpenAPI is published at the well-known path.
    let (oas_status, oas_body) = http_get(addr, "/.well-known/openapi.json")
        .await
        .expect("openapi endpoint should respond");
    assert_eq!(oas_status, 200);
    assert!(
        oas_body.contains("ping-gear") && oas_body.contains("1.2.3"),
        "OpenAPI document should carry the gear title + version, got: {oas_body}"
    );

    // 7. Self-registration ran through the full stack with the advertised URI.
    let registered = {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
        loop {
            if directory.register_calls.load(Ordering::SeqCst) >= 1 {
                break true;
            }
            if tokio::time::Instant::now() >= deadline {
                break false;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    };
    assert!(
        registered,
        "the gear should self-register with the directory"
    );
    let registered_endpoint_seen = { directory.registered_endpoint.lock().clone() };
    assert_eq!(
        registered_endpoint_seen,
        Some(format!("http://{addr}")),
        "the advertised REST endpoint should be registered"
    );

    // 8. Graceful shutdown: cancel, the server drains + deregisters, the stop
    //    phase runs, and the lifecycle returns Ok.
    cancel.cancel();
    let result = tokio::time::timeout(Duration::from_secs(5), server)
        .await
        .expect("run_oop_serving should finish promptly after cancel")
        .expect("run_oop_serving task should not panic");
    assert!(
        result.is_ok(),
        "OoP serving lifecycle should end cleanly: {result:?}"
    );

    assert_eq!(
        directory.deregister_calls.load(Ordering::SeqCst),
        1,
        "the gear should deregister exactly once on graceful shutdown"
    );
}

const AXIS_SA: &str = "good-sa-token";

/// Rejects every bearer so tenant credentials cannot admit these routes.
struct RejectingBearer;

impl toolkit_security::BearerAuthenticator for RejectingBearer {
    async fn authenticate(
        &self,
        _token: &str,
    ) -> Result<toolkit_security::SecurityContext, toolkit_security::AuthNError> {
        Err(toolkit_security::AuthNError::InvalidToken)
    }
}

/// Platform authenticator: `good-sa-token` is a workload, the rest rejected.
struct AxisInternal;

impl toolkit_security::InternalAuthenticator for AxisInternal {
    async fn authenticate(
        &self,
        token: &str,
    ) -> Result<toolkit_security::PlatformIdentity, toolkit_security::InternalAuthNError> {
        if token == AXIS_SA {
            Ok(
                toolkit_security::PlatformIdentity::KubernetesServiceAccount {
                    namespace: "toolkit".to_owned(),
                    service_account: "axis".to_owned(),
                    pod: None,
                },
            )
        } else {
            Err(toolkit_security::InternalAuthNError::InvalidToken)
        }
    }
}

/// Uses `OperationBuilder` specs to test `compose_oop_router` auth derivation.
#[derive(Default)]
struct AxisGear;

#[async_trait]
impl Gear for AxisGear {
    async fn init(&self, _ctx: &GearCtx) -> anyhow::Result<()> {
        Ok(())
    }
}

impl RestApiCapability for AxisGear {
    fn register_rest(
        &self,
        _ctx: &GearCtx,
        router: Router,
        openapi: &dyn OpenApiRegistry,
    ) -> anyhow::Result<Router> {
        use crate::api::OperationBuilder;
        let router = OperationBuilder::get("/anon")
            .operation_id("axis.anon")
            .anonymous()
            .handler(|| async { "anon" })
            .text_response(http::StatusCode::OK, "OK", "text/plain")
            .register(router, openapi);
        Ok(OperationBuilder::get("/platform")
            .operation_id("axis.platform")
            .platform_authenticated()
            .no_license_required()
            .handler(|| async { "platform" })
            .text_response(http::StatusCode::OK, "OK", "text/plain")
            .register(router, openapi))
    }
}

/// A raw HTTP/1.1 GET with extra headers, returning the status.
async fn http_status(addr: SocketAddr, path: &str, headers: &[(&str, &str)]) -> Option<u16> {
    let mut stream = tokio::net::TcpStream::connect(addr).await.ok()?;
    let mut req = format!("GET {path} HTTP/1.1\r\nHost: localhost\r\n");
    for (name, value) in headers {
        req.push_str(name);
        req.push_str(": ");
        req.push_str(value);
        req.push_str("\r\n");
    }
    req.push_str("Connection: close\r\n\r\n");
    stream.write_all(req.as_bytes()).await.ok()?;
    let mut buf = Vec::new();
    stream.read_to_end(&mut buf).await.ok()?;
    String::from_utf8_lossy(&buf)
        .lines()
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|s| s.parse().ok())
}

#[tokio::test]
async fn the_composed_router_enforces_each_routes_declared_auth() {
    let addr = free_addr();
    let directory = Arc::new(RecordingDirectory::default());
    let gear = Arc::new(AxisGear);
    let mut builder = RegistryBuilder::default();
    builder.register_core_with_meta("axis-gear", &[], gear.clone() as Arc<dyn Gear>);
    builder.register_rest_with_meta("axis-gear", gear as Arc<dyn RestApiCapability>);
    let registry = builder.build_topo_sorted().unwrap();
    let cancel = CancellationToken::new();
    let host = HostRuntime::new(
        registry,
        Arc::new(EmptyConfigProvider) as Arc<dyn ConfigProvider>,
        DbOptions::None,
        Arc::new(ClientHub::new()),
        cancel.clone(),
        Uuid::new_v4(),
        None,
    );
    let options = OopServeOptions {
        gear_name: "axis-gear".to_owned(),
        instance_id: "i-1".to_owned(),
        version: None,
        advertise_uri: format!("http://{addr}"),
        listen_addr: addr,
        probe_bind_addr: None,
        drain_timeout: Duration::from_secs(5),
        heartbeat_interval: Duration::from_secs(30),
        healthcheck_timeout: Duration::from_millis(500),
        directory: Arc::clone(&directory) as Arc<dyn DirectoryClient>,
        bearer_authenticator: Some(toolkit_security::DynBearerAuthenticator::new(
            RejectingBearer,
        )),
        internal_authenticator: Some(toolkit_security::DynInternalAuthenticator::new(
            AxisInternal,
        )),
        auth_disabled: false,
        labels: std::collections::BTreeMap::new(),
    };
    let server = tokio::spawn(host.run_oop_serving(options));
    assert!(
        poll_status(addr, "/healthz", 200, Duration::from_secs(5)).await,
        "the gear boots"
    );

    // Without the derived policy every route would demand a bearer.
    assert_eq!(
        http_status(addr, "/anon", &[]).await,
        Some(200),
        "an anonymous route needs no credential"
    );
    assert_eq!(
        http_status(addr, "/platform", &[("X-ToolKit-Internal-Token", AXIS_SA)]).await,
        Some(200),
        "a platform route admits a validated internal token"
    );
    assert_eq!(
        http_status(addr, "/platform", &[]).await,
        Some(401),
        "and refuses a caller without one"
    );

    cancel.cancel();
    let result = tokio::time::timeout(Duration::from_secs(5), server)
        .await
        .expect("finishes after cancel")
        .expect("does not panic");
    assert!(result.is_ok(), "{result:?}");
}

/// Serves one tenant operation, so it needs a tenant authenticator to start.
struct TenantGear;

#[async_trait]
impl Gear for TenantGear {
    async fn init(&self, _ctx: &GearCtx) -> anyhow::Result<()> {
        Ok(())
    }
}

impl RestApiCapability for TenantGear {
    fn register_rest(
        &self,
        _ctx: &GearCtx,
        router: Router,
        openapi: &dyn OpenApiRegistry,
    ) -> anyhow::Result<Router> {
        use crate::api::OperationBuilder;
        Ok(OperationBuilder::get("/tenant")
            .operation_id("tenant.get")
            .authenticated()
            .no_license_required()
            .handler(|| async { "tenant" })
            .text_response(http::StatusCode::OK, "OK", "text/plain")
            .register(router, openapi))
    }
}

#[tokio::test]
async fn a_worker_without_a_tenant_authenticator_fails_before_it_is_advertised() {
    let addr = free_addr();
    let directory = Arc::new(RecordingDirectory::default());
    let gear = Arc::new(TenantGear);
    let mut builder = RegistryBuilder::default();
    builder.register_core_with_meta("tenant-gear", &[], Arc::clone(&gear) as Arc<dyn Gear>);
    builder.register_rest_with_meta("tenant-gear", gear as Arc<dyn RestApiCapability>);
    let registry = builder.build_topo_sorted().unwrap();
    let host = HostRuntime::new(
        registry,
        Arc::new(EmptyConfigProvider) as Arc<dyn ConfigProvider>,
        DbOptions::None,
        Arc::new(ClientHub::new()),
        CancellationToken::new(),
        Uuid::new_v4(),
        None,
    );
    let options = OopServeOptions {
        gear_name: "tenant-gear".to_owned(),
        instance_id: "i-1".to_owned(),
        version: None,
        advertise_uri: format!("http://{addr}"),
        listen_addr: addr,
        probe_bind_addr: None,
        drain_timeout: Duration::from_secs(5),
        heartbeat_interval: Duration::from_secs(30),
        healthcheck_timeout: Duration::from_millis(500),
        directory: Arc::clone(&directory) as Arc<dyn DirectoryClient>,
        bearer_authenticator: None,
        internal_authenticator: None,
        auth_disabled: false,
        labels: std::collections::BTreeMap::new(),
    };

    let result = tokio::time::timeout(Duration::from_secs(5), host.run_oop_serving(options))
        .await
        .expect("startup fails promptly");
    let error = result
        .expect_err("a tenant route without an authenticator")
        .to_string();
    assert!(error.contains("tenant authenticator"), "{error}");
    assert_eq!(
        directory.register_calls.load(Ordering::SeqCst),
        0,
        "the gear is never advertised"
    );
    let served = tokio::time::timeout(Duration::from_secs(5), http_get(addr, "/tenant"))
        .await
        .expect("the probe finishes promptly");
    assert_eq!(
        served, None,
        "the listener is torn down without serving the route"
    );
}

/// Accepts the bearer `tenant-token` as a tenant subject, rejects the rest.
struct TenantBearer;

impl toolkit_security::BearerAuthenticator for TenantBearer {
    async fn authenticate(
        &self,
        token: &str,
    ) -> Result<toolkit_security::SecurityContext, toolkit_security::AuthNError> {
        if token == "tenant-token" {
            toolkit_security::SecurityContext::builder()
                .subject_id(Uuid::from_u128(1))
                .subject_tenant_id(Uuid::from_u128(2))
                .build()
                .map_err(|_| toolkit_security::AuthNError::InvalidToken)
        } else {
            Err(toolkit_security::AuthNError::InvalidToken)
        }
    }
}

/// The startup check sees the tenant authenticator a linked gear registers in `ClientHub`
/// (as `authn-resolver` does): the worker starts and checks each tenant request.
#[tokio::test]
async fn a_worker_with_a_hub_tenant_authenticator_starts_and_checks_bearers() {
    let addr = free_addr();
    let directory = Arc::new(RecordingDirectory::default());
    let gear = Arc::new(TenantGear);
    let mut builder = RegistryBuilder::default();
    builder.register_core_with_meta("tenant-gear", &[], Arc::clone(&gear) as Arc<dyn Gear>);
    builder.register_rest_with_meta("tenant-gear", gear as Arc<dyn RestApiCapability>);
    let registry = builder.build_topo_sorted().unwrap();
    let hub = Arc::new(ClientHub::new());
    hub.register::<toolkit_security::DynBearerAuthenticator>(Arc::new(
        toolkit_security::DynBearerAuthenticator::new(TenantBearer),
    ));
    let cancel = CancellationToken::new();
    let host = HostRuntime::new(
        registry,
        Arc::new(EmptyConfigProvider) as Arc<dyn ConfigProvider>,
        DbOptions::None,
        hub,
        cancel.clone(),
        Uuid::new_v4(),
        None,
    );
    let options = OopServeOptions {
        gear_name: "tenant-gear".to_owned(),
        instance_id: "i-1".to_owned(),
        version: None,
        advertise_uri: format!("http://{addr}"),
        listen_addr: addr,
        probe_bind_addr: None,
        drain_timeout: Duration::from_secs(5),
        heartbeat_interval: Duration::from_secs(30),
        healthcheck_timeout: Duration::from_millis(500),
        directory: Arc::clone(&directory) as Arc<dyn DirectoryClient>,
        bearer_authenticator: None,
        internal_authenticator: None,
        auth_disabled: false,
        labels: std::collections::BTreeMap::new(),
    };
    let server = tokio::spawn(host.run_oop_serving(options));

    // Gear routes answer 503 until attached; poll until the tenant route serves.
    let bearer = [("Authorization", "Bearer tenant-token")];
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    let mut status = None;
    while tokio::time::Instant::now() < deadline {
        status = http_status(addr, "/tenant", &bearer).await;
        if status == Some(200) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert_eq!(status, Some(200), "a valid bearer is admitted");
    assert_eq!(
        http_status(addr, "/tenant", &[]).await,
        Some(401),
        "a missing bearer is refused"
    );

    cancel.cancel();
    let result = tokio::time::timeout(Duration::from_secs(5), server)
        .await
        .expect("finishes after cancel")
        .expect("does not panic");
    assert!(result.is_ok(), "{result:?}");
}

/// Holds a publication until notified: the registry is absent until then.
/// `dropped` is set when a held publication future is dropped unfinished.
#[derive(Default)]
struct HeldRegistry {
    release: tokio::sync::Notify,
    dropped: Arc<std::sync::atomic::AtomicBool>,
    /// Refuses every publication at once instead.
    refuse: bool,
}

/// Sets its flag when dropped.
struct DropFlag(Arc<std::sync::atomic::AtomicBool>);

impl Drop for DropFlag {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

fn held_backend(hub: &ClientHub) -> anyhow::Result<crate::gts_publication::GtsPublicationBackend> {
    let held = hub.get::<HeldRegistry>()?;
    Ok(crate::gts_publication::GtsPublicationBackend::new(
        move |_| {
            let held = Arc::clone(&held);
            async move {
                anyhow::ensure!(!held.refuse, "rejected: incompatible");
                let flag = DropFlag(Arc::clone(&held.dropped));
                held.release.notified().await;
                std::mem::forget(flag);
                Ok(())
            }
        },
    ))
}

fn publishing_ping_documents() -> Vec<crate::gts_publication::DeclarationSource> {
    vec![crate::gts_publication::DeclarationSource::new(
        "test",
        vec![(
            "gts.cf.test.publication.ping.v1~".to_owned(),
            "{}".to_owned(),
        )],
    )]
}

static PUBLISHING_PING: crate::gts_publication::GtsPublicationDescriptor =
    crate::gts_publication::GtsPublicationDescriptor {
        gear: "publishing-ping",
        publisher_version: "1.0.0",
        declarations: publishing_ping_documents,
        entities: None,
        backend: held_backend,
        provider: "held-registry",
        wire: no_wire,
    };

/// An `OoP` root gear that publishes: its route counts handler calls.
#[derive(Default)]
struct PublishingPing {
    handled: AtomicUsize,
}

#[async_trait]
impl Gear for PublishingPing {
    async fn init(&self, _ctx: &GearCtx) -> anyhow::Result<()> {
        Ok(())
    }
}

impl crate::gts_publication::GtsPublicationCapability for PublishingPing {
    fn descriptor(&self) -> &'static crate::gts_publication::GtsPublicationDescriptor {
        &PUBLISHING_PING
    }
}

impl RestApiCapability for PublishingPing {
    fn register_rest(
        &self,
        ctx: &GearCtx,
        router: Router,
        _openapi: &dyn OpenApiRegistry,
    ) -> anyhow::Result<Router> {
        let gear = ctx
            .client_hub()
            .get::<PublishingPing>()
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        Ok(router.route(
            "/published",
            get(move || async move {
                gear.handled.fetch_add(1, Ordering::SeqCst);
                "pong"
            }),
        ))
    }
}

/// Before admission an `OoP` publishing root is live (`/healthz`), not ready,
/// not initialized, its handler is not reached, and the directory has no
/// presence for it. After admission it serves and registers.
#[tokio::test]
async fn an_oop_publishing_gear_is_neither_served_nor_advertised_before_admission() {
    let addr = free_addr();
    let directory = Arc::new(RecordingDirectory::default());
    let gear = Arc::new(PublishingPing::default());
    let mut builder = RegistryBuilder::default();
    builder.register_core_with_meta("publishing-ping", &[], gear.clone() as Arc<dyn Gear>);
    builder.register_rest_with_meta(
        "publishing-ping",
        gear.clone() as Arc<dyn RestApiCapability>,
    );
    builder.register_gts_publication_with_meta(
        "publishing-ping",
        gear.clone() as Arc<dyn crate::gts_publication::GtsPublicationCapability>,
    );
    let hub = Arc::new(ClientHub::new());
    hub.register::<PublishingPing>(Arc::clone(&gear));
    let held = Arc::new(HeldRegistry::default());
    hub.register::<HeldRegistry>(Arc::clone(&held));
    let cancel = CancellationToken::new();
    let host = HostRuntime::new(
        builder.build_topo_sorted().unwrap(),
        Arc::new(EmptyConfigProvider) as Arc<dyn ConfigProvider>,
        DbOptions::None,
        hub,
        cancel.clone(),
        Uuid::new_v4(),
        None,
    );
    let options = OopServeOptions {
        gear_name: "publishing-ping".to_owned(),
        instance_id: "i-1".to_owned(),
        version: None,
        advertise_uri: format!("http://{addr}"),
        listen_addr: addr,
        probe_bind_addr: None,
        drain_timeout: Duration::from_secs(5),
        heartbeat_interval: Duration::from_secs(30),
        healthcheck_timeout: Duration::from_millis(500),
        directory: Arc::clone(&directory) as Arc<dyn DirectoryClient>,
        bearer_authenticator: None,
        internal_authenticator: None,
        auth_disabled: true,
        labels: std::collections::BTreeMap::new(),
    };
    let server = tokio::spawn(host.run_oop_serving(options));

    assert!(poll_status(addr, "/healthz", 200, Duration::from_secs(5)).await);
    // Routes not attached yet: 503 from the toolkit, not the handler.
    assert!(poll_status(addr, "/published", 503, Duration::from_secs(5)).await);
    assert_eq!(http_get(addr, "/readyz").await.map(|r| r.0), Some(503));
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(gear.handled.load(Ordering::SeqCst), 0);
    assert_eq!(
        directory.register_calls.load(Ordering::SeqCst),
        0,
        "no presence yet"
    );

    held.release.notify_one();
    assert!(poll_status(addr, "/published", 200, Duration::from_secs(5)).await);
    assert!(gear.handled.load(Ordering::SeqCst) >= 1);
    assert!(poll_status(addr, "/readyz", 200, Duration::from_secs(5)).await);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    while directory.register_calls.load(Ordering::SeqCst) == 0 {
        assert!(
            tokio::time::Instant::now() < deadline,
            "presence after admission"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    cancel.cancel();
    tokio::time::timeout(Duration::from_secs(5), server)
        .await
        .expect("ends after cancel")
        .expect("no panic")
        .expect("clean shutdown");
}

/// Puts a gear in the system group; wires no runtime internals.
struct SystemMarker;
impl crate::contracts::SystemCapability for SystemMarker {}

/// An application gear with a documented route.
struct DocumentedApp;

#[async_trait]
impl Gear for DocumentedApp {
    async fn init(&self, _ctx: &GearCtx) -> anyhow::Result<()> {
        Ok(())
    }
}

impl RestApiCapability for DocumentedApp {
    fn register_rest(
        &self,
        _ctx: &GearCtx,
        router: Router,
        openapi: &dyn OpenApiRegistry,
    ) -> anyhow::Result<Router> {
        use crate::api::OperationBuilder;
        Ok(OperationBuilder::get("/app-doc")
            .operation_id("app.doc")
            .anonymous()
            .handler(|| async { "doc" })
            .text_response(http::StatusCode::OK, "OK", "text/plain")
            .register(router, openapi))
    }
}

/// A system root serves its platform-plane and anonymous routes and
/// is present in the directory while its co-hosted application group waits
/// for admission: other processes must still discover it (a registry, say)
/// to publish what the group waits for. The group's routes are not served
/// before; afterwards they are, on the same listener.
#[tokio::test]
async fn a_system_root_serves_and_is_present_while_the_application_group_waits() {
    let addr = free_addr();
    let directory = Arc::new(RecordingDirectory::default());
    let root = Arc::new(AxisGear);
    let side = Arc::new(PublishingPing::default());
    let mut builder = RegistryBuilder::default();
    builder.register_core_with_meta("axis-gear", &[], root.clone() as Arc<dyn Gear>);
    builder.register_rest_with_meta("axis-gear", root as Arc<dyn RestApiCapability>);
    builder.register_system_with_meta(
        "axis-gear",
        Arc::new(SystemMarker) as Arc<dyn crate::contracts::SystemCapability>,
    );
    builder.register_core_with_meta("publishing-ping", &[], side.clone() as Arc<dyn Gear>);
    builder.register_rest_with_meta(
        "publishing-ping",
        side.clone() as Arc<dyn RestApiCapability>,
    );
    builder.register_gts_publication_with_meta(
        "publishing-ping",
        side.clone() as Arc<dyn crate::gts_publication::GtsPublicationCapability>,
    );
    builder.register_core_with_meta(
        "documented-app",
        &[],
        Arc::new(DocumentedApp) as Arc<dyn Gear>,
    );
    builder.register_rest_with_meta(
        "documented-app",
        Arc::new(DocumentedApp) as Arc<dyn RestApiCapability>,
    );
    let hub = Arc::new(ClientHub::new());
    hub.register::<PublishingPing>(Arc::clone(&side));
    let held = Arc::new(HeldRegistry::default());
    hub.register::<HeldRegistry>(Arc::clone(&held));
    let cancel = CancellationToken::new();
    let host = HostRuntime::new(
        builder.build_topo_sorted().unwrap(),
        Arc::new(EmptyConfigProvider) as Arc<dyn ConfigProvider>,
        DbOptions::None,
        hub,
        cancel.clone(),
        Uuid::new_v4(),
        None,
    );
    let options = OopServeOptions {
        gear_name: "axis-gear".to_owned(),
        instance_id: "i-1".to_owned(),
        version: None,
        advertise_uri: format!("http://{addr}"),
        listen_addr: addr,
        probe_bind_addr: None,
        drain_timeout: Duration::from_secs(5),
        heartbeat_interval: Duration::from_secs(30),
        healthcheck_timeout: Duration::from_millis(500),
        directory: Arc::clone(&directory) as Arc<dyn DirectoryClient>,
        bearer_authenticator: None,
        internal_authenticator: Some(toolkit_security::DynInternalAuthenticator::new(
            AxisInternal,
        )),
        auth_disabled: true,
        labels: std::collections::BTreeMap::new(),
    };
    let server = tokio::spawn(host.run_oop_serving(options));

    assert!(poll_status(addr, "/anon", 200, Duration::from_secs(5)).await);
    assert_eq!(
        http_status(addr, "/platform", &[("X-ToolKit-Internal-Token", AXIS_SA)]).await,
        Some(200),
        "the platform plane is served before the application group"
    );
    assert_eq!(
        http_status(addr, "/platform", &[]).await,
        Some(401),
        "and still refuses a caller without a credential"
    );
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    while directory.register_calls.load(Ordering::SeqCst) == 0 {
        assert!(
            tokio::time::Instant::now() < deadline,
            "root presence while the application group waits"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(http_get(addr, "/published").await.map(|r| r.0), Some(503));
    assert_eq!(side.handled.load(Ordering::SeqCst), 0);
    // The process-wide readiness still reports the pending group.
    assert_eq!(http_get(addr, "/readyz").await.map(|r| r.0), Some(503));

    held.release.notify_one();
    assert!(poll_status(addr, "/published", 200, Duration::from_secs(5)).await);
    assert_eq!(
        http_status(addr, "/platform", &[("X-ToolKit-Internal-Token", AXIS_SA)]).await,
        Some(200),
        "the same listener keeps the platform routes"
    );
    assert!(poll_status(addr, "/readyz", 200, Duration::from_secs(5)).await);
    let first = {
        directory
            .registered_openapi
            .lock()
            .first()
            .cloned()
            .flatten()
    };
    assert!(
        first.is_some_and(|spec| spec.contains("/anon") && !spec.contains("/app-doc")),
        "registered first with the bootstrap document"
    );
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let last = {
            directory
                .registered_openapi
                .lock()
                .last()
                .cloned()
                .flatten()
        };
        if last.is_some_and(|spec| spec.contains("/anon") && spec.contains("/app-doc")) {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the directory gets the full document once the application group attached"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    cancel.cancel();
    tokio::time::timeout(Duration::from_secs(5), server)
        .await
        .expect("ends after cancel")
        .expect("no panic")
        .expect("clean shutdown");
}

/// A refused publication leaves the system root serving until shutdown, then
/// the run reports the application group that never started.
#[tokio::test]
#[tracing_test::traced_test]
async fn a_failed_application_group_is_returned_after_the_system_root_served() {
    let addr = free_addr();
    let directory = Arc::new(RecordingDirectory::default());
    let root = Arc::new(AxisGear);
    let side = Arc::new(PublishingPing::default());
    let mut builder = RegistryBuilder::default();
    builder.register_core_with_meta("axis-gear", &[], root.clone() as Arc<dyn Gear>);
    builder.register_rest_with_meta("axis-gear", root as Arc<dyn RestApiCapability>);
    builder.register_system_with_meta(
        "axis-gear",
        Arc::new(SystemMarker) as Arc<dyn crate::contracts::SystemCapability>,
    );
    builder.register_core_with_meta("publishing-ping", &[], side.clone() as Arc<dyn Gear>);
    builder.register_rest_with_meta(
        "publishing-ping",
        side.clone() as Arc<dyn RestApiCapability>,
    );
    builder.register_gts_publication_with_meta(
        "publishing-ping",
        side.clone() as Arc<dyn crate::gts_publication::GtsPublicationCapability>,
    );
    let hub = Arc::new(ClientHub::new());
    hub.register::<PublishingPing>(Arc::clone(&side));
    hub.register::<HeldRegistry>(Arc::new(HeldRegistry {
        refuse: true,
        ..HeldRegistry::default()
    }));
    let cancel = CancellationToken::new();
    let host = HostRuntime::new(
        builder.build_topo_sorted().unwrap(),
        Arc::new(EmptyConfigProvider) as Arc<dyn ConfigProvider>,
        DbOptions::None,
        hub,
        cancel.clone(),
        Uuid::new_v4(),
        None,
    );
    let server = tokio::spawn(tracing::Instrument::in_current_span(
        host.run_oop_serving(oop_options("axis-gear", addr, &directory, None)),
    ));

    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while !logs_contain("the application group did not start") {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the failure is reported"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(
        http_get(addr, "/anon").await.map(|r| r.0),
        Some(200),
        "the system root keeps serving"
    );
    assert_eq!(
        http_get(addr, "/published").await.map(|r| r.0),
        Some(503),
        "the application routes never attach"
    );
    assert!(!server.is_finished(), "until shutdown");

    cancel.cancel();
    let err = tokio::time::timeout(Duration::from_secs(5), server)
        .await
        .expect("ends after cancel")
        .expect("no panic")
        .unwrap_err();
    let message = format!("{err:#}");
    assert!(
        message.contains("the application group did not start") && message.contains("incompatible"),
        "{message}"
    );
    assert_eq!(side.handled.load(Ordering::SeqCst), 0);
}

/// Dropping the `OoP` lifecycle future while the root's publication is pending
/// ends the publication and closes the listener: nothing outlives the runtime,
/// without any cancellation from outside.
#[tokio::test]
async fn dropping_the_oop_runtime_ends_a_pending_publication_and_the_listener() {
    let addr = free_addr();
    let gear = Arc::new(PublishingPing::default());
    let mut builder = RegistryBuilder::default();
    builder.register_core_with_meta("publishing-ping", &[], gear.clone() as Arc<dyn Gear>);
    builder.register_rest_with_meta(
        "publishing-ping",
        gear.clone() as Arc<dyn RestApiCapability>,
    );
    builder.register_gts_publication_with_meta(
        "publishing-ping",
        gear.clone() as Arc<dyn crate::gts_publication::GtsPublicationCapability>,
    );
    let hub = Arc::new(ClientHub::new());
    hub.register::<PublishingPing>(Arc::clone(&gear));
    let held = Arc::new(HeldRegistry::default());
    hub.register::<HeldRegistry>(Arc::clone(&held));
    let host = HostRuntime::new(
        builder.build_topo_sorted().unwrap(),
        Arc::new(EmptyConfigProvider) as Arc<dyn ConfigProvider>,
        DbOptions::None,
        hub,
        CancellationToken::new(),
        Uuid::new_v4(),
        None,
    );
    let options = OopServeOptions {
        gear_name: "publishing-ping".to_owned(),
        instance_id: "i-1".to_owned(),
        version: None,
        advertise_uri: format!("http://{addr}"),
        listen_addr: addr,
        probe_bind_addr: None,
        drain_timeout: Duration::from_secs(5),
        heartbeat_interval: Duration::from_secs(30),
        healthcheck_timeout: Duration::from_millis(500),
        directory: Arc::new(RecordingDirectory::default()) as Arc<dyn DirectoryClient>,
        bearer_authenticator: None,
        internal_authenticator: None,
        auth_disabled: true,
        labels: std::collections::BTreeMap::new(),
    };
    let server = tokio::spawn(host.run_oop_serving(options));
    assert!(poll_status(addr, "/healthz", 200, Duration::from_secs(5)).await);
    assert!(poll_status(addr, "/published", 503, Duration::from_secs(5)).await);
    assert!(
        !held.dropped.load(Ordering::SeqCst),
        "the publication is pending"
    );

    server.abort();
    assert!(server.await.unwrap_err().is_cancelled());

    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let closed = tokio::net::TcpStream::connect(addr).await.is_err();
        if held.dropped.load(Ordering::SeqCst) && closed {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "publication dropped: {}, listener closed: {closed}",
            held.dropped.load(Ordering::SeqCst)
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

fn oop_options(
    gear: &str,
    addr: SocketAddr,
    directory: &Arc<RecordingDirectory>,
    bearer: Option<toolkit_security::DynBearerAuthenticator>,
) -> OopServeOptions {
    OopServeOptions {
        gear_name: gear.to_owned(),
        instance_id: "i-1".to_owned(),
        version: None,
        advertise_uri: format!("http://{addr}"),
        listen_addr: addr,
        probe_bind_addr: None,
        drain_timeout: Duration::from_secs(5),
        heartbeat_interval: Duration::from_secs(30),
        healthcheck_timeout: Duration::from_millis(500),
        directory: Arc::clone(directory) as Arc<dyn DirectoryClient>,
        bearer_authenticator: bearer,
        internal_authenticator: Some(toolkit_security::DynInternalAuthenticator::new(
            AxisInternal,
        )),
        auth_disabled: true,
        labels: std::collections::BTreeMap::new(),
    }
}

/// A system gear serving REST that stays out of the directory, as a
/// `legacy_only` registry does.
struct Unadvertised;

#[async_trait]
impl Gear for Unadvertised {
    async fn init(&self, _ctx: &GearCtx) -> anyhow::Result<()> {
        Ok(())
    }
}

impl RestApiCapability for Unadvertised {
    fn register_rest(
        &self,
        _ctx: &GearCtx,
        router: Router,
        _openapi: &dyn OpenApiRegistry,
    ) -> anyhow::Result<Router> {
        Ok(router)
    }

    fn advertised_in_directory(&self) -> bool {
        false
    }
}

/// An application root co-hosting a system provider: the provider is
/// discoverable at this listener while the application group waits, the
/// root only once the group started, an unadvertised system gear
/// never.
#[tokio::test]
async fn a_co_hosted_system_provider_is_discoverable_under_an_application_root() {
    let addr = free_addr();
    let directory = Arc::new(RecordingDirectory::default());
    let root = Arc::new(PingGear);
    let system = Arc::new(AxisGear);
    let hidden = Arc::new(Unadvertised);
    let side = Arc::new(PublishingPing::default());
    let mut builder = RegistryBuilder::default();
    builder.register_core_with_meta("ping-gear", &[], root.clone() as Arc<dyn Gear>);
    builder.register_rest_with_meta("ping-gear", root as Arc<dyn RestApiCapability>);
    builder.register_core_with_meta("axis-gear", &[], system.clone() as Arc<dyn Gear>);
    builder.register_rest_with_meta("axis-gear", system as Arc<dyn RestApiCapability>);
    builder.register_system_with_meta(
        "axis-gear",
        Arc::new(SystemMarker) as Arc<dyn crate::contracts::SystemCapability>,
    );
    builder.register_core_with_meta("hidden", &[], hidden.clone() as Arc<dyn Gear>);
    builder.register_rest_with_meta("hidden", hidden as Arc<dyn RestApiCapability>);
    builder.register_system_with_meta(
        "hidden",
        Arc::new(SystemMarker) as Arc<dyn crate::contracts::SystemCapability>,
    );
    builder.register_core_with_meta("publishing-ping", &[], side.clone() as Arc<dyn Gear>);
    builder.register_rest_with_meta(
        "publishing-ping",
        side.clone() as Arc<dyn RestApiCapability>,
    );
    builder.register_gts_publication_with_meta(
        "publishing-ping",
        side.clone() as Arc<dyn crate::gts_publication::GtsPublicationCapability>,
    );
    let hub = Arc::new(ClientHub::new());
    hub.register::<PublishingPing>(Arc::clone(&side));
    let held = Arc::new(HeldRegistry::default());
    hub.register::<HeldRegistry>(Arc::clone(&held));
    let cancel = CancellationToken::new();
    let host = HostRuntime::new(
        builder.build_topo_sorted().unwrap(),
        Arc::new(EmptyConfigProvider) as Arc<dyn ConfigProvider>,
        DbOptions::None,
        hub,
        cancel.clone(),
        Uuid::new_v4(),
        None,
    );
    let server =
        tokio::spawn(host.run_oop_serving(oop_options("ping-gear", addr, &directory, None)));

    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while !directory
        .registered_gears
        .lock()
        .contains(&"axis-gear".to_owned())
    {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the system provider is present"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let registered_endpoint_seen = { directory.registered_endpoint.lock().clone() };
    assert_eq!(
        registered_endpoint_seen,
        Some(format!("http://{addr}")),
        "at this listener"
    );
    tokio::time::sleep(Duration::from_millis(50)).await;
    let registered_gears_seen = { directory.registered_gears.lock().clone() };
    assert_eq!(
        registered_gears_seen,
        ["axis-gear"],
        "neither the waiting root nor the unadvertised gear"
    );

    held.release.notify_one();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while !directory
        .registered_gears
        .lock()
        .contains(&"ping-gear".to_owned())
    {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the root once started"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(
        !directory
            .registered_gears
            .lock()
            .contains(&"hidden".to_owned())
    );

    cancel.cancel();
    tokio::time::timeout(Duration::from_secs(5), server)
        .await
        .expect("ends after cancel")
        .expect("no panic")
        .expect("clean shutdown");
    assert_eq!(
        directory.deregister_calls.load(Ordering::SeqCst),
        2,
        "root and provider"
    );
}

/// A system gear with a tenant-plane route.
struct TenantAxis;

#[async_trait]
impl Gear for TenantAxis {
    async fn init(&self, _ctx: &GearCtx) -> anyhow::Result<()> {
        Ok(())
    }
}

impl RestApiCapability for TenantAxis {
    fn register_rest(
        &self,
        _ctx: &GearCtx,
        router: Router,
        openapi: &dyn OpenApiRegistry,
    ) -> anyhow::Result<Router> {
        use crate::api::OperationBuilder;
        let router = OperationBuilder::get("/who")
            .operation_id("axis.who")
            .anonymous()
            .handler(
                |axum::Extension(ctx): axum::Extension<toolkit_security::SecurityContext>| async move {
                    if ctx.is_anonymous() { "anonymous" } else { "tenant" }
                },
            )
            .text_response(http::StatusCode::OK, "OK", "text/plain")
            .register(router, openapi);
        Ok(OperationBuilder::get("/tenant")
            .operation_id("axis.tenant")
            .authenticated()
            .no_license_required()
            .handler(|| async { "tenant" })
            .text_response(http::StatusCode::OK, "OK", "text/plain")
            .register(router, openapi))
    }
}

/// Accepts `good-bearer` only.
struct AcceptingBearer;

impl toolkit_security::BearerAuthenticator for AcceptingBearer {
    async fn authenticate(
        &self,
        token: &str,
    ) -> Result<toolkit_security::SecurityContext, toolkit_security::AuthNError> {
        if token == "good-bearer" {
            Ok(toolkit_security::SecurityContext::builder()
                .subject_id(Uuid::from_u128(1))
                .subject_tenant_id(Uuid::from_u128(2))
                .build()
                .expect("a tenant context"))
        } else {
            Err(toolkit_security::AuthNError::InvalidToken)
        }
    }
}

/// An application gear registering the tenant-plane bridge in its `init`, as
/// a linked authn-resolver does.
struct BearerBridge;

#[async_trait]
impl Gear for BearerBridge {
    async fn init(&self, ctx: &GearCtx) -> anyhow::Result<()> {
        ctx.client_hub()
            .register::<toolkit_security::DynBearerAuthenticator>(Arc::new(
                toolkit_security::DynBearerAuthenticator::new(AcceptingBearer),
            ));
        Ok(())
    }
}

/// A system tenant route is `503` before the application group —
/// no authenticator exists yet, and it is not served unauthenticated — then
/// authenticated by the bridge the group's `init` registered, on the same URL.
#[tokio::test]
async fn a_system_tenant_route_is_authenticated_once_the_bridge_exists() {
    let addr = free_addr();
    let directory = Arc::new(RecordingDirectory::default());
    let system = Arc::new(TenantAxis);
    let side = Arc::new(PublishingPing::default());
    let mut builder = RegistryBuilder::default();
    builder.register_core_with_meta("tenant-axis", &[], system.clone() as Arc<dyn Gear>);
    builder.register_rest_with_meta("tenant-axis", system as Arc<dyn RestApiCapability>);
    builder.register_system_with_meta(
        "tenant-axis",
        Arc::new(SystemMarker) as Arc<dyn crate::contracts::SystemCapability>,
    );
    builder.register_core_with_meta("bridge", &[], Arc::new(BearerBridge) as Arc<dyn Gear>);
    builder.register_core_with_meta("publishing-ping", &[], side.clone() as Arc<dyn Gear>);
    builder.register_rest_with_meta(
        "publishing-ping",
        side.clone() as Arc<dyn RestApiCapability>,
    );
    builder.register_gts_publication_with_meta(
        "publishing-ping",
        side.clone() as Arc<dyn crate::gts_publication::GtsPublicationCapability>,
    );
    let hub = Arc::new(ClientHub::new());
    hub.register::<PublishingPing>(Arc::clone(&side));
    let held = Arc::new(HeldRegistry::default());
    hub.register::<HeldRegistry>(Arc::clone(&held));
    let cancel = CancellationToken::new();
    let host = HostRuntime::new(
        builder.build_topo_sorted().unwrap(),
        Arc::new(EmptyConfigProvider) as Arc<dyn ConfigProvider>,
        DbOptions::None,
        hub,
        cancel.clone(),
        Uuid::new_v4(),
        None,
    );
    let server =
        tokio::spawn(host.run_oop_serving(oop_options("tenant-axis", addr, &directory, None)));
    let good = [("Authorization", "Bearer good-bearer")];

    assert!(poll_status(addr, "/healthz", 200, Duration::from_secs(5)).await);
    assert!(poll_status(addr, "/tenant", 503, Duration::from_secs(5)).await);
    assert_eq!(http_status(addr, "/tenant", &good).await, Some(503));

    held.release.notify_one();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while http_status(addr, "/tenant", &good).await != Some(200) {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the tenant plane after the group"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(
        http_status(addr, "/tenant", &[("Authorization", "Bearer nope")]).await,
        Some(401)
    );
    assert_eq!(http_status(addr, "/tenant", &[]).await, Some(401));

    cancel.cancel();
    tokio::time::timeout(Duration::from_secs(5), server)
        .await
        .expect("ends after cancel")
        .expect("no panic")
        .expect("clean shutdown");
}

/// Before the application group, an anonymous system route runs as anonymous,
/// not as the development-only default tenant: tenant authentication is not
/// disabled, it is only not installed yet. Tenant routes still answer `503`.
#[tokio::test]
async fn an_anonymous_system_route_stays_anonymous_before_the_application_group() {
    let addr = free_addr();
    let directory = Arc::new(RecordingDirectory::default());
    let system = Arc::new(TenantAxis);
    let side = Arc::new(PublishingPing::default());
    let mut builder = RegistryBuilder::default();
    builder.register_core_with_meta("tenant-axis", &[], system.clone() as Arc<dyn Gear>);
    builder.register_rest_with_meta("tenant-axis", system as Arc<dyn RestApiCapability>);
    builder.register_system_with_meta(
        "tenant-axis",
        Arc::new(SystemMarker) as Arc<dyn crate::contracts::SystemCapability>,
    );
    builder.register_core_with_meta("bridge", &[], Arc::new(BearerBridge) as Arc<dyn Gear>);
    builder.register_core_with_meta("publishing-ping", &[], side.clone() as Arc<dyn Gear>);
    builder.register_rest_with_meta(
        "publishing-ping",
        side.clone() as Arc<dyn RestApiCapability>,
    );
    builder.register_gts_publication_with_meta(
        "publishing-ping",
        side.clone() as Arc<dyn crate::gts_publication::GtsPublicationCapability>,
    );
    let hub = Arc::new(ClientHub::new());
    hub.register::<PublishingPing>(Arc::clone(&side));
    let held = Arc::new(HeldRegistry::default());
    hub.register::<HeldRegistry>(Arc::clone(&held));
    let cancel = CancellationToken::new();
    let host = HostRuntime::new(
        builder.build_topo_sorted().unwrap(),
        Arc::new(EmptyConfigProvider) as Arc<dyn ConfigProvider>,
        DbOptions::None,
        hub,
        cancel.clone(),
        Uuid::new_v4(),
        None,
    );
    let options = OopServeOptions {
        auth_disabled: false,
        ..oop_options("tenant-axis", addr, &directory, None)
    };
    let server = tokio::spawn(host.run_oop_serving(options));

    assert!(poll_status(addr, "/who", 200, Duration::from_secs(5)).await);
    assert_eq!(
        http_get(addr, "/who").await.map(|r| r.1),
        Some("anonymous".to_owned()),
        "no synthetic tenant context before the application group"
    );
    assert_eq!(http_status(addr, "/tenant", &[]).await, Some(503));
    assert_eq!(
        http_status(addr, "/tenant", &[("Authorization", "Bearer good-bearer")]).await,
        Some(503)
    );

    held.release.notify_one();
    let good = [("Authorization", "Bearer good-bearer")];
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while http_status(addr, "/tenant", &good).await != Some(200) {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the tenant plane after the group"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(
        http_get(addr, "/who").await.map(|r| r.1),
        Some("anonymous".to_owned())
    );

    cancel.cancel();
    tokio::time::timeout(Duration::from_secs(5), server)
        .await
        .expect("ends after cancel")
        .expect("no panic")
        .expect("clean shutdown");
}

/// A root whose `/slow` route blocks until released.
struct Slow {
    entered: Arc<tokio::sync::Notify>,
    release: Arc<tokio::sync::Notify>,
}

#[async_trait]
impl Gear for Slow {
    async fn init(&self, _ctx: &GearCtx) -> anyhow::Result<()> {
        Ok(())
    }
}

impl RestApiCapability for Slow {
    fn register_rest(
        &self,
        _ctx: &GearCtx,
        router: Router,
        _openapi: &dyn OpenApiRegistry,
    ) -> anyhow::Result<Router> {
        let (entered, release) = (Arc::clone(&self.entered), Arc::clone(&self.release));
        Ok(router.route(
            "/slow",
            get(move || {
                let (entered, release) = (Arc::clone(&entered), Arc::clone(&release));
                async move {
                    entered.notify_one();
                    release.notified().await;
                    "done"
                }
            }),
        ))
    }
}

/// Shutdown flips readiness to draining first: while an in-flight request
/// still holds the HTTP drain, the process already reports draining.
#[tokio::test]
async fn shutdown_reports_draining_before_in_flight_requests_drain() {
    let addr = free_addr();
    let directory = Arc::new(RecordingDirectory::default());
    let gear = Arc::new(Slow {
        entered: Arc::new(tokio::sync::Notify::new()),
        release: Arc::new(tokio::sync::Notify::new()),
    });
    let mut builder = RegistryBuilder::default();
    builder.register_core_with_meta("slow", &[], gear.clone() as Arc<dyn Gear>);
    builder.register_rest_with_meta("slow", gear.clone() as Arc<dyn RestApiCapability>);
    let cancel = CancellationToken::new();
    let host = HostRuntime::new(
        builder.build_topo_sorted().unwrap(),
        Arc::new(EmptyConfigProvider) as Arc<dyn ConfigProvider>,
        DbOptions::None,
        Arc::new(ClientHub::new()),
        cancel.clone(),
        Uuid::new_v4(),
        None,
    );
    let readiness = Arc::clone(&host.dep_checker);
    let server = tokio::spawn(host.run_oop_serving(oop_options("slow", addr, &directory, None)));
    assert!(poll_status(addr, "/readyz", 200, Duration::from_secs(5)).await);

    let in_flight = tokio::spawn(http_get(addr, "/slow"));
    gear.entered.notified().await;
    cancel.cancel();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    while !readiness.is_draining() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "draining while the request is in flight"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert!(
        !in_flight.is_finished(),
        "the request still holds the drain"
    );

    gear.release.notify_one();
    assert_eq!(in_flight.await.unwrap().map(|r| r.0), Some(200));
    tokio::time::timeout(Duration::from_secs(10), server)
        .await
        .expect("ends after the drain")
        .expect("no panic")
        .expect("clean shutdown");
}

#[allow(
    clippy::unnecessary_wraps,
    clippy::needless_pass_by_value,
    reason = "must match the WireFn signature the gear macro emits"
)]
fn no_wire(
    _hub: &ClientHub,
    _resolver: Arc<dyn crate::discovery::EndpointResolver>,
    _tuning: toolkit_contract::wiring::ClientTuning,
) -> anyhow::Result<crate::discovery::WireOutcome> {
    Ok(crate::discovery::WireOutcome::Local)
}

/// The `OoP` path runs the groups like the in-process one: the system group
/// complete, then the application group, with the legacy finalization once
/// between its `init` and its `post_init`.
#[tokio::test]
async fn the_oop_path_runs_each_group_and_the_legacy_finalization_in_order() {
    use super::host_runtime_preparation_tests::{
        Events, FakeRegistry, GROUPED_STARTUP, grouped_gears, runtime,
    };
    let events: Events = Arc::default();
    let cancel = CancellationToken::new();
    let host = runtime(
        grouped_gears(&events).build_topo_sorted().unwrap(),
        &FakeRegistry::with(&[]),
        &cancel,
    );
    let checker = Arc::clone(&host.dep_checker);
    let addr = free_addr();
    let directory = Arc::new(RecordingDirectory::default());
    let server = tokio::spawn(host.run_oop_serving(oop_options("a", addr, &directory, None)));

    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while !checker.unresolved_deps().is_empty() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the application group starts: {:?}",
            events.lock()
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let events_seen = { events.lock().clone() };
    assert_eq!(events_seen, GROUPED_STARTUP);

    cancel.cancel();
    tokio::time::timeout(Duration::from_secs(5), server)
        .await
        .expect("ends after cancel")
        .expect("no panic")
        .expect("clean shutdown");
}

/// The `OoP` startup wires a consumed client after the application group's
/// `init` and before its `post_init`, as the in-process one does.
#[tokio::test]
async fn the_oop_path_wires_a_consumed_client_between_init_and_post_init() {
    use super::host_runtime_preparation_tests::{Consumer, WIRED_CLIENT};
    let consumer = Arc::new(Consumer::default());
    let mut builder = RegistryBuilder::default();
    builder.register_core_with_meta("consumer", &[], consumer.clone() as Arc<dyn Gear>);
    let cancel = CancellationToken::new();
    let host = HostRuntime::new(
        builder.build_topo_sorted().unwrap(),
        Arc::new(EmptyConfigProvider) as Arc<dyn ConfigProvider>,
        DbOptions::None,
        Arc::new(ClientHub::new()),
        cancel.clone(),
        Uuid::new_v4(),
        None,
    )
    .with_consumer_registrations(vec![&WIRED_CLIENT]);
    let addr = free_addr();
    let directory = Arc::new(RecordingDirectory::default());
    let server =
        tokio::spawn(host.run_oop_serving(oop_options("consumer", addr, &directory, None)));

    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while !consumer.post_init_ran.load(Ordering::SeqCst) {
        assert!(tokio::time::Instant::now() < deadline, "post_init");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    cancel.cancel();
    tokio::time::timeout(Duration::from_secs(5), server)
        .await
        .expect("ends after cancel")
        .expect("no panic")
        .expect("clean shutdown");

    assert!(!consumer.in_init.load(Ordering::SeqCst), "not yet in init");
    assert!(
        consumer.in_post_init.load(Ordering::SeqCst),
        "wired before post_init"
    );
}
