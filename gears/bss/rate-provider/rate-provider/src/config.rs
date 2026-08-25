//! Core gear configuration.
//!
//! The core gear composes the registered source plugins (filtered by
//! `source_vendor`) and re-registers the composite as a rate-provider plugin the
//! ledger discovers (advertised under `vendor` + `priority`).

use serde::Deserialize;

/// Config for the core `bss-rate-provider` gear (`config:` block).
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RateProviderConfig {
    /// Vendor this composite advertises for the ledger's plugin selection.
    pub vendor: String,
    /// Priority of this composite instance (lower preferred if several exist).
    pub priority: i16,
    /// Vendor used to select which SOURCE plugins this gear composes.
    pub source_vendor: String,
    /// Id reported by `provider_id()` before the first successful discovery
    /// (never `"none"`, the ledger's unconfigured sentinel).
    pub id: String,
}

impl Default for RateProviderConfig {
    fn default() -> Self {
        Self {
            vendor: "cf.bss".to_owned(),
            priority: 100,
            source_vendor: "cf.bss".to_owned(),
            id: "bss-rate-provider".to_owned(),
        }
    }
}

#[cfg(test)]
#[path = "config_tests.rs"]
mod tests;
