// Created: 2026-06-16 by Constructor Tech
//! The [`ClusterCacheProvider`] implementation for the standalone backend.
//!
//! This is the production glue the wiring crate dispatches to when an operator
//! binds a cache to `provider: standalone`. It implements the SDK trait — so this
//! crate depends on `cluster-sdk` only, never on the wiring crate — and builds
//! the native in-process cache, owning its TTL sweeper via the returned
//! [`StopHook`].

use std::sync::Arc;
use std::time::Duration;

use cluster_sdk::{ClusterCacheBackend, ClusterCacheProvider, ClusterError, StopHook};

use crate::plugin::StandaloneClusterPlugin;

/// The operator config `provider` name that selects the standalone backend.
pub const PROVIDER_NAME: &str = "standalone";

/// Builds the standalone in-process cache backend from operator config.
///
/// Recognized options (Design A — flattened into the backend binding):
/// - `sweep_interval_ms` (integer): the cache TTL-sweep cadence in milliseconds.
///   Omitted → the plugin default. Zero is rejected by the builder.
pub struct StandaloneCacheProvider;

impl ClusterCacheProvider for StandaloneCacheProvider {
    fn provider(&self) -> &'static str {
        PROVIDER_NAME
    }

    fn build_cache(
        &self,
        options: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<(Arc<dyn ClusterCacheBackend>, StopHook), ClusterError> {
        let mut builder = StandaloneClusterPlugin::builder();
        if let Some(value) = options.get("sweep_interval_ms") {
            // Present-but-malformed is an operator error, not a silent fallback.
            let ms = value.as_u64().ok_or_else(|| ClusterError::InvalidConfig {
                reason: format!(
                    "standalone: `sweep_interval_ms` must be a non-negative integer, got `{value}`"
                ),
            })?;
            builder = builder.sweep_interval(Duration::from_millis(ms));
        }

        let handle = builder.build_and_start()?;
        let cache = handle.cache();
        let stop: StopHook = Box::new(move || Box::pin(async move { handle.stop().await }));
        Ok((cache, stop))
    }
}

#[cfg(test)]
#[path = "provider_tests.rs"]
mod provider_tests;
