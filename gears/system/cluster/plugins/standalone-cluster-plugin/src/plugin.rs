//! The standalone cluster plugin's outbox-style builder and lifecycle handle
//! (DESIGN §3.7). The plugin is a library, not a `RunnableCapability`: a parent
//! host gear (or the cluster wiring crate) owns the [`StandaloneClusterHandle`]
//! from its own `start`/`stop`.

use std::sync::Arc;

use cluster_sdk::observability::otel::OtelClusterMetrics;
use cluster_sdk::{ClusterCacheBackend, ClusterError, ClusterMetrics, InstrumentedCache};
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

    /// Builds the plugin: creates the native cache and starts its TTL sweeper.
    ///
    /// The wiring crate (`cf-gears-cluster`) wraps the cache backend with the SDK
    /// default leader-election and lock backends (DESIGN §3.11).
    ///
    /// # Errors
    /// - [`ClusterError::InvalidConfig`] if `sweep_interval` is zero.
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
        // operations emit the contracted `cluster.cache.*` signals. The concrete
        // `Arc<StandaloneCache>` is retained separately for `stop` to call the
        // native `shutdown()`, which the dyn trait does not expose.
        let cache_dyn: Arc<dyn ClusterCacheBackend> = Arc::new(InstrumentedCache::new(
            Arc::clone(&cache) as Arc<dyn ClusterCacheBackend>,
            PROVIDER_NAME,
            Arc::clone(&metrics),
        ));

        Ok(StandaloneClusterHandle {
            cache,
            cache_dyn,
            sweeper,
            shutdown,
        })
    }
}

/// The running standalone plugin. Hands its cache backend to the wiring crate
/// for `ClientHub` registration and wrapping with SDK default backends.
///
/// Call [`stop`](Self::stop) on graceful shutdown.
pub struct StandaloneClusterHandle {
    /// The concrete cache, retained so [`stop`](Self::stop) can close active
    /// watches via the native [`StandaloneCache::shutdown`] (DESIGN §3.13).
    cache: Arc<StandaloneCache>,
    /// The same cache as an instrumented trait object, handed to the wiring crate.
    cache_dyn: Arc<dyn ClusterCacheBackend>,
    sweeper: JoinHandle<()>,
    shutdown: CancellationToken,
}

impl StandaloneClusterHandle {
    /// The instrumented cache backend (to be registered in `ClientHub` and
    /// wrapped with SDK defaults by the wiring crate).
    #[must_use]
    pub fn cache(&self) -> Arc<dyn ClusterCacheBackend> {
        Arc::clone(&self.cache_dyn)
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
        if let Err(join_err) = self.sweeper.await {
            tracing::error!(
                error = %join_err,
                "standalone cluster plugin TTL sweeper task panicked during shutdown"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use cluster_sdk::cache::PutRequest;

    use super::StandaloneClusterPlugin;

    #[tokio::test]
    async fn build_and_start_provides_cache_backend() {
        let handle = StandaloneClusterPlugin::builder()
            .build_and_start()
            .expect("standalone plugin must start");

        let cache = handle.cache();
        assert!(
            cache
                .put(PutRequest {
                    key: "k",
                    value: b"v",
                    ttl: cluster_sdk::cache::Ttl::Indefinite,
                })
                .await
                .is_ok()
        );
        let Ok(Some(entry)) = cache.get("k").await else {
            panic!("value must be present");
        };
        assert_eq!(entry.value, b"v");

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
