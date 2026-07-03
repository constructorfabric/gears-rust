// Created: 2026-06-03 by Constructor Tech
//! The cache watch-union event type and the [`CacheWatch`] receiver.
//!
//! The union shape (`Event`/`Lagged`/`Reset`/`Closed`) is shared across all
//! three cluster watches per ADR-003 / DESIGN §3.9. `CacheWatch` is a
//! single-consumer receiver: per-key ordering is preserved and each event is
//! delivered at most once. Backends drive events through the paired
//! [`CacheWatchSender`]; the SDK auto-restart combinator (a later feature)
//! wraps the receiver.

use std::sync::Arc;

use tokio::sync::mpsc;

use crate::cache::types::CacheEvent;
use crate::error::ClusterError;
use crate::observability::ClusterMetrics;
use crate::restart::{RestartingWatch, ResubscribeFuture, RetryPolicy};

/// The boxed factory the facade installs so an auto-restarted watch can recreate
/// its subscription (re-running `watch`/`watch_prefix` against the bound
/// backend). `None` on a bare [`CacheWatch::channel`] watch.
type CacheResubscribeFn = Arc<dyn Fn() -> ResubscribeFuture<CacheWatch> + Send + Sync>;

/// A cache watch-union event (DESIGN §3.9). Infallible at the type level:
/// terminal failures arrive as [`CacheWatchEvent::Closed`]; transient backend
/// errors are retried internally and never surface here.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum CacheWatchEvent {
    /// A cache mutation; the consumer calls `get(key)` for the value.
    Event(CacheEvent),
    /// The watcher fell behind and `dropped` events were lost; treat watched
    /// keys as stale and re-read.
    Lagged {
        /// The number of events dropped.
        dropped: u64,
    },
    /// The subscription was re-established (reconnect or compaction); re-read.
    Reset,
    /// Terminal — the watch is no longer usable and yields no further events.
    Closed(ClusterError),
}

/// An async receiver of [`CacheWatchEvent`]s for one `watch`/`watch_prefix`
/// subscription. Dropping it unsubscribes.
pub struct CacheWatch {
    rx: mpsc::Receiver<CacheWatchEvent>,
    /// Installed by the facade so [`auto_restart`](Self::auto_restart) can
    /// reconnect; `None` for a bare [`channel`](Self::channel) watch.
    resubscribe: Option<CacheResubscribeFn>,
    /// The `(provider, metrics)` context stamped by the (instrumented) backend so
    /// [`RestartingWatch`] can emit `cluster_watch_resets_total` /
    /// `cluster.watch.reset` on a reconnect. `None` when no metrics are wired.
    observability: Option<(&'static str, Arc<dyn ClusterMetrics>)>,
}

impl std::fmt::Debug for CacheWatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The resubscribe factory is not `Debug`; report only its presence.
        f.debug_struct("CacheWatch")
            .field("resubscribe", &self.can_resubscribe())
            .finish_non_exhaustive()
    }
}

impl CacheWatch {
    /// Creates a watch and its paired sender. The backend pushes events through
    /// the returned [`CacheWatchSender`]; the [`CacheWatch`] is handed to the
    /// consumer. `capacity` bounds the in-flight buffer.
    ///
    /// The watch carries no resubscribe seam, so [`auto_restart`](Self::auto_restart)
    /// propagates a retryable close unchanged rather than reconnecting; the
    /// facade installs the seam on watches it hands to consumers.
    ///
    /// # Panics
    /// Panics if `capacity` is zero — a bounded channel requires a non-zero
    /// buffer.
    #[must_use]
    pub fn channel(capacity: usize) -> (CacheWatchSender, Self) {
        let (tx, rx) = mpsc::channel(capacity);
        (
            CacheWatchSender { tx },
            Self {
                rx,
                resubscribe: None,
                observability: None,
            },
        )
    }

    /// Awaits the next event, or `None` once the backend has dropped the sender
    /// without sending a terminal [`CacheWatchEvent::Closed`] (end of stream).
    pub async fn recv(&mut self) -> Option<CacheWatchEvent> {
        self.rx.recv().await
    }

    /// Installs the resubscribe seam used by [`auto_restart`](Self::auto_restart).
    /// Crate-internal: the facade calls this so a reconnect re-runs the original
    /// `watch`/`watch_prefix` against the bound backend.
    pub(crate) fn set_resubscribe<F>(&mut self, factory: F)
    where
        F: Fn() -> ResubscribeFuture<CacheWatch> + Send + Sync + 'static,
    {
        self.resubscribe = Some(Arc::new(factory));
    }

    /// Stamps the `(provider, metrics)` observability context, so an
    /// [`auto_restart`](Self::auto_restart)ed watch emits the watch-reset signals.
    /// Set by the instrumented backend when it hands out the watch.
    pub(crate) fn set_observability(
        &mut self,
        provider: &'static str,
        metrics: Arc<dyn ClusterMetrics>,
    ) {
        self.observability = Some((provider, metrics));
    }

    /// The stamped observability context, if any.
    pub(crate) fn observability_context(&self) -> Option<(&'static str, Arc<dyn ClusterMetrics>)> {
        self.observability.clone()
    }

    /// Whether a resubscribe seam is installed.
    pub(crate) fn can_resubscribe(&self) -> bool {
        self.resubscribe.is_some()
    }

    /// Produces a fresh-subscription future via the installed seam, or `None`
    /// when no seam is present.
    pub(crate) fn try_resubscribe(&self) -> Option<ResubscribeFuture<CacheWatch>> {
        self.resubscribe.as_ref().map(|factory| factory())
    }

    /// Wraps this watch in the opt-in [`RestartingWatch`] combinator, which
    /// transparently reconnects on retryable terminal closes per `policy`
    /// (DESIGN §3.9). The raw watch remains consumable without this wrapper.
    pub fn auto_restart(self, policy: RetryPolicy) -> RestartingWatch<Self> {
        RestartingWatch::new(self, policy)
    }
}

/// Why a non-blocking [`try_send`](CacheWatchSender::try_send) did not deliver.
///
/// Closed (not `#[non_exhaustive]`): full-vs-closed are the only two intrinsic
/// non-blocking outcomes, so backends can match them exhaustively.
#[derive(Debug)]
pub enum CacheWatchTrySendError {
    /// The consumer's buffer is full; the event was dropped. The backend is
    /// expected to coalesce drops and surface a [`CacheWatchEvent::Lagged`] once
    /// space frees, rather than blocking the writer.
    Full,
    /// The consumer dropped the watch; the subscription is dead and should be
    /// pruned.
    Closed,
}

/// The backend-side sender paired with a [`CacheWatch`] by
/// [`CacheWatch::channel`].
#[derive(Debug, Clone)]
pub struct CacheWatchSender {
    tx: mpsc::Sender<CacheWatchEvent>,
}

impl CacheWatchSender {
    /// Sends an event to the paired [`CacheWatch`], awaiting buffer space if the
    /// consumer is behind.
    ///
    /// Prefer [`try_send`](Self::try_send) on a fan-out path where one slow
    /// consumer must not stall a shared writer.
    ///
    /// # Errors
    /// Returns the unsent event back if the consumer has dropped the watch.
    pub async fn send(&self, event: CacheWatchEvent) -> Result<(), CacheWatchEvent> {
        self.tx.send(event).await.map_err(|err| err.0)
    }

    /// Tries to deliver `event` without ever blocking the caller.
    ///
    /// A backend fanning one mutation out to many watchers can use this so a
    /// single consumer that has stopped draining cannot apply backpressure to
    /// the write path. On [`Full`](CacheWatchTrySendError::Full) the event is
    /// dropped; the backend should record the drop and emit a
    /// [`CacheWatchEvent::Lagged`] once the buffer drains.
    ///
    /// # Errors
    /// [`CacheWatchTrySendError::Full`] if the buffer is full, or
    /// [`CacheWatchTrySendError::Closed`] if the consumer dropped the watch.
    pub fn try_send(&self, event: CacheWatchEvent) -> Result<(), CacheWatchTrySendError> {
        self.tx.try_send(event).map_err(|err| match err {
            mpsc::error::TrySendError::Full(_) => CacheWatchTrySendError::Full,
            mpsc::error::TrySendError::Closed(_) => CacheWatchTrySendError::Closed,
        })
    }

    /// Returns `true` once the paired [`CacheWatch`] has been dropped. Lets a
    /// backend that may go an arbitrary interval without an event (for example
    /// the [`PollingPrefixWatch`](crate::cache::PollingPrefixWatch) polyfill on a
    /// quiescent keyspace) notice the consumer is gone and stop its task.
    #[must_use]
    pub fn is_closed(&self) -> bool {
        self.tx.is_closed()
    }

    /// Resolves once the paired [`CacheWatch`] has been dropped. Lets a forwarding
    /// task that may park on an upstream `recv()` (for example the scoped-backend
    /// event rewriter) notice the consumer is gone and stop promptly, rather than
    /// only on the next event's failed `send`.
    pub(crate) async fn closed(&self) {
        self.tx.closed().await;
    }
}

#[cfg(test)]
mod tests {
    use super::{CacheEvent, CacheWatch, CacheWatchEvent};

    #[tokio::test]
    async fn delivers_events_in_order_then_ends_on_sender_drop() {
        let (tx, mut watch) = CacheWatch::channel(8);
        assert!(
            tx.send(CacheWatchEvent::Event(CacheEvent::Changed {
                key: "k".to_owned()
            }))
            .await
            .is_ok()
        );
        assert!(tx.send(CacheWatchEvent::Reset).await.is_ok());
        drop(tx);

        assert!(matches!(
            watch.recv().await,
            Some(CacheWatchEvent::Event(CacheEvent::Changed { .. }))
        ));
        assert!(matches!(watch.recv().await, Some(CacheWatchEvent::Reset)));
        assert!(watch.recv().await.is_none());
    }

    #[tokio::test]
    async fn send_errors_after_watch_dropped() {
        let (tx, watch) = CacheWatch::channel(1);
        drop(watch);
        assert!(tx.send(CacheWatchEvent::Reset).await.is_err());
    }
}
