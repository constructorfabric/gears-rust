//! Configuration for the license resolver gear.

use std::collections::HashMap;

use gts::GtsTypeId;
use serde::Deserialize;

/// Gear configuration.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LicenseResolverConfig {
    /// Vendor selector used to pick a backend plugin implementation.
    ///
    /// The gear queries types-registry for plugin instances matching this
    /// vendor and selects the one with the lowest priority.
    pub vendor: String,

    /// Subject contract type for each principal type, used for a principal
    /// subject. A principal type without an entry takes the entry of its
    /// nearest ancestor in types-registry; with none, it is not licensed.
    pub principal_subject_types: HashMap<String, GtsTypeId>,
}

impl Default for LicenseResolverConfig {
    fn default() -> Self {
        Self {
            vendor: "constructorfabric".to_owned(),
            principal_subject_types: HashMap::new(),
        }
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "config_tests.rs"]
mod config_tests;
