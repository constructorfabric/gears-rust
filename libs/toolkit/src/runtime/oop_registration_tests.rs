//! Tests for `OoP` self-registration and dependency resolution.

use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use cf_system_sdks::directory::{ServiceEndpoint, ServiceInstanceInfo};

/// Stub directory: fails `register`/`resolve` a configurable number of times,
/// then succeeds; records register calls and heartbeats.
struct StubDirectory {
    fail_register: AtomicUsize,
    register_calls: AtomicUsize,
    heartbeats: AtomicUsize,
    fail_resolve: AtomicUsize,
}

impl StubDirectory {
    fn new(fail_register: usize, fail_resolve: usize) -> Self {
        Self {
            fail_register: AtomicUsize::new(fail_register),
            register_calls: AtomicUsize::new(0),
            heartbeats: AtomicUsize::new(0),
            fail_resolve: AtomicUsize::new(fail_resolve),
        }
    }
}

#[async_trait]
impl DirectoryClient for StubDirectory {
    async fn resolve_grpc_service(&self, _service: &str) -> anyhow::Result<ServiceEndpoint> {
        Ok(ServiceEndpoint::new("http://grpc"))
    }

    async fn resolve_rest_service(&self, gear: &str) -> anyhow::Result<ServiceEndpoint> {
        if self.fail_resolve.load(Ordering::SeqCst) > 0 {
            self.fail_resolve.fetch_sub(1, Ordering::SeqCst);
            anyhow::bail!("not yet available");
        }
        Ok(ServiceEndpoint::new(format!("http://{gear}:8080")))
    }

    async fn get_openapi_spec(&self, _gear: &str) -> anyhow::Result<String> {
        Ok(String::new())
    }

    async fn list_instances(&self, _gear: &str) -> anyhow::Result<Vec<ServiceInstanceInfo>> {
        Ok(vec![])
    }

    async fn register_instance(&self, _info: RegisterInstanceInfo) -> anyhow::Result<()> {
        self.register_calls.fetch_add(1, Ordering::SeqCst);
        if self.fail_register.load(Ordering::SeqCst) > 0 {
            self.fail_register.fetch_sub(1, Ordering::SeqCst);
            anyhow::bail!("directory unavailable");
        }
        Ok(())
    }

    async fn deregister_instance(&self, _gear: &str, _instance: &str) -> anyhow::Result<()> {
        Ok(())
    }

    async fn send_heartbeat(&self, _gear: &str, _instance: &str) -> anyhow::Result<()> {
        self.heartbeats.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

fn info() -> RegisterInstanceInfo {
    RegisterInstanceInfo {
        gear: "billing".to_owned(),
        instance_id: "i-1".to_owned(),
        grpc_services: vec![],
        version: Some("1.0.0".to_owned()),
        rest_endpoint: Some(ServiceEndpoint::new("http://billing:8080")),
        openapi_spec: Some("{}".to_owned()),
    }
}

#[test]
fn backoff_doubles_and_caps() {
    assert_eq!(
        next_backoff(Duration::from_millis(100)),
        Duration::from_millis(200)
    );
    assert_eq!(
        next_backoff(Duration::from_millis(200)),
        Duration::from_millis(400)
    );
    assert_eq!(next_backoff(Duration::from_secs(20)), MAX_BACKOFF);
    assert_eq!(next_backoff(MAX_BACKOFF), MAX_BACKOFF);
}

#[tokio::test]
async fn registration_retries_until_success() {
    let directory: Arc<dyn DirectoryClient> = Arc::new(StubDirectory::new(2, 0));
    let cancel = CancellationToken::new();
    let ok = register_once_with_backoff(&directory, &info(), &cancel).await;
    assert!(ok);
}

#[tokio::test]
async fn registration_returns_false_when_cancelled() {
    let directory: Arc<dyn DirectoryClient> = Arc::new(StubDirectory::new(1000, 0));
    let cancel = CancellationToken::new();
    cancel.cancel();
    let ok = register_once_with_backoff(&directory, &info(), &cancel).await;
    assert!(!ok, "cancelled registration must return false");
}

#[tokio::test]
async fn presence_loop_registers_once_then_heartbeats_until_cancel() {
    let stub = Arc::new(StubDirectory::new(0, 0));
    let directory: Arc<dyn DirectoryClient> = stub.clone();
    let cancel = CancellationToken::new();

    // 1s is the minimum interval presence_loop clamps to; wait past one tick so
    // at least one heartbeat fires.
    let task = tokio::spawn(presence_loop(
        Arc::clone(&directory),
        info(),
        Duration::from_secs(1),
        cancel.clone(),
    ));

    tokio::time::sleep(Duration::from_millis(1500)).await;

    assert_eq!(
        stub.register_calls.load(Ordering::SeqCst),
        1,
        "steady state registers exactly once (self-heal re-register is 30s, not fired here)"
    );
    assert!(
        stub.heartbeats.load(Ordering::SeqCst) >= 1,
        "the single presence task must send heartbeats"
    );

    cancel.cancel();
    task.await.unwrap();
}

// Dependency resolution moved to the proxy-wiring phase (typed
// `#[toolkit::consumes]` clients feeding the shared `DependencyChecker`); the
// former `resolve_one_dep`/`resolve_deps`/`ResolvedRestEndpoints` stopgap and
// its tests were retired. Consumer-wiring resolution is covered by
// `host_runtime` proxy-wiring tests and the api-contracts example E2E tests.
