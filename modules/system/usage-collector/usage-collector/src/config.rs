//! Configuration for the usage-collector gateway module.

use std::collections::HashMap;
use std::time::Duration;

use serde::Deserialize;
use usage_collector_sdk::UsageKind;
use usage_emitter::UsageEmitterConfig;

/// Default number of raw records returned per page when `page_size` is absent.
pub const DEFAULT_PAGE_SIZE: usize = 100;

/// Maximum allowed value for `page_size` in raw queries.
pub const MAX_PAGE_SIZE: usize = 1_000;

/// Maximum number of rows `query_aggregated` may return before returning `QueryResultTooLarge`.
pub const MAX_AGG_ROWS: usize = 10_000;

/// Maximum byte length for string filter fields (`usage_type`, `resource_type`, `subject_type`, `source`).
pub const MAX_FILTER_STRING_LEN: usize = 256;

/// Maximum allowed query time range (from, to) per request (~1 year).
pub const MAX_QUERY_TIME_RANGE: Duration = Duration::from_hours(8784);

/// Per-metric allowed configuration.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MetricConfig {
    /// Gauge vs counter semantics.
    pub kind: UsageKind,
    /// Modules allowed to emit this metric. If absent, all modules are allowed.
    pub modules: Option<Vec<String>>,
}

/// Module configuration.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct UsageCollectorConfig {
    /// Vendor selector used to pick a storage plugin instance from types-registry.
    pub vendor: String,

    /// Timeout for each storage plugin `create_usage_record()` call.
    /// Valid range: 100ms–30s. Default: 5s.
    #[serde(with = "modkit_utils::humantime_serde")]
    pub plugin_timeout: Duration,

    /// Number of failures within `circuit_breaker_window` that will open the circuit.
    /// Valid range: 1–100. Default: 5.
    pub circuit_breaker_failure_threshold: u32,

    /// Rolling window for counting failures.
    /// Default: 10s.
    #[serde(with = "modkit_utils::humantime_serde")]
    pub circuit_breaker_window: Duration,

    /// Duration to wait in the open state before allowing a half-open probe.
    /// Valid range: 1s–5m. Default: 30s.
    #[serde(with = "modkit_utils::humantime_serde")]
    pub circuit_breaker_recovery_timeout: Duration,

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
        if self.circuit_breaker_failure_threshold < 1 {
            anyhow::bail!("circuit_breaker_failure_threshold must be at least 1");
        }
        if self.circuit_breaker_failure_threshold > 100 {
            anyhow::bail!("circuit_breaker_failure_threshold must not exceed 100");
        }
        if self.circuit_breaker_window < std::time::Duration::from_millis(100) {
            anyhow::bail!("circuit_breaker_window must be at least 100ms");
        }
        if self.circuit_breaker_recovery_timeout < std::time::Duration::from_secs(1) {
            anyhow::bail!("circuit_breaker_recovery_timeout must be at least 1s");
        }
        if self.circuit_breaker_recovery_timeout > std::time::Duration::from_mins(5) {
            anyhow::bail!("circuit_breaker_recovery_timeout must not exceed 5min");
        }
        // Startup invariants for query configuration constants.
        #[allow(clippy::assertions_on_constants)]
        {
            assert!(
                DEFAULT_PAGE_SIZE > 0,
                "DEFAULT_PAGE_SIZE must be greater than zero"
            );
            assert!(MAX_PAGE_SIZE > 0, "MAX_PAGE_SIZE must be greater than zero");
            assert!(
                DEFAULT_PAGE_SIZE <= MAX_PAGE_SIZE,
                "DEFAULT_PAGE_SIZE must not exceed MAX_PAGE_SIZE"
            );
            assert!(
                MAX_FILTER_STRING_LEN > 0,
                "MAX_FILTER_STRING_LEN must be greater than zero"
            );
        }
        Ok(())
    }
}

impl Default for UsageCollectorConfig {
    fn default() -> Self {
        Self {
            vendor: "cyberfabric".to_owned(),
            plugin_timeout: Duration::from_secs(5),
            circuit_breaker_failure_threshold: 5,
            circuit_breaker_window: Duration::from_secs(10),
            circuit_breaker_recovery_timeout: Duration::from_secs(30),
            emitter: UsageEmitterConfig::default(),
            metrics: HashMap::new(),
        }
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "config_tests.rs"]
mod config_tests;
