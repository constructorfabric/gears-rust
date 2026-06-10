// Created: 2026-06-11 by Constructor Tech
// @cpt-dod:cpt-cf-clst-dod-showcase-audit-examples:p1
//! Showcase: the plugin-author builder/handle shape (ADR-006).
//!
//! A backend plugin author writes two things and no framework integration code:
//!
//! 1. a **backend trait impl** for each primitive the plugin serves (here
//!    [`ClusterCacheBackend`]); and
//! 2. a **builder/handle pair** — `Plugin::builder()` produces a builder whose
//!    `build_and_start()` spawns the plugin's background tasks (TTL reapers,
//!    renewal loops, watch fan-out, …) and returns a handle. The handle's
//!    `stop()` is the single release path; the parent host gear owns the handle
//!    and calls `stop()` from its own shutdown.
//!
//! This example implements that shape against an in-memory store and a
//! representative background maintenance task, then drives the full lifecycle:
//! build → register in `ClientHub` → resolve & use → stop.
//!
//! Run with: `cargo run --example plugin_author`

mod common;

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use cluster_sdk::cache::{
    CacheConsistency, CacheEntry, CacheFeatures, CacheWatch, ClusterCacheBackend, ClusterCacheV1,
};
use cluster_sdk::error::ClusterError;
use cluster_sdk::profile::ClusterProfile;
use cluster_sdk::registration::{deregister_cache_backend, register_cache_backend};
use common::MemCacheBackend;
use tokio::sync::Notify;
use tokio::task::JoinHandle;
use toolkit::client_hub::ClientHub;

/// The profile the host binds this plugin's backend under.
#[derive(Clone, Copy)]
struct AppProfile;

impl ClusterProfile for AppProfile {
    const NAME: &'static str = "app";
}

// ---------------------------------------------------------------------------
// 1. The backend trait impl.
// ---------------------------------------------------------------------------

/// The plugin's cache backend. A real plugin implements every method against its
/// own store (Postgres rows, Redis keys, …); here it delegates to an in-memory
/// store so the example stays self-contained. The point is the *shape*: a
/// concrete `ClusterCacheBackend` the wiring crate registers as `Arc<dyn _>`.
struct ExampleCacheBackend {
    store: Arc<dyn ClusterCacheBackend>,
}

#[async_trait]
impl ClusterCacheBackend for ExampleCacheBackend {
    fn provider_name(&self) -> &'static str {
        "ExampleCachePlugin"
    }

    fn consistency(&self) -> CacheConsistency {
        self.store.consistency()
    }

    fn features(&self) -> CacheFeatures {
        self.store.features()
    }

    async fn get(&self, key: &str) -> Result<Option<CacheEntry>, ClusterError> {
        self.store.get(key).await
    }

    async fn put(
        &self,
        key: &str,
        value: &[u8],
        ttl: Option<Duration>,
    ) -> Result<(), ClusterError> {
        self.store.put(key, value, ttl).await
    }

    async fn delete(&self, key: &str) -> Result<bool, ClusterError> {
        self.store.delete(key).await
    }

    async fn contains(&self, key: &str) -> Result<bool, ClusterError> {
        self.store.contains(key).await
    }

    async fn put_if_absent(
        &self,
        key: &str,
        value: &[u8],
        ttl: Option<Duration>,
    ) -> Result<Option<CacheEntry>, ClusterError> {
        self.store.put_if_absent(key, value, ttl).await
    }

    async fn compare_and_swap(
        &self,
        key: &str,
        expected_version: u64,
        new_value: &[u8],
        ttl: Option<Duration>,
    ) -> Result<CacheEntry, ClusterError> {
        self.store
            .compare_and_swap(key, expected_version, new_value, ttl)
            .await
    }

    async fn watch(&self, key: &str) -> Result<CacheWatch, ClusterError> {
        self.store.watch(key).await
    }

    async fn watch_prefix(&self, prefix: &str) -> Result<CacheWatch, ClusterError> {
        self.store.watch_prefix(prefix).await
    }

    async fn scan_prefix(&self, prefix: &str) -> Result<Vec<String>, ClusterError> {
        self.store.scan_prefix(prefix).await
    }
}

// ---------------------------------------------------------------------------
// 2. The builder / handle pair (ADR-006).
// ---------------------------------------------------------------------------

/// The plugin entry point. `ExampleClusterPlugin::builder()` is the only way in.
struct ExampleClusterPlugin;

impl ExampleClusterPlugin {
    fn builder() -> ExampleClusterPluginBuilder {
        ExampleClusterPluginBuilder {
            maintenance_interval: Duration::from_millis(50),
        }
    }
}

/// Collects configuration, then `build_and_start()` brings the plugin up.
struct ExampleClusterPluginBuilder {
    maintenance_interval: Duration,
}

impl ExampleClusterPluginBuilder {
    /// Tunes the background maintenance cadence (in a real plugin: TTL-reaper /
    /// renewal interval).
    fn maintenance_interval(mut self, interval: Duration) -> Self {
        self.maintenance_interval = interval;
        self
    }

    /// Builds the backend and starts the plugin's background tasks, returning the
    /// handle that owns them.
    ///
    /// # Errors
    /// Propagates any [`ClusterError`] from the plugin's startup probe.
    async fn build_and_start(self) -> Result<ExampleClusterPluginHandle, ClusterError> {
        // @cpt-begin:cpt-cf-clst-flow-showcase-audit-consumer-examples:p1:inst-ex-plugin
        let backend: Arc<dyn ClusterCacheBackend> = Arc::new(ExampleCacheBackend {
            store: MemCacheBackend::linearizable(),
        });
        // @cpt-end:cpt-cf-clst-flow-showcase-audit-consumer-examples:p1:inst-ex-plugin

        // A representative startup probe — a real plugin verifies connectivity
        // to its store here before declaring itself started.
        backend.contains("__plugin_health__").await?;

        // Spawn the background maintenance task and keep a shutdown signal so
        // `stop()` can wind it down gracefully.
        let shutdown = Arc::new(Notify::new());
        let task = spawn_maintenance(
            Arc::clone(&backend),
            self.maintenance_interval,
            Arc::clone(&shutdown),
        );

        Ok(ExampleClusterPluginHandle {
            backend,
            shutdown,
            task: Some(task),
        })
    }
}

/// Owns the plugin's running backend and background task. The host registers
/// [`backend`](Self::backend) in `ClientHub` and calls [`stop`](Self::stop) at
/// shutdown — the single release path.
struct ExampleClusterPluginHandle {
    backend: Arc<dyn ClusterCacheBackend>,
    shutdown: Arc<Notify>,
    task: Option<JoinHandle<()>>,
}

impl ExampleClusterPluginHandle {
    /// The backend to register in `ClientHub` (`Arc<dyn _>`, per ADR-005).
    fn backend(&self) -> Arc<dyn ClusterCacheBackend> {
        Arc::clone(&self.backend)
    }

    /// Signals the background task to stop and waits for it to wind down.
    async fn stop(mut self) {
        self.shutdown.notify_one();
        if let Some(task) = self.task.take()
            && task.await.is_err()
        {
            println!("[plugin] maintenance task ended abnormally");
        }
        println!("[plugin] stopped");
    }
}

/// The plugin's background task: a periodic maintenance pass that runs until the
/// shutdown signal fires. Stands in for the TTL reapers / renewal loops real
/// backends own.
fn spawn_maintenance(
    backend: Arc<dyn ClusterCacheBackend>,
    interval: Duration,
    shutdown: Arc<Notify>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(interval);
        loop {
            tokio::select! {
                () = shutdown.notified() => break,
                _ = ticker.tick() => {
                    // A real reaper would expire stale entries here; we just touch
                    // the keyspace to model periodic work. A gone store ends the loop.
                    if backend.scan_prefix("").await.is_err() {
                        break;
                    }
                }
            }
        }
    })
}

#[tokio::main]
async fn main() -> Result<(), ClusterError> {
    // The host owns the plugin lifecycle via the builder/handle pair.
    let handle = ExampleClusterPlugin::builder()
        .maintenance_interval(Duration::from_millis(25))
        .build_and_start()
        .await?;
    println!("[plugin] started");

    // Wiring step: register the plugin's backend in ClientHub under the profile.
    let hub = ClientHub::new();
    register_cache_backend(&hub, AppProfile::NAME, handle.backend())?;

    // Consumers resolve and use it exactly as in the other examples.
    let cache = ClusterCacheV1::resolver(&hub)
        .profile(AppProfile)
        .resolve()?;
    cache.put("plugin/demo", b"ok", None).await?;
    if let Some(entry) = cache.get("plugin/demo").await? {
        println!(
            "[consumer] read plugin/demo = {} via provider",
            String::from_utf8_lossy(&entry.value)
        );
    }

    // Shutdown: deregister from ClientHub, then stop the plugin (single release
    // path). After this, resolutions on the profile fail with ProfileNotBound.
    deregister_cache_backend(&hub, AppProfile::NAME)?;
    handle.stop().await;
    Ok(())
}
