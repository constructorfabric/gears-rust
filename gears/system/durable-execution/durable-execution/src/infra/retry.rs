//! Bounded database retry shared by worker and metadata-only control planes.
use std::time::Duration;

use tokio_util::sync::CancellationToken;

use crate::{config::Config, infra::storage::StoreError};

pub struct RetryPolicy {
    failures: u32,
    limit: u32,
    base: Duration,
    next_delay: Duration,
    max_delay: Duration,
}

impl RetryPolicy {
    pub(crate) fn new(config: &Config) -> Self {
        let base = Duration::from_secs(config.dispatch_interval_secs);
        Self {
            failures: 0,
            limit: config.control_plane_failure_limit,
            base,
            next_delay: base,
            max_delay: base.max(Duration::from_secs(60)),
        }
    }

    pub(crate) fn reset(&mut self) {
        self.failures = 0;
        self.next_delay = self.base;
    }

    /// Only database failures are retryable. Authorization and per-run races
    /// have their own pause/skip semantics in the calling loop.
    pub(crate) fn retry_delay(&mut self, error: &StoreError) -> Option<Duration> {
        let kind = match error {
            StoreError::Database(_) => "database",
            StoreError::Scope(toolkit_db::secure::ScopeError::Db(_)) => "scoped_database",
            _ => return None,
        };
        self.failures = self.failures.saturating_add(1);
        if self.failures >= self.limit {
            tracing::error!(
                kind,
                failures = self.failures,
                limit = self.limit,
                "durable control plane database retry limit reached"
            );
            return None;
        }
        let delay = self.next_delay;
        self.next_delay = delay.saturating_mul(2).min(self.max_delay);
        tracing::warn!(
            kind,
            failures = self.failures,
            limit = self.limit,
            retry_delay_secs = delay.as_secs(),
            "durable control plane database operation will retry"
        );
        Some(delay)
    }
}

/// Return false when shutdown interrupts the delay.
pub async fn wait_retry(delay: Duration, shutdown: &CancellationToken) -> bool {
    tokio::select! {
        biased;
        () = shutdown.cancelled() => false,
        () = tokio::time::sleep(delay) => true,
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "../../tests/unit/retry_tests.rs"]
mod tests;
