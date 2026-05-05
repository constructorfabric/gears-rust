//! Configuration for the no-op usage-collector storage plugin.

use serde::Deserialize;

/// Plugin configuration.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct NoopUsageCollectorStorageConfig {
    /// Vendor name (must match the usage-collector gateway `vendor`).
    pub vendor: String,

    /// Plugin priority (lower = higher priority when multiple instances exist).
    pub priority: i16,
}

impl Default for NoopUsageCollectorStorageConfig {
    fn default() -> Self {
        Self {
            vendor: "cyberfabric".to_owned(),
            priority: 100,
        }
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "config_tests.rs"]
mod config_tests;
