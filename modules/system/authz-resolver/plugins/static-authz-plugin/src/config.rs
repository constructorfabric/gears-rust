//! Configuration for the static `AuthZ` resolver plugin.

use serde::Deserialize;

/// Plugin configuration.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct StaticAuthZPluginConfig {
    /// Vendor name for GTS instance registration.
    pub vendor: String,

    /// Plugin priority (lower = higher priority).
    pub priority: i16,
}

impl Default for StaticAuthZPluginConfig {
    fn default() -> Self {
        Self {
            vendor: "cyberfabric".to_owned(),
            priority: 100,
        }
    }
}
