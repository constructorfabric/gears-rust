//! A publisher adapter with the interface `gts(publisher = ...)` expects.
#![allow(dead_code)]

use std::sync::Arc;

use parking_lot::Mutex;
use tokio_util::sync::CancellationToken;

/// The registry contract the adapter publishes through.
#[async_trait::async_trait]
pub trait FakeRegistryApi: Send + Sync {
    async fn register(
        &self,
        publisher: (String, String),
        documents: Vec<(String, serde_json::Value)>,
    ) -> Result<(), String>;
}

pub type Registry = dyn FakeRegistryApi;
/// The client `#[toolkit::consumes]` of [`FakeRegistryApi`] wires.
pub type FakeRegistryApiRestResolvingClient = ResolvingClient;
pub const PROVIDER: &str = "fake-registry";

/// The remote client: never reachable in these tests.
pub struct ResolvingClient;

impl ResolvingClient {
    pub fn new(
        _resolver: Arc<dyn toolkit::discovery::EndpointResolver>,
        _provider: &'static str,
        _tuning: toolkit::contract_support::wiring::ClientTuning,
    ) -> Self {
        Self
    }
}

#[async_trait::async_trait]
impl FakeRegistryApi for ResolvingClient {
    async fn register(
        &self,
        _publisher: (String, String),
        _documents: Vec<(String, serde_json::Value)>,
    ) -> Result<(), String> {
        Err("unreachable".to_owned())
    }
}

#[derive(Debug)]
pub struct PublishError(pub String);

impl std::fmt::Display for PublishError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for PublishError {}

pub async fn publish(
    registry: &Registry,
    publisher_name: &str,
    publisher_version: &str,
    documents: &[(String, serde_json::Value)],
    _cancel: &CancellationToken,
    _on_pending: impl FnMut(&str) + Send,
) -> Result<(), PublishError> {
    registry
        .register(
            (publisher_name.to_owned(), publisher_version.to_owned()),
            documents.to_vec(),
        )
        .await
        .map_err(PublishError)
}

/// One admitted publication: `(publisher name, version)` and its documents.
pub type Admitted = ((String, String), Vec<(String, serde_json::Value)>);

/// A local registry that records what it admitted.
#[derive(Default)]
pub struct LocalRegistry {
    pub admitted: Mutex<Vec<Admitted>>,
}

#[async_trait::async_trait]
impl FakeRegistryApi for LocalRegistry {
    async fn register(
        &self,
        publisher: (String, String),
        documents: Vec<(String, serde_json::Value)>,
    ) -> Result<(), String> {
        self.admitted.lock().push((publisher, documents));
        Ok(())
    }
}

/// An empty gear config provider with per-gear `config` sections.
pub struct Config(pub serde_json::Value);

impl toolkit::ConfigProvider for Config {
    fn get_gear_config(&self, gear: &str) -> Option<&serde_json::Value> {
        self.0.get(gear)
    }
}

/// Poll `condition` for up to two seconds.
pub async fn until(what: &str, mut condition: impl FnMut() -> bool) {
    for _ in 0..2000 {
        if condition() {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(1)).await;
    }
    panic!("timed out waiting for {what}");
}
