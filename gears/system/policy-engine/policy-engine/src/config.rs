//! Gear configuration.

use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::domain::model::ContentLimits;

/// Configuration of the `policy-engine` gear (besides its database section).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PolicyEngineConfig {
    /// How the engine registers as an admission engine plugin.
    pub engine_plugin: EnginePluginConfig,
    /// Wall-clock budget for evaluating all documents of one request.
    pub evaluation_timeout_ms: u64,
    /// Bound on each tenant-hierarchy call.
    pub hierarchy_timeout_ms: u64,
    /// Bound on the types-registry lookup at validation time.
    pub registry_timeout_ms: u64,
    /// Compiled versions kept in memory.
    pub compile_cache_capacity: usize,
    /// Most documents in one version.
    pub max_documents_per_version: usize,
    /// Most content bytes in one document.
    pub max_document_bytes: usize,
}

/// Admission engine plugin registration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct EnginePluginConfig {
    /// Vendor the plugin registers under.
    pub vendor: String,
    /// Plugin priority; the lowest value wins among engines.
    pub priority: i16,
}

impl Default for EnginePluginConfig {
    fn default() -> Self {
        Self {
            vendor: "constructorfabric".to_owned(),
            priority: 100,
        }
    }
}

impl Default for PolicyEngineConfig {
    fn default() -> Self {
        Self {
            engine_plugin: EnginePluginConfig::default(),
            evaluation_timeout_ms: 5,
            hierarchy_timeout_ms: 20,
            registry_timeout_ms: 500,
            compile_cache_capacity: 256,
            max_documents_per_version: 256,
            max_document_bytes: 65_536,
        }
    }
}

/// A configuration value the gear refuses to start with.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ConfigError {
    /// A numeric value that must be positive is zero.
    #[error("`{field}` must be greater than zero")]
    NotPositive {
        /// The offending key.
        field: &'static str,
    },
    /// `engine_plugin.vendor` is empty.
    #[error("`engine_plugin.vendor` must not be blank")]
    EnginePluginVendorBlank,
}

impl PolicyEngineConfig {
    /// Checks the configuration.
    ///
    /// # Errors
    ///
    /// [`ConfigError`] for a zero bound or a blank vendor.
    pub fn validate(&self) -> Result<(), ConfigError> {
        let bounds: [(&'static str, usize); 6] = [
            (
                "evaluation_timeout_ms",
                to_usize(self.evaluation_timeout_ms),
            ),
            ("hierarchy_timeout_ms", to_usize(self.hierarchy_timeout_ms)),
            ("registry_timeout_ms", to_usize(self.registry_timeout_ms)),
            ("compile_cache_capacity", self.compile_cache_capacity),
            ("max_documents_per_version", self.max_documents_per_version),
            ("max_document_bytes", self.max_document_bytes),
        ];
        if let Some((field, _)) = bounds.iter().find(|(_, v)| *v == 0) {
            return Err(ConfigError::NotPositive { field });
        }
        if self.engine_plugin.vendor.trim().is_empty() {
            return Err(ConfigError::EnginePluginVendorBlank);
        }
        Ok(())
    }

    /// The evaluation budget of one request.
    #[must_use]
    pub const fn evaluation_timeout(&self) -> Duration {
        Duration::from_millis(self.evaluation_timeout_ms)
    }

    /// The bound on each tenant-hierarchy call.
    #[must_use]
    pub const fn hierarchy_timeout(&self) -> Duration {
        Duration::from_millis(self.hierarchy_timeout_ms)
    }

    /// The bound on the types-registry lookup.
    #[must_use]
    pub const fn registry_timeout(&self) -> Duration {
        Duration::from_millis(self.registry_timeout_ms)
    }

    /// The content limits enforced on write and on validation.
    #[must_use]
    pub const fn content_limits(&self) -> ContentLimits {
        ContentLimits {
            max_documents_per_version: self.max_documents_per_version,
            max_document_bytes: self.max_document_bytes,
        }
    }
}

fn to_usize(value: u64) -> usize {
    usize::try_from(value).unwrap_or(usize::MAX)
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "config_tests.rs"]
mod config_tests;
