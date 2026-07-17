//! ECB source-plugin configuration.

use serde::Deserialize;

/// Config for the ECB source plugin (`config:` block under `bss-rate-provider-ecb-plugin:`).
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct EcbPluginConfig {
    /// Stable `provider_id` stamped on synced ledger rows (design O-8 default "ecb").
    pub id: String,
    /// Plugin-selection vendor (must match the core gear's configured vendor).
    pub vendor: String,
    /// Fallback order across sources: lower `priority` is tried first by the composite.
    pub priority: i16,
    /// ECB daily reference-rate feed URL (MUST be https).
    pub base_url: String,
    /// Outbound per-attempt HTTP timeout in milliseconds.
    pub timeout_ms: u64,
}

impl Default for EcbPluginConfig {
    fn default() -> Self {
        Self {
            id: "ecb".to_owned(),
            vendor: "cf.bss".to_owned(),
            priority: 100,
            base_url: "https://www.ecb.europa.eu/stats/eurofxref/eurofxref-daily.xml".to_owned(),
            timeout_ms: 5000,
        }
    }
}
