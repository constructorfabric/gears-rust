// Created: 2026-06-11 by Constructor Tech
//! The standalone cluster plugin's outbox-style builder and lifecycle handle
//! (DESIGN §3.7). The plugin is a library, not a `RunnableCapability`: a parent
//! host gear (or the cluster wiring crate) owns the [`StandaloneClusterHandle`]
//! from its own `start`/`stop`.

use std::sync::Arc;

use cluster_sdk::observability::otel::OtelClusterMetrics;
use cluster_sdk::{
    CacheBasedServiceDiscoveryBackend, CasBasedDistributedLockBackend,
    CasBasedLeaderElectionBackend, ClusterCacheBackend, ClusterError, ClusterMetrics,
    DistributedLockBackend, InstrumentedCache, LeaderElectionBackend, ServiceDiscoveryBackend,
};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::cache::StandaloneCache;
use crate::config::StandaloneClusterConfig;
use crate::provider::PROVIDER_NAME;

/// Entry point for constructing the standalone in-process cluster plugin.
///
/// ```no_run
/// # async fn doc() -> Result<(), cluster_sdk::ClusterError> {
/// use standalone_cluster_plugin::StandaloneClusterPlugin;
/// let handle = StandaloneClusterPlugin::builder().build_and_start()?;
/// handle.stop().await;
/// # Ok(())
/// # }
/// ```
pub struct StandaloneClusterPlugin;

impl StandaloneClusterPlugin {
    /// Returns a builder with the [`default`](StandaloneClusterConfig::default)
    /// configuration.
    pub fn builder() -> StandaloneClusterBuilder {
        StandaloneClusterBuilder {
            config: StandaloneClusterConfig::default(),
        }
    }
}

/// A fluent builder for the standalone plugin. Build and start it with
/// [`build_and_start`](Self::build_and_start).
#[must_use = "a builder starts nothing until `.build_and_start()` is called"]
pub struct StandaloneClusterBuilder {
    config: StandaloneClusterConfig,
}

impl StandaloneClusterBuilder {
    /// Replaces the whole configuration.
    pub fn config(mut self, config: StandaloneClusterConfig) -> Self {
        self.config = config;
        self
    }

    /// Overrides just the cache TTL-sweep cadence.
    pub fn sweep_interval(mut self, interval: std::time::Duration) -> Self {
        self.config.sweep_interval = interval;
        self
    }

    /// Builds the plugin: creates the native cache, starts its TTL sweeper, and
    /// layers the SDK default leader-election, lock, and service-discovery
    /// backends over it.
    ///
    /// # Errors
    /// - [`ClusterError::InvalidConfig`] if `sweep_interval` is zero (which would
    ///   otherwise panic the underlying interval timer).
    /// - Propagates any [`ClusterError`] from constructing the default backends
    ///   (none expected: the native cache is linearizable, which their
    ///   consistency guard requires).
    pub fn build_and_start(self) -> Result<StandaloneClusterHandle, ClusterError> {
        if self.config.sweep_interval.is_zero() {
            return Err(ClusterError::InvalidConfig {
                reason: "standalone cluster plugin sweep_interval must be non-zero".to_owned(),
            });
        }

        let cache = StandaloneCache::new();
        let shutdown = CancellationToken::new();
        let sweeper = cache.spawn_sweeper(self.config.sweep_interval, shutdown.clone());

        // The shared metrics sink for every primitive, labelled `standalone`.
        // Built over the process-global OTel meter; if no meter provider is
        // installed (the zero-infra dev path, tests) it is transparently a no-op
        // (ADR-004). Spans and log events are emitted via `tracing` regardless.
        let metrics: Arc<dyn ClusterMetrics> =
            Arc::new(OtelClusterMetrics::from_global_meter(PROVIDER_NAME));

        // Wrap the native cache in the SDK's `InstrumentedCache` decorator so its
        // operations emit the contracted `cluster.cache.*` signals. The SDK
        // default backends below are built over this instrumented handle, so the
        // cache traffic of their internal coordination is observable too. The
        // concrete `Arc<StandaloneCache>` is retained separately for `stop` to
        // call the native `shutdown()`, which the dyn trait does not expose; it
        // also keeps the sweeper's `Weak` alive for the handle's lifetime.
        let cache_dyn: Arc<dyn ClusterCacheBackend> = Arc::new(InstrumentedCache::new(
            Arc::clone(&cache) as Arc<dyn ClusterCacheBackend>,
            PROVIDER_NAME,
            Arc::clone(&metrics),
        ));

        // The remaining three primitives ride the SDK defaults over the cache —
        // the "implement cache only, get all four" guarantee. `new` is the
        // default-safe constructor; it succeeds here because the cache is
        // linearizable. Each is given the `standalone` provider label and the
        // shared metrics sink so it emits the contracted signals.
        let leader: Arc<dyn LeaderElectionBackend> = Arc::new(
            CasBasedLeaderElectionBackend::new(Arc::clone(&cache_dyn))?
                .with_observability(PROVIDER_NAME, Arc::clone(&metrics)),
        );
        let lock: Arc<dyn DistributedLockBackend> = Arc::new(
            CasBasedDistributedLockBackend::new(Arc::clone(&cache_dyn))?
                .with_observability(PROVIDER_NAME, Arc::clone(&metrics)),
        );
        let discovery: Arc<dyn ServiceDiscoveryBackend> = Arc::new(
            CacheBasedServiceDiscoveryBackend::new(Arc::clone(&cache_dyn))
                .with_observability(PROVIDER_NAME, Arc::clone(&metrics)),
        );

        Ok(StandaloneClusterHandle {
            cache,
            cache_dyn,
            leader,
            lock,
            discovery,
            sweeper,
            shutdown,
        })
    }
}

/// The running standalone plugin. Hands its four backends to the wiring crate /
/// `ClientHub` registration, and owns the cache sweeper's lifecycle.
///
/// Accessors return cheap `Arc` clones of the trait objects. Call
/// [`stop`](Self::stop) on graceful shutdown to stop the sweeper; the plugin
/// performs no remote I/O and never relies on `Drop` for cleanup.
pub struct StandaloneClusterHandle {
    /// The concrete cache, retained so [`stop`](Self::stop) can close active
    /// watches via the native [`StandaloneCache::shutdown`] (DESIGN §3.13).
    cache: Arc<StandaloneCache>,
    /// The same cache as a trait object, handed to consumers and the SDK default
    /// backends.
    cache_dyn: Arc<dyn ClusterCacheBackend>,
    leader: Arc<dyn LeaderElectionBackend>,
    lock: Arc<dyn DistributedLockBackend>,
    discovery: Arc<dyn ServiceDiscoveryBackend>,
    sweeper: JoinHandle<()>,
    shutdown: CancellationToken,
}

impl StandaloneClusterHandle {
    /// The native cache backend.
    #[must_use]
    pub fn cache(&self) -> Arc<dyn ClusterCacheBackend> {
        Arc::clone(&self.cache_dyn)
    }

    /// The leader-election backend (SDK default over the native cache).
    #[must_use]
    pub fn leader_election(&self) -> Arc<dyn LeaderElectionBackend> {
        Arc::clone(&self.leader)
    }

    /// The distributed-lock backend (SDK default over the native cache).
    #[must_use]
    pub fn lock(&self) -> Arc<dyn DistributedLockBackend> {
        Arc::clone(&self.lock)
    }

    /// The service-discovery backend (SDK default over the native cache).
    #[must_use]
    pub fn service_discovery(&self) -> Arc<dyn ServiceDiscoveryBackend> {
        Arc::clone(&self.discovery)
    }

    /// Stops the plugin: closes every active cache watch terminally, then cancels
    /// the cache sweeper and waits for it to exit. Consumes the handle.
    ///
    /// The cache `shutdown()` runs **first** so any active watch observes a
    /// terminal `Closed(Shutdown)` (`cpt-cf-clst-fr-shutdown-revoke`, DESIGN
    /// §3.13) before the sweeper stops and the cache is dropped. TTL bounds any
    /// remaining in-flight cluster resources (DESIGN §3.7) — there is no
    /// best-effort remote cleanup.
    pub async fn stop(self) {
        self.cache.shutdown();
        self.shutdown.cancel();
        let _exited = self.sweeper.await;
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use cluster_sdk::{DiscoveryFilter, LeaderStatus, LeaderWatchEvent, ServiceRegistration};

    use super::StandaloneClusterPlugin;

    #[tokio::test]
    async fn build_and_start_wires_all_four_primitives() {
        let handle = StandaloneClusterPlugin::builder()
            .build_and_start()
            .expect("standalone plugin must start");

        // Cache round-trips.
        let cache = handle.cache();
        assert!(cache.put("k", b"v", None).await.is_ok());
        let Ok(Some(entry)) = cache.get("k").await else {
            panic!("value must be present");
        };
        assert_eq!(entry.value, b"v");

        // Lock acquires.
        let lock = handle.lock();
        let guard = lock
            .try_lock("ledger", Duration::from_secs(30))
            .await
            .expect("free lock must acquire");
        assert!(guard.release().await.is_ok());

        // Leader election resolves to leader for a sole candidate.
        let leader = handle.leader_election();
        let mut watch = leader.elect("primary").await.expect("election must join");
        assert!(matches!(
            watch.changed().await,
            LeaderWatchEvent::Status(LeaderStatus::Leader)
        ));

        // Service discovery registers and discovers.
        let discovery = handle.service_discovery();
        let _registered = discovery
            .register(ServiceRegistration {
                name: "delivery".to_owned(),
                instance_id: Some("i-1".to_owned()),
                address: "127.0.0.1:9000".to_owned(),
                metadata: std::collections::HashMap::new(),
            })
            .await
            .expect("registration must succeed");
        let found = discovery
            .discover("delivery", DiscoveryFilter::default())
            .await
            .expect("discover must succeed");
        assert!(
            found.iter().any(|i| i.instance_id == "i-1"),
            "the registered instance must be discoverable"
        );

        handle.stop().await;
    }

    #[tokio::test]
    async fn zero_sweep_interval_is_rejected() {
        let result = StandaloneClusterPlugin::builder()
            .sweep_interval(Duration::ZERO)
            .build_and_start();
        assert!(matches!(
            result,
            Err(cluster_sdk::ClusterError::InvalidConfig { .. })
        ));
    }
}
