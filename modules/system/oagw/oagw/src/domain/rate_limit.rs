use std::collections::HashSet;
use std::time::Instant;

use crate::domain::error::DomainError;
use crate::domain::model::{RateLimitConfig, Window};
use dashmap::DashMap;
use modkit_macros::domain_model;

#[domain_model]
pub struct RateLimiter {
    buckets: DashMap<String, TokenBucket>,
}

#[domain_model]
struct TokenBucket {
    capacity: f64,
    tokens: f64,
    refill_rate: f64, // tokens per second
    last_refill: Instant,
}

impl TokenBucket {
    fn new(config: &RateLimitConfig) -> Self {
        let capacity = config
            .burst
            .as_ref()
            .map_or(config.sustained.rate as f64, |b| b.capacity as f64);
        let window_secs = window_to_secs(&config.sustained.window);
        let refill_rate = config.sustained.rate as f64 / window_secs;
        Self {
            capacity,
            tokens: capacity,
            refill_rate,
            last_refill: Instant::now(),
        }
    }

    fn refill(&mut self) {
        let now = Instant::now();
        let elapsed = now.duration_since(self.last_refill).as_secs_f64();
        self.tokens = (self.tokens + elapsed * self.refill_rate).min(self.capacity);
        self.last_refill = now;
    }

    fn try_consume(&mut self, cost: f64) -> bool {
        self.refill();
        if self.tokens >= cost {
            self.tokens -= cost;
            true
        } else {
            false
        }
    }

    fn retry_after_secs(&self, cost: f64) -> u64 {
        if self.refill_rate <= 0.0 {
            return 60;
        }
        let needed = cost - self.tokens;
        if needed <= 0.0 {
            return 0;
        }
        (needed / self.refill_rate).ceil() as u64
    }
}

fn window_to_secs(window: &Window) -> f64 {
    match window {
        Window::Second => 1.0,
        Window::Minute => 60.0,
        Window::Hour => 3600.0,
        Window::Day => 86400.0,
    }
}

impl RateLimiter {
    #[must_use]
    pub fn new() -> Self {
        Self {
            buckets: DashMap::new(),
        }
    }

    /// Remove all entries whose keys are not in `active_keys`.
    #[allow(dead_code)]
    pub fn purge_keys(&self, active_keys: &HashSet<String>) {
        self.buckets.retain(|k, _| active_keys.contains(k));
    }

    /// Remove a single rate-limit bucket by key.
    ///
    /// Called when an upstream or route is deleted so the stale bucket
    /// does not linger in memory.
    pub fn remove_key(&self, key: &str) {
        self.buckets.remove(key);
    }

    /// Try to consume tokens for the given key.
    ///
    /// # Errors
    /// Returns `DomainError::RateLimitExceeded` with Retry-After seconds when exhausted.
    pub fn try_consume(
        &self,
        key: &str,
        config: &RateLimitConfig,
        instance_uri: &str,
    ) -> Result<(), DomainError> {
        let cost = config.cost as f64;
        let mut bucket = self
            .buckets
            .entry(key.to_string())
            .or_insert_with(|| TokenBucket::new(config));

        if bucket.try_consume(cost) {
            Ok(())
        } else {
            let retry_after = bucket.retry_after_secs(cost);
            Err(DomainError::RateLimitExceeded {
                detail: format!("rate limit exceeded for key: {key}"),
                instance: instance_uri.to_string(),
                retry_after_secs: Some(retry_after),
            })
        }
    }
}

#[cfg(test)]
#[path = "rate_limit_tests.rs"]
mod rate_limit_tests;
