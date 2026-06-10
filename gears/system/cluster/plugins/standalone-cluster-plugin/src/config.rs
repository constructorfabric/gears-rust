// Created: 2026-06-11 by Constructor Tech
//! Configuration for the standalone in-process cluster plugin.

use std::time::Duration;

/// The default cache TTL-sweep cadence. Coarse enough to avoid needless wakeups,
/// fine enough that watch subscribers observe `Expired` events (and the default
/// leader/lock backends observe failover) within roughly this bound after a TTL
/// elapses. Lazy reads already treat an expired entry as absent immediately, so
/// this only bounds *eventing* latency, not correctness.
pub const DEFAULT_SWEEP_INTERVAL: Duration = Duration::from_secs(1);

/// Tunables for the standalone cluster plugin.
///
/// Construct via [`StandaloneClusterConfig::default`] and override fields, or use
/// the builder conveniences on
/// [`StandaloneClusterBuilder`](crate::StandaloneClusterBuilder).
#[derive(Debug, Clone)]
pub struct StandaloneClusterConfig {
    /// How often the cache's background sweeper scans for and evicts expired
    /// entries, emitting an `Expired` event to matching watchers. Must be
    /// non-zero — [`build_and_start`](crate::StandaloneClusterBuilder::build_and_start)
    /// rejects a zero interval.
    pub sweep_interval: Duration,
}

impl Default for StandaloneClusterConfig {
    fn default() -> Self {
        Self {
            sweep_interval: DEFAULT_SWEEP_INTERVAL,
        }
    }
}
