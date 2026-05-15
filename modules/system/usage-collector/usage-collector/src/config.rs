//! Configuration for the usage-collector gateway module.

use std::collections::HashMap;
use std::time::Duration;

use serde::Deserialize;
use usage_collector_sdk::UsageKind;
use usage_emitter::UsageEmitterConfig;

/// Per-metric allowed configuration.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MetricConfig {
    /// Gauge vs counter semantics.
    pub kind: UsageKind,
    /// Modules allowed to emit this metric. If absent, all modules are allowed.
    pub modules: Option<Vec<String>>,
}

/// Sliding-window circuit-breaker tuning for the storage plugin proxy.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CircuitBreakerConfig {
    /// Number of failures within `window` that will open the circuit.
    /// Valid range: 1–100. Default: 5.
    pub failure_threshold: u32,

    /// Rolling window for counting failures.
    /// Default: 10s.
    #[serde(with = "modkit_utils::humantime_serde")]
    pub window: Duration,

    /// Duration to wait in the open state before allowing a half-open probe.
    /// Valid range: 1s–5m. Default: 30s.
    #[serde(with = "modkit_utils::humantime_serde")]
    pub recovery_timeout: Duration,
}

impl Default for CircuitBreakerConfig {
    fn default() -> Self {
        Self {
            failure_threshold: 5,
            window: Duration::from_secs(10),
            recovery_timeout: Duration::from_secs(30),
        }
    }
}

/// Module configuration.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct UsageCollectorConfig {
    /// Vendor selector used to pick a storage plugin instance from types-registry.
    pub vendor: String,

    /// Maximum serialized size, in bytes, of a `UsageRecord.metadata` payload.
    ///
    /// Published to emitters via `get_module_config` and enforced there. Default:
    /// `8192`. Upper bound: `1_048_576` (1 MiB). A value of `0` disables metadata
    /// entirely — any non-None metadata payload will be rejected by the emitter.
    pub max_metadata_bytes: u32,

    /// Timeout for each storage plugin `create_usage_record()` call.
    /// Valid range: 100ms–30s. Default: 5s.
    #[serde(with = "modkit_utils::humantime_serde")]
    pub plugin_timeout: Duration,

    /// Sliding-window circuit-breaker tuning for the storage plugin proxy.
    pub circuit_breaker: CircuitBreakerConfig,

    /// Outbox/authorization tuning for the embedded usage emitter.
    pub emitter: UsageEmitterConfig,

    /// Allowed metrics configuration. Key is the metric name.
    pub metrics: HashMap<String, MetricConfig>,
}

impl UsageCollectorConfig {
    /// Validate operational bounds for the gateway configuration.
    ///
    /// # Errors
    ///
    /// Returns an error when any timeout or circuit-breaker setting falls outside
    /// the supported range.
    pub fn validate(&self) -> anyhow::Result<()> {
        if self.plugin_timeout < std::time::Duration::from_millis(100) {
            anyhow::bail!("plugin_timeout must be at least 100ms");
        }
        if self.plugin_timeout > std::time::Duration::from_secs(30) {
            anyhow::bail!("plugin_timeout must not exceed 30s");
        }
        if self.circuit_breaker.failure_threshold < 1 {
            anyhow::bail!("circuit_breaker.failure_threshold must be at least 1");
        }
        if self.circuit_breaker.failure_threshold > 100 {
            anyhow::bail!("circuit_breaker.failure_threshold must not exceed 100");
        }
        if self.circuit_breaker.window < std::time::Duration::from_millis(100) {
            anyhow::bail!("circuit_breaker.window must be at least 100ms");
        }
        if self.circuit_breaker.recovery_timeout < std::time::Duration::from_secs(1) {
            anyhow::bail!("circuit_breaker.recovery_timeout must be at least 1s");
        }
        if self.circuit_breaker.recovery_timeout > std::time::Duration::from_mins(5) {
            anyhow::bail!("circuit_breaker.recovery_timeout must not exceed 5min");
        }
        self.emitter.validate()?;
        anyhow::ensure!(
            self.max_metadata_bytes <= 1_048_576,
            "max_metadata_bytes must not exceed 1 MiB (1_048_576 bytes)"
        );
        Ok(())
    }
}

impl Default for UsageCollectorConfig {
    fn default() -> Self {
        Self {
            vendor: "cyberfabric".to_owned(),
            max_metadata_bytes: 8192,
            plugin_timeout: Duration::from_secs(5),
            circuit_breaker: CircuitBreakerConfig::default(),
            emitter: UsageEmitterConfig::default(),
            metrics: HashMap::new(),
        }
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "config_tests.rs"]
mod config_tests;
