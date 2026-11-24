//! Configuration for `[quota-enforcement-storage-plugin]`.

use std::time::Duration;

use serde::Deserialize;

/// Plugin configuration. Read once at `Gear::init`.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct StoragePluginConfig {
    /// Vendor the gear selects this backend by. Must match
    /// `gears.quota-enforcement.config.storage_vendor`.
    pub vendor: String,
    /// Selection priority among plugins of the same vendor. Lower wins.
    pub priority: i16,
    /// Seconds an operator edit of the configuration tables may take to apply.
    pub config_refresh_secs: u64,
}

impl Default for StoragePluginConfig {
    fn default() -> Self {
        Self {
            vendor: "constructorfabric".to_owned(),
            priority: 100,
            config_refresh_secs: 30,
        }
    }
}

impl StoragePluginConfig {
    /// Reject a configuration that can never be selected.
    ///
    /// # Errors
    ///
    /// Returns an error when `vendor` is blank or `config_refresh_secs` is zero.
    pub fn validate(&self) -> anyhow::Result<()> {
        if self.vendor.trim().is_empty() {
            anyhow::bail!(
                "[quota-enforcement-storage-plugin].vendor must not be empty or whitespace-only"
            );
        }
        if self.config_refresh_secs == 0 {
            anyhow::bail!(
                "[quota-enforcement-storage-plugin].config_refresh_secs must be at least 1"
            );
        }
        Ok(())
    }

    /// How long a snapshot of the configuration tables serves.
    #[must_use]
    pub const fn config_refresh(&self) -> Duration {
        Duration::from_secs(self.config_refresh_secs)
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "config_tests.rs"]
mod config_tests;
