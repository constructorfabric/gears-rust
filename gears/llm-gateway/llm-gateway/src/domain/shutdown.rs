//! Graceful shutdown signal (DESIGN §3.6 / §4.5 Graceful shutdown): one
//! source of truth for "the gear is stopping" and "the drain deadline passed".
//!
//! [`ShutdownSignal::begin`] (the gear's stop signal) makes the Service stop
//! accepting requests. The signal's [`ShutdownSignal::cut`] token fires once
//! the cut delay passed after `begin` (`ShutdownConfig::cut_after`): requests
//! still running are cut with `gateway_unavailable`, and their leases are
//! settled before the Usage Reporter's quiescence wait ends.

use std::sync::Arc;
use std::time::Duration;

use tokio_util::sync::CancellationToken;
use toolkit_macros::domain_model;

struct Inner {
    begun: CancellationToken,
    cut: CancellationToken,
    cut_after: Duration,
}

/// Cheap to clone; all clones share the state.
#[domain_model]
#[derive(Clone)]
pub struct ShutdownSignal {
    inner: Arc<Inner>,
}

impl ShutdownSignal {
    /// A signal that has not begun; once it does, the cut token fires after
    /// `cut_after`.
    #[must_use]
    pub fn new(cut_after: Duration) -> Self {
        Self {
            inner: Arc::new(Inner {
                begun: CancellationToken::new(),
                cut: CancellationToken::new(),
                cut_after,
            }),
        }
    }

    /// The gear is stopping: reject new requests, start the cut timer.
    /// Idempotent. Must run inside a Tokio runtime.
    pub fn begin(&self) {
        if self.inner.begun.is_cancelled() {
            return;
        }
        self.inner.begun.cancel();
        let cut = self.inner.cut.clone();
        let after = self.inner.cut_after;
        tokio::spawn(async move {
            tokio::time::sleep(after).await;
            cut.cancel();
        });
    }

    /// Resolve `begin` once `stop` (the runtime's cancel) fires.
    pub async fn begin_on(&self, stop: &CancellationToken) {
        stop.cancelled().await;
        self.begin();
    }

    /// Whether the gear is stopping.
    #[must_use]
    pub fn is_begun(&self) -> bool {
        self.inner.begun.is_cancelled()
    }

    /// Fires when the drain deadline passed.
    #[must_use]
    pub fn cut(&self) -> CancellationToken {
        self.inner.cut.clone()
    }
}

impl Default for ShutdownSignal {
    /// A signal that never begins on its own (tests, embedding).
    fn default() -> Self {
        Self::new(Duration::from_hours(24 * 365))
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "shutdown_tests.rs"]
mod shutdown_tests;
