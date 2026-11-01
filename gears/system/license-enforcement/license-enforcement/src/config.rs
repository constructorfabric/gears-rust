//! Configuration of the license-enforcement gear.

use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct LicenseEnforcementConfig {
    /// Vendor of the `LicenseResolverPluginSpecV1` instance; license-resolver
    /// selects its backend by it.
    pub vendor: String,
    /// Priority of that instance among the backends of the same vendor; the
    /// lower value wins. The default, 50, wins over the default of the static
    /// license plugin (100), so that one of them is chosen when both are linked.
    pub priority: i16,
    /// How long the result of a mutating request is kept for replays, from 1
    /// second to [`MAX_IDEMPOTENCY_WINDOW_SECS`].
    pub idempotency_window_secs: i64,
    pub default_page_size: u64,
    pub max_page_size: u64,
    /// Most candidates that one check reads, in candidate order.
    pub max_candidates: u64,
}

/// One year: far below the overflow of `now + window`.
pub const MAX_IDEMPOTENCY_WINDOW_SECS: i64 = 365 * 24 * 60 * 60;

impl LicenseEnforcementConfig {
    /// # Errors
    ///
    /// A message when the window is outside 1 s ..= one year, the page sizes
    /// do not satisfy `1 <= default <= max < u64::MAX`, or `max_candidates` is 0.
    pub fn validate(&self) -> Result<(), String> {
        if !(1..=MAX_IDEMPOTENCY_WINDOW_SECS).contains(&self.idempotency_window_secs) {
            return Err(format!(
                "idempotency_window_secs must be in 1..={MAX_IDEMPOTENCY_WINDOW_SECS}, got {}",
                self.idempotency_window_secs
            ));
        }
        if !(1 <= self.default_page_size
            && self.default_page_size <= self.max_page_size
            && self.max_page_size < u64::MAX)
        {
            return Err(format!(
                "page sizes must satisfy 1 <= default_page_size <= max_page_size < u64::MAX, got {} and {}",
                self.default_page_size, self.max_page_size
            ));
        }
        if self.max_candidates == 0 {
            return Err("max_candidates must be at least 1".to_owned());
        }
        Ok(())
    }
}

impl Default for LicenseEnforcementConfig {
    fn default() -> Self {
        Self {
            vendor: "constructorfabric".to_owned(),
            priority: 50,
            idempotency_window_secs: 24 * 60 * 60,
            default_page_size: 50,
            max_page_size: 1000,
            max_candidates: 100,
        }
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "config_tests.rs"]
mod config_tests;
