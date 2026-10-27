//! Configuration for the RG tenant resolver plugin.

use resource_group_sdk::TENANT_RG_TYPE_PATH;
use serde::Deserialize;

/// Plugin configuration.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RgTrPluginConfig {
    /// Vendor name for GTS instance registration.
    pub vendor: String,

    /// Plugin priority (lower = higher priority).
    pub priority: i16,

    /// RG type paths whose groups are treated as tenants (matched with
    /// `type in (...)`). Defaults to `[TENANT_RG_TYPE_PATH]`. Every entry
    /// must be a type registered in resource-group: RG rejects a `$filter`
    /// that names an unknown type.
    pub tenant_types: Vec<String>,
}

impl Default for RgTrPluginConfig {
    fn default() -> Self {
        Self {
            vendor: "constructorfabric".to_owned(),
            priority: 50,
            tenant_types: vec![TENANT_RG_TYPE_PATH.to_owned()],
        }
    }
}
