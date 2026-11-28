//! Configuration for the `AuthZ` resolver.

use std::time::Duration;

use serde::Deserialize;

/// Configuration.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AuthZResolverConfig {
    /// Vendor selector used to pick a plugin implementation.
    pub vendor: String,
    /// Bounds of batch evaluation (`POST /authz-resolver/v1/evaluations`).
    pub batch: BatchConfig,
}

impl Default for AuthZResolverConfig {
    fn default() -> Self {
        Self {
            vendor: "constructorfabric".to_owned(),
            batch: BatchConfig::default(),
        }
    }
}

impl AuthZResolverConfig {
    /// Checks every bound.
    ///
    /// # Errors
    ///
    /// The first out-of-range key, named in the error.
    pub fn validate(&self) -> Result<(), ConfigError> {
        self.batch.validate()
    }
}

/// Bounds of batch evaluation. A batch that exceeds the item limit, the size
/// limit or the deadline fails as a whole; concurrency bounds only delay item
/// evaluations.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct BatchConfig {
    /// Most items in one batch, counted before default values are applied
    /// (`1..=1000`). Exceeding it fails with `400`.
    pub max_items: usize,
    /// Request body limit of the batch route, in bytes. Exceeding it fails
    /// with `413`.
    pub max_body_bytes: usize,
    /// Item evaluations in flight per batch (`1..=64`).
    pub max_concurrency: usize,
    /// Item evaluations in flight across all batches in the process; at least
    /// `max_concurrency`, at most [`MAX_BATCH_IN_FLIGHT`].
    pub max_in_flight: usize,
    /// Whole-batch deadline, measured from request arrival. Expiry fails with
    /// `503`.
    #[serde(with = "toolkit_utils::humantime_serde")]
    pub deadline: Duration,
}

impl Default for BatchConfig {
    fn default() -> Self {
        Self {
            max_items: 100,
            max_body_bytes: 1024 * 1024,
            max_concurrency: 8,
            max_in_flight: 64,
            deadline: Duration::from_secs(20),
        }
    }
}

/// Upper bound of [`BatchConfig::max_items`]: the SDK's decoding cap.
pub const MAX_BATCH_ITEMS: usize = authz_resolver_sdk::MAX_BATCH_ITEMS;
/// Upper bound of [`BatchConfig::max_concurrency`].
pub const MAX_BATCH_CONCURRENCY: usize = 64;
/// Upper bound of [`BatchConfig::max_in_flight`].
pub const MAX_BATCH_IN_FLIGHT: usize = 4096;

impl BatchConfig {
    /// Checks every bound.
    ///
    /// # Errors
    ///
    /// The first out-of-range key, named in the error.
    pub fn validate(&self) -> Result<(), ConfigError> {
        if !(1..=MAX_BATCH_ITEMS).contains(&self.max_items) {
            return Err(ConfigError::new(
                "max_items",
                format!("must be in 1..={MAX_BATCH_ITEMS}, got {}", self.max_items),
            ));
        }
        if self.max_body_bytes == 0 {
            return Err(ConfigError::new(
                "max_body_bytes",
                "must be positive".to_owned(),
            ));
        }
        if !(1..=MAX_BATCH_CONCURRENCY).contains(&self.max_concurrency) {
            return Err(ConfigError::new(
                "max_concurrency",
                format!(
                    "must be in 1..={MAX_BATCH_CONCURRENCY}, got {}",
                    self.max_concurrency
                ),
            ));
        }
        if self.max_in_flight < self.max_concurrency || self.max_in_flight > MAX_BATCH_IN_FLIGHT {
            return Err(ConfigError::new(
                "max_in_flight",
                format!(
                    "must be in max_concurrency ({})..={MAX_BATCH_IN_FLIGHT}, got {}",
                    self.max_concurrency, self.max_in_flight
                ),
            ));
        }
        if self.deadline.is_zero() {
            return Err(ConfigError::new("deadline", "must be positive".to_owned()));
        }
        Ok(())
    }
}

/// An out-of-range configuration value.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("authz-resolver config: batch.{key} {reason}")]
pub struct ConfigError {
    key: &'static str,
    reason: String,
}

impl ConfigError {
    fn new(key: &'static str, reason: String) -> Self {
        Self { key, reason }
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "config_tests.rs"]
mod config_tests;
