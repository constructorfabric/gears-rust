//! One monotonic shutdown budget shared by dispatch and all activity workers.
use std::{future::Future, sync::OnceLock, time::Duration};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

/// Shutdown timeout split into a runtime deadline and an earlier cleanup deadline.
pub struct ShutdownBudget {
    started: OnceLock<Instant>,
    runtime: Duration,
    cleanup: Duration,
}

impl ShutdownBudget {
    /// Ends runtime one reserve and cleanup two reserves before `total`,
    /// where the reserve is the lesser of 5 seconds and `total / 6`.
    pub(crate) fn new(total: Duration) -> Self {
        let reserve = Duration::from_secs(5).min(total / 6);
        Self {
            started: OnceLock::new(),
            runtime: total.saturating_sub(reserve),
            cleanup: total.saturating_sub(reserve * 2),
        }
    }

    /// Records the shutdown start on first call and returns it.
    pub(crate) fn begin(&self) -> Instant {
        *self.started.get_or_init(Instant::now)
    }

    /// Returns the deadline for in-flight work, starting the budget if needed.
    pub(crate) fn runtime_deadline(&self) -> Instant {
        self.begin() + self.runtime
    }

    /// Returns the earlier of `lease` and the cleanup deadline.
    /// An unstarted budget measures the cleanup deadline from now.
    pub(crate) fn cleanup_deadline(&self, lease: Instant) -> Instant {
        let start = self.started.get().copied().unwrap_or_else(Instant::now);
        lease.min(start + self.cleanup)
    }

    /// Bound even pre-claim and post-handler I/O after shutdown. Dropping an
    /// ambiguous write leaves its fenced claim for recovery.
    pub(crate) async fn bound<F: Future>(
        &self,
        stop: &CancellationToken,
        future: F,
    ) -> Result<F::Output, tokio::time::error::Elapsed> {
        tokio::pin!(future);
        tokio::select! {
            biased;
            () = stop.cancelled() => tokio::time::timeout_at(self.runtime_deadline(), future).await,
            result = &mut future => Ok(result),
        }
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "../../tests/unit/shutdown_tests.rs"]
mod tests;
