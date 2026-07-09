//! Configuration for the usage-collector gear.
//!
//! Carries only the vendor selector used to bind a storage-plugin
//! implementation. Read once at `Gear::init` via `ctx.config_or_default()`;
//! changing the binding requires a gear restart. The usage-type catalog is
//! plugin-owned (ADR-0012 / foundation.md 0.2.0), so no usage-type
//! declarations are accepted here.

use serde::Deserialize;

/// Gear configuration for `[usage-collector]`.
///
/// Read once at `Gear::init` via `ctx.config_or_default()`; changing the
/// binding requires a gear restart.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct UsageCollectorConfig {
    /// Vendor selector used to pick a storage-plugin implementation.
    ///
    /// The host queries types-registry for plugin instances matching this
    /// vendor and selects the one with the lowest priority number — but only
    /// lazily, on the first dispatch. No `types-registry` query happens at
    /// `init`.
    pub vendor: String,
}

impl Default for UsageCollectorConfig {
    fn default() -> Self {
        Self {
            vendor: "cyberfabric".to_owned(),
        }
    }
}

impl UsageCollectorConfig {
    /// Validates the configuration at bootstrap.
    ///
    /// Rejects an empty or whitespace-only `vendor` selector so the failure
    /// surfaces at `Gear::init` rather than lazily on the first dispatch when
    /// plugin selection finds no match.
    ///
    /// # Errors
    ///
    /// Returns an error if `vendor` is empty or whitespace-only.
    pub fn validate(&self) -> anyhow::Result<()> {
        if self.vendor.trim().is_empty() {
            anyhow::bail!("[usage_collector].vendor must not be empty or whitespace-only");
        }
        Ok(())
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "config_tests.rs"]
mod config_tests;
