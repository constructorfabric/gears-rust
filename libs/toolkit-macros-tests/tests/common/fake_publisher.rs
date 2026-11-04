//! A publisher adapter with the interface `gts_publication(publisher = ...)` expects.
#![allow(dead_code)]

use std::num::NonZeroUsize;
use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;
use tokio_util::sync::CancellationToken;

/// The policy the adapter's builder received, for the tests to compare.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Policy {
    pub deadline: Option<tokio::time::Instant>,
    pub timeout: Option<Duration>,
    pub retry_backoff: (Duration, Duration),
    pub batch_size: NonZeroUsize,
}

impl Policy {
    /// What the runtime forwards for `options` and a caller's `deadline`.
    pub fn of(
        options: toolkit::gts_publication::GtsPublicationOptions,
        deadline: Option<tokio::time::Instant>,
    ) -> Self {
        Self {
            deadline,
            timeout: options.timeout(),
            retry_backoff: (options.retry_backoff(), options.retry_backoff_max()),
            batch_size: options.batch_size(),
        }
    }
}

/// The registry contract the adapter publishes through.
#[async_trait::async_trait]
pub trait FakeRegistryApi: Send + Sync {
    async fn register(
        &self,
        publisher: (String, String),
        documents: Vec<(gts::GtsId, serde_json::Value)>,
        policy: Policy,
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
        _documents: Vec<(gts::GtsId, serde_json::Value)>,
        _policy: Policy,
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

/// A publication of one manifest: the adapter's `Publication`.
pub struct Publication<'a> {
    registry: &'a Registry,
    publisher: (String, String),
    documents: Vec<(gts::GtsId, serde_json::Value)>,
    deadline: Option<tokio::time::Instant>,
    timeout: Option<Duration>,
    retry_backoff: Option<(Duration, Duration)>,
    batch_size: Option<NonZeroUsize>,
}

impl<'a> Publication<'a> {
    pub fn from_manifest(
        registry: &'a Registry,
        publisher_name: &str,
        publisher_version: &str,
        documents: &[(gts::GtsId, serde_json::Value)],
    ) -> Result<Self, PublishError> {
        Ok(Self {
            registry,
            publisher: (publisher_name.to_owned(), publisher_version.to_owned()),
            documents: documents.to_vec(),
            deadline: None,
            timeout: None,
            retry_backoff: None,
            batch_size: None,
        })
    }

    pub fn deadline(mut self, deadline: Option<tokio::time::Instant>) -> Self {
        self.deadline = deadline;
        self
    }

    pub fn timeout(mut self, timeout: Option<Duration>) -> Self {
        self.timeout = timeout;
        self
    }

    pub fn retry_backoff(mut self, initial: Duration, max: Duration) -> Self {
        self.retry_backoff = Some((initial, max));
        self
    }

    pub fn batch_size(mut self, batch_size: NonZeroUsize) -> Self {
        self.batch_size = Some(batch_size);
        self
    }

    pub fn on_pending(self, _on_pending: impl FnMut(&PublishError) + Send) -> Self {
        self
    }

    /// Fails unless the runtime called every policy builder.
    pub async fn publish(self, _cancel: &CancellationToken) -> Result<(), PublishError> {
        let (Some(retry_backoff), Some(batch_size)) = (self.retry_backoff, self.batch_size) else {
            return Err(PublishError(
                "the runtime did not forward the policy".to_owned(),
            ));
        };
        let policy = Policy {
            deadline: self.deadline,
            timeout: self.timeout,
            retry_backoff,
            batch_size,
        };
        self.registry
            .register(self.publisher, self.documents, policy)
            .await
            .map_err(PublishError)
    }
}

/// One admitted publication: `(publisher name, version)` and its documents.
pub type Admitted = ((String, String), Vec<(gts::GtsId, serde_json::Value)>);

/// A local registry that records what it admitted.
#[derive(Default)]
pub struct LocalRegistry {
    pub admitted: Mutex<Vec<Admitted>>,
    /// The policy of each admitted publication, by publisher name.
    pub policies: Mutex<Vec<(String, Policy)>>,
}

#[async_trait::async_trait]
impl FakeRegistryApi for LocalRegistry {
    async fn register(
        &self,
        publisher: (String, String),
        documents: Vec<(gts::GtsId, serde_json::Value)>,
        policy: Policy,
    ) -> Result<(), String> {
        self.policies.lock().push((publisher.0.clone(), policy));
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
