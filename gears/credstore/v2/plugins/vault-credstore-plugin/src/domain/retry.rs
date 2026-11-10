// Created: 2026-10-04 by Constructor Tech
// Updated: 2026-10-06 by Constructor Tech
//! Retry policy for idempotent backend calls: a bounded number of attempts
//! with exponential backoff.
//!
//! Only the caller decides *what* may be retried (`get`, `delete_key`,
//! `destroy` and the metadata read inside it; never `put`). This type decides
//! *how often* and *how long to wait*.
use std::time::Duration;

use toolkit_macros::domain_model;
use uuid::Uuid;

use crate::config::{MAX_RETRY_DELAY_MS, RetryConfig};

/// How many times an operation is attempted and how long to pause in between.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetryPolicy {
    max_attempts: u32,
    base_delay: Duration,
}

impl RetryPolicy {
    /// Builds the policy from the validated `retry` block of the config.
    #[must_use]
    pub fn new(cfg: &RetryConfig) -> Self {
        Self {
            max_attempts: cfg.max_attempts.max(1),
            base_delay: Duration::from_millis(cfg.base_delay_ms.min(MAX_RETRY_DELAY_MS)),
        }
    }

    /// A policy that never retries: one attempt, no waiting. Used for `put`.
    #[must_use]
    pub fn no_retry() -> Self {
        Self {
            max_attempts: 1,
            base_delay: Duration::ZERO,
        }
    }

    /// Total number of attempts, the first one included.
    #[must_use]
    pub fn max_attempts(&self) -> u32 {
        self.max_attempts
    }

    /// Upper bound of the pause after `failed_attempts` consecutive failures
    /// (`1` = the first attempt failed): the base delay doubled for every
    /// further failure, never more than [`MAX_RETRY_DELAY_MS`].
    #[must_use]
    pub fn backoff(&self, failed_attempts: u32) -> Duration {
        let cap = Duration::from_millis(MAX_RETRY_DELAY_MS);
        let doublings = failed_attempts.saturating_sub(1).min(16);
        self.base_delay
            .checked_mul(1_u32 << doublings)
            .map_or(cap, |d| d.min(cap))
    }

    /// The pause actually taken after `failed_attempts` failures: a random
    /// point in the upper half of [`Self::backoff`], so replicas that failed
    /// together do not retry in lockstep.
    #[must_use]
    pub fn jittered_backoff(&self, failed_attempts: u32) -> Duration {
        let upper = self.backoff(failed_attempts);
        let half = upper / 2;
        let span_us = u64::try_from(half.as_micros()).unwrap_or(u64::MAX);
        if span_us == 0 {
            return upper;
        }
        // `Uuid::new_v4` is the crate's source of randomness (no extra
        // dependency); only its low 64 bits are used.
        let noise = Uuid::new_v4().as_u64_pair().1;
        half + Duration::from_micros(noise % (span_us + 1))
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "retry_tests.rs"]
mod retry_tests;
