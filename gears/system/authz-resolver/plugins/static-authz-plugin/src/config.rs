// Updated: 2026-10-06 by Constructor Tech
//! Configuration for the static `AuthZ` resolver plugin.

use serde::Deserialize;
use uuid::Uuid;

/// Plugin configuration.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct StaticAuthZPluginConfig {
    /// Vendor name for GTS instance registration.
    pub vendor: String,

    /// Plugin priority (lower = higher priority).
    pub priority: i16,

    /// Property grants: restrict which values of a resource property a caller
    /// gets (default: none, behaviour unchanged).
    pub property_grants: Vec<PropertyGrantConfig>,
}

/// One property-grant rule as written in the configuration.
///
/// When a request matches `resource_type`, `actions` and `subjects`, the
/// emitted constraints are narrowed with `In(property, values)`.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PropertyGrantConfig {
    /// Exact match on `request.resource.resource_type`.
    pub resource_type: String,

    /// PDP property name to constrain (e.g. `secret_type`, `reference`).
    pub property: String,

    /// Actions the rule applies to; empty or absent = every action.
    #[serde(default)]
    pub actions: Vec<String>,

    /// Subject ids (`request.subject.id`) the rule applies to; empty or
    /// absent = every subject.
    #[serde(default)]
    pub subjects: Vec<Uuid>,

    /// Granted values: a UUID, a GTS id (converted to its v5 UUID) or a plain
    /// string. Empty = the rule admits nothing.
    #[serde(default)]
    pub values: Vec<String>,
}

impl Default for StaticAuthZPluginConfig {
    fn default() -> Self {
        Self {
            vendor: "constructorfabric".to_owned(),
            priority: 100,
            property_grants: Vec::new(),
        }
    }
}
