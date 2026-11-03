//! A cancellation switch that is cheap to check.
//!
//! [`CancellationToken::is_cancelled`] takes a lock; the I/O paths check
//! whether a connection was killed on every read and write, so they read a
//! flag instead and keep the token only for waiting.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use tokio_util::sync::{
    CancellationToken, WaitForCancellationFuture, WaitForCancellationFutureOwned,
};

/// Shared kill switch of a connection or an exchange. Clones share it.
#[derive(Debug, Clone, Default)]
pub struct Kill {
    inner: Arc<Inner>,
}

#[derive(Debug, Default)]
struct Inner {
    killed: AtomicBool,
    token: CancellationToken,
}

impl Kill {
    /// A switch that is not flipped yet.
    pub fn new() -> Self {
        Self::default()
    }

    /// Flip the switch and wake everyone waiting on it. Idempotent.
    pub fn cancel(&self) {
        self.inner.killed.store(true, Ordering::Release);
        self.inner.token.cancel();
    }

    /// Whether the switch was flipped (an atomic load, no lock).
    pub fn is_cancelled(&self) -> bool {
        self.inner.killed.load(Ordering::Acquire)
    }

    /// Resolves once the switch is flipped.
    pub fn cancelled(&self) -> WaitForCancellationFuture<'_> {
        self.inner.token.cancelled()
    }

    /// [`Kill::cancelled`] without a borrow.
    pub fn cancelled_owned(&self) -> WaitForCancellationFutureOwned {
        self.inner.token.clone().cancelled_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn cancel_is_seen_by_clones_and_waiters() {
        let kill = Kill::new();
        let clone = kill.clone();
        let waiter = tokio::spawn(clone.cancelled_owned());
        assert!(!clone.is_cancelled());
        kill.cancel();
        assert!(clone.is_cancelled());
        waiter.await.unwrap();
        // Already cancelled: resolves at once.
        clone.cancelled().await;
    }
}
