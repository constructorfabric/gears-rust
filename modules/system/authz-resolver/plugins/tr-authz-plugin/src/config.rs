//! Configuration for the TR `AuthZ` resolver plugin.

use serde::Deserialize;

/// Plugin configuration.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TrAuthZPluginConfig {
    /// Vendor name for GTS instance registration.
    pub vendor: String,

    /// Plugin priority (lower = higher priority).
    pub priority: i16,
}

impl Default for TrAuthZPluginConfig {
    fn default() -> Self {
        Self {
            vendor: "hyperspot".to_owned(),
            priority: 50,
        }
    }
}
