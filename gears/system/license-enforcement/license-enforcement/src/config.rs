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
    /// Most candidates that one check reads, in candidate order, from 1 to
    /// [`MAX_CANDIDATES`].
    pub max_candidates: u64,
    /// How long terminated grants are kept, in days, at least 1 and at least
    /// the idempotency window: a replay reads the grants of its first answer.
    pub retention_days: i64,
    /// Seconds between two runs of the retention sweeper and the cleanup of
    /// expired idempotency records, from 1 to [`MAX_SWEEP_INTERVAL_SECS`].
    pub sweep_interval_secs: u64,
}

/// A hundred years: far below the overflow of `now - retention`.
pub const MAX_RETENTION_DAYS: i64 = 36_500;

/// Keeps the `IN` list of the root ids that one check reads far below the
/// bind-parameter limit of every backend.
pub const MAX_CANDIDATES: u64 = 1000;

/// One week: a longer pause leaves expired idempotency records too long.
pub const MAX_SWEEP_INTERVAL_SECS: u64 = 7 * 24 * 60 * 60;

/// One year: far below the overflow of `now + window`.
pub const MAX_IDEMPOTENCY_WINDOW_SECS: i64 = 365 * 24 * 60 * 60;

impl LicenseEnforcementConfig {
    /// # Errors
    ///
    /// A message when the window is outside 1 s ..= one year, the page sizes
    /// do not satisfy `1 <= default <= max < u64::MAX`, `max_candidates` is outside 1 ..= 1000,
    /// `retention_days` is outside 1 ..= 36 500 or shorter than the window, or
    /// `sweep_interval_secs` is outside 1 s ..= one week.
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
        if !(1..=MAX_CANDIDATES).contains(&self.max_candidates) {
            return Err(format!(
                "max_candidates must be in 1..={MAX_CANDIDATES}, got {}",
                self.max_candidates
            ));
        }
        if !(1..=MAX_RETENTION_DAYS).contains(&self.retention_days) {
            return Err(format!(
                "retention_days must be in 1..={MAX_RETENTION_DAYS}, got {}",
                self.retention_days
            ));
        }
        if self.retention_days * 24 * 60 * 60 < self.idempotency_window_secs {
            return Err(format!(
                "retention_days * 86400 must be at least idempotency_window_secs, got {} * 86400 < {}",
                self.retention_days, self.idempotency_window_secs
            ));
        }
        if !(1..=MAX_SWEEP_INTERVAL_SECS).contains(&self.sweep_interval_secs) {
            return Err(format!(
                "sweep_interval_secs must be in 1..={MAX_SWEEP_INTERVAL_SECS}, got {}",
                self.sweep_interval_secs
            ));
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
            retention_days: 400,
            sweep_interval_secs: 60 * 60,
        }
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "config_tests.rs"]
mod config_tests;
