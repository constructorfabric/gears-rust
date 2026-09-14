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
    assert_eq!(
        directory.registered_endpoint.lock().clone(),
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
