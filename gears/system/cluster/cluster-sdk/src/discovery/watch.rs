// Created: 2026-06-04 by Constructor Tech
//! The service-discovery topology-change type, the watch-union event type, and
//! the [`ServiceWatch`] receiver.
//!
//! The union shape (`Change`/`Lagged`/`Reset`/`Closed`) is shared across all
//! three cluster watches per ADR-003 / DESIGN §3.9. Like [`CacheWatch`] — and
//! unlike `LeaderWatch` — a [`ServiceWatch`] is a pure event stream with **no**
//! synchronous cached snapshot, so it mirrors the cache watch exactly:
//! `channel(capacity)` pairs a [`ServiceWatchSender`] with the receiver, and the
//! consumer awaits [`ServiceWatch::recv`]. Dropping the receiver unsubscribes.
//!
//! [`CacheWatch`]: crate::cache::CacheWatch
//!
//! The stream is **unfiltered** (DESIGN §3.3): the backend emits every topology
//! change for the service and the consumer applies its own
//! [`DiscoveryFilter`](crate::discovery::DiscoveryFilter) client-side
//! (`inst-tw-filter`). On `Lagged`/`Reset` the consumer re-reads membership via
//! `discover` (`inst-tw-reread`); on `Closed` it stops consuming
//! (`inst-tw-stop`).
//!
//! Naming note: this receiver is named [`recv`](ServiceWatch::recv) to match
//! [`CacheWatch::recv`](crate::cache::CacheWatch::recv) (its true analog — both
//! are snapshot-free streams), whereas `LeaderWatch::changed` reads its event
//! stream alongside a synchronous status snapshot. Reconciling the
//! `recv`/`changed` naming across the three watches is deferred to the watch
//! auto-restart combinator (DECOMPOSITION §2.8), which wraps all three.

use std::sync::Arc;

use tokio::sync::mpsc;

use crate::discovery::types::ServiceInstance;
use crate::error::ClusterError;
use crate::observability::ClusterMetrics;
use crate::restart::{RestartingWatch, ResubscribeFuture, RetryPolicy};

/// The boxed factory the facade installs so an auto-restarted watch can re-run
/// `watch` against the bound backend. `None` on a bare
/// [`ServiceWatch::channel`] watch.
type ServiceResubscribeFn = Arc<dyn Fn() -> ResubscribeFuture<ServiceWatch> + Send + Sync>;

/// A topology change for a watched service (DESIGN §3.1 / §3.9).
///
/// `#[non_exhaustive]` so future change kinds are additive.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum TopologyChange {
    /// A new instance joined and is now discoverable.
    Joined(ServiceInstance),
    /// The identified instance left (deregistered or its heartbeat expired).
    /// Carries only the instance id — the record is already gone.
    Left {
        /// The id of the instance that left.
        instance_id: String,
    },
    /// An existing instance changed — its metadata or serving intent was
    /// updated (e.g. a drain flip via
    /// [`ServiceHandle::set_state`](crate::discovery::ServiceHandle::set_state)).
    Updated(ServiceInstance),
}

/// A service-discovery watch-union event (DESIGN §3.9). Infallible at the type
/// level per ADR-003: terminal failures arrive as [`ServiceWatchEvent::Closed`];
/// transient backend errors are retried internally and never surface here.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum ServiceWatchEvent {
    /// A topology change; apply the consumer's filter client-side.
    Change(TopologyChange),
    /// The watcher fell behind and `dropped` events were lost; treat membership
    /// as stale and re-read via `discover`.
    Lagged {
        /// The number of events dropped.
        dropped: u64,
    },
    /// The subscription was re-established (reconnect or compaction); re-read
    /// membership via `discover`.
    Reset,
    /// Terminal — the watch is no longer usable and yields no further events.
    Closed(ClusterError),
}

/// An async receiver of [`ServiceWatchEvent`]s for one `watch` subscription.
/// Dropping it unsubscribes.
pub struct ServiceWatch {
    rx: mpsc::Receiver<ServiceWatchEvent>,
    /// Installed by the facade so [`auto_restart`](Self::auto_restart) can
    /// reconnect; `None` for a bare [`channel`](Self::channel) watch.
    resubscribe: Option<ServiceResubscribeFn>,
    /// The `(provider, metrics)` context stamped by the backend so
    /// [`RestartingWatch`] can emit the watch-reset signals on a reconnect.
    observability: Option<(&'static str, Arc<dyn ClusterMetrics>)>,
}

impl std::fmt::Debug for ServiceWatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The resubscribe factory is not `Debug`; report only its presence.
        f.debug_struct("ServiceWatch")
            .field("resubscribe", &self.can_resubscribe())
            .finish_non_exhaustive()
    }
}

impl ServiceWatch {
    /// Creates a watch and its paired sender. The backend pushes events through
    /// the returned [`ServiceWatchSender`]; the [`ServiceWatch`] is handed to the
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
    pub fn channel(capacity: usize) -> (ServiceWatchSender, Self) {
        let (tx, rx) = mpsc::channel(capacity);
        (
            ServiceWatchSender { tx },
            Self {
                rx,
                resubscribe: None,
                observability: None,
            },
        )
    }

    /// Awaits the next event, or `None` once the backend has dropped the sender
    /// without sending a terminal [`ServiceWatchEvent::Closed`] (end of stream).
    pub async fn recv(&mut self) -> Option<ServiceWatchEvent> {
        self.rx.recv().await
    }

    /// Installs the resubscribe seam used by [`auto_restart`](Self::auto_restart).
    /// Crate-internal: the facade calls this so a reconnect re-runs the original
    /// `watch` against the bound backend.
    pub(crate) fn set_resubscribe<F>(&mut self, factory: F)
    where
        F: Fn() -> ResubscribeFuture<ServiceWatch> + Send + Sync + 'static,
    {
        self.resubscribe = Some(Arc::new(factory));
    }

    /// Stamps the `(provider, metrics)` observability context so an
    /// [`auto_restart`](Self::auto_restart)ed watch emits the watch-reset signals.
    pub fn set_observability(&mut self, provider: &'static str, metrics: Arc<dyn ClusterMetrics>) {
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
    pub(crate) fn try_resubscribe(&self) -> Option<ResubscribeFuture<ServiceWatch>> {
        self.resubscribe.as_ref().map(|factory| factory())
    }

    /// Wraps this watch in the opt-in [`RestartingWatch`] combinator, which
    /// transparently reconnects on retryable terminal closes per `policy`
    /// (DESIGN §3.9). The raw watch remains consumable without this wrapper.
    pub fn auto_restart(self, policy: RetryPolicy) -> RestartingWatch<Self> {
        RestartingWatch::new(self, policy)
    }
}

/// Why a non-blocking [`try_send`](ServiceWatchSender::try_send) did not deliver.
///
/// Crate-internal (the only caller is the shutdown-revocation path) and a small
/// closed enum, so it does not bloat the `Err` variant the way returning the
/// large [`ServiceWatchEvent`] would.
#[derive(Debug)]
pub enum ServiceWatchTrySendError {
    /// The consumer's buffer is full; the event was dropped.
    Full,
    /// The consumer dropped the watch.
    Closed,
}

/// The backend-side sender paired with a [`ServiceWatch`] by
/// [`ServiceWatch::channel`].
#[derive(Debug, Clone)]
pub struct ServiceWatchSender {
    tx: mpsc::Sender<ServiceWatchEvent>,
}

impl ServiceWatchSender {
    /// Sends an event to the paired [`ServiceWatch`].
    ///
    /// # Errors
    /// Returns the unsent event back if the consumer has dropped the watch.
    pub async fn send(&self, event: ServiceWatchEvent) -> Result<(), ServiceWatchEvent> {
        self.tx.send(event).await.map_err(|err| err.0)
    }

    /// Tries to deliver `event` without ever blocking the caller, returning a
    /// [`ServiceWatchTrySendError`] ([`Full`](ServiceWatchTrySendError::Full) when
    /// the buffer is full, [`Closed`](ServiceWatchTrySendError::Closed) when the
    /// consumer dropped the watch) instead of the event itself.
    ///
    /// Used on the graceful-shutdown revocation path: the backend awaits the
    /// watch task to confirm the terminal `Closed(Shutdown)` was emitted, so that
    /// send must not block on a backed-up consumer (mirrors
    /// [`LeaderWatchSender::revoke_for_shutdown`](crate::leader::LeaderWatchSender)
    /// and [`CacheWatchSender::try_send`](crate::cache::CacheWatchSender::try_send)).
    ///
    /// # Errors
    /// Returns [`ServiceWatchTrySendError::Full`] when the consumer's buffer is
    /// full, or [`ServiceWatchTrySendError::Closed`] when the consumer has
    /// dropped the watch.
    pub fn try_send(&self, event: ServiceWatchEvent) -> Result<(), ServiceWatchTrySendError> {
        self.tx.try_send(event).map_err(|err| match err {
            mpsc::error::TrySendError::Full(_) => ServiceWatchTrySendError::Full,
            mpsc::error::TrySendError::Closed(_) => ServiceWatchTrySendError::Closed,
        })
    }
}

#[cfg(test)]
#[path = "watch_tests.rs"]
mod watch_tests;
