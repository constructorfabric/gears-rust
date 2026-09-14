//! The leadership watch-union event type and the [`LeaderWatch`] handle.
//!
//! The union shape (`Status`/`Lagged`/`Reset`/`Closed`) is shared across all
//! three cluster watches per ADR-003 / DESIGN §3.9. Unlike the cache watch, a
//! [`LeaderWatch`] also exposes a **synchronous cached status** in addition to
//! the event stream, so it carries three coupled channels driven by the
//! backend's renewal task:
//!
//! - an `mpsc` event receiver — drives [`LeaderWatch::changed`];
//! - a [`tokio::sync::watch`] snapshot of the current [`LeaderStatus`] — drives
//!   the synchronous [`LeaderWatch::status`] / [`LeaderWatch::is_leader`];
//! - a typed resign command channel — carries [`LeaderWatch::resign`] back to
//!   the backend so the consumer's explicit step-down returns the backend's
//!   real result.
//!
//! The single backend renewal task drives all three through the paired
//! [`LeaderWatchSender`] (which is deliberately **not** `Clone`, so the
//! single-writer ordering below cannot be subverted): its
//! [`send_status`](LeaderWatchSender::send_status) updates the snapshot and
//! emits the matching `Status` event together so the cached snapshot can never
//! disagree with the last observed transition. If the sender is dropped
//! abruptly — without the graceful terminal `Status(Lost)` — the snapshot
//! reports [`LeaderStatus::Lost`] rather than latching its last value, staying
//! coherent with the `Closed` event that [`LeaderWatch::changed`] synthesizes
//! on the same teardown.

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::error::{ClusterError, ProviderErrorKind};
use crate::leader::types::LeaderStatus;
use crate::observability::ClusterMetrics;
use crate::restart::{RestartingWatch, ResubscribeFuture, RetryPolicy};

/// The boxed factory the facade installs so an auto-restarted leader watch can
/// re-run `elect`/`elect_with_config` against the bound backend. `None` on a
/// bare [`LeaderWatch::channel`] watch.
type LeaderResubscribeFn = Arc<dyn Fn() -> ResubscribeFuture<LeaderWatch> + Send + Sync>;

/// Extra event-channel slots reserved as permits for the two terminal
/// shutdown events (`Status(Lost)` then `Closed(Shutdown)`), so they remain
/// deliverable even when the consumer-visible buffer is full (ADR-003).
const TERMINAL_HEADROOM: usize = 2;

/// A leadership watch-union event (DESIGN §3.9). Infallible at the type level
/// per ADR-003: terminal failures arrive as [`LeaderWatchEvent::Closed`];
/// transient backend errors are retried internally by the renewal task and
/// never surface here.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum LeaderWatchEvent {
    /// A leadership transition. `Status(Lost)` is transient — the watch
    /// auto-reenrolls and a later `Status` resolves to `Leader`/`Follower`.
    Status(LeaderStatus),
    /// The watcher fell behind and `dropped` events were lost; wait for the
    /// next `Status` event before resuming leader-only work.
    Lagged {
        /// The number of events dropped.
        dropped: u64,
    },
    /// The subscription was re-established (reconnect or compaction); wait for
    /// the next `Status` event before resuming leader-only work.
    Reset,
    /// Terminal — the watch is no longer usable and yields no further events.
    /// On graceful shutdown the backend delivers `Status(Lost)` first, then
    /// `Closed(ClusterError::Shutdown)`.
    Closed(ClusterError),
}

/// An internal resign command: a request to release the claim, carrying a
/// one-shot reply channel so the backend can return its real result.
struct ResignRequest {
    reply: oneshot::Sender<Result<(), ClusterError>>,
}

/// A handle into an ongoing election (DESIGN §3.1 / §3.3).
///
/// Observe leadership two ways: await [`changed`](Self::changed) for the event
/// stream, or read the cached [`status`](Self::status) / [`is_leader`](Self::is_leader)
/// synchronously inside a loop. Call [`resign`](Self::resign) for an explicit,
/// immediate step-down.
///
/// **Drop is a no-op** (no I/O in `Drop`): dropping the watch does *not* resign
/// — leadership lapses through TTL expiry, the safety net. Use
/// [`resign`](Self::resign) for immediate release.
pub struct LeaderWatch {
    events: mpsc::Receiver<LeaderWatchEvent>,
    snapshot: watch::Receiver<LeaderStatus>,
    resign: mpsc::Sender<ResignRequest>,
    /// Installed by the facade so [`auto_restart`](Self::auto_restart) can
    /// re-run the election; `None` for a bare [`channel`](Self::channel) watch.
    resubscribe: Option<LeaderResubscribeFn>,
    /// The `(provider, metrics)` context stamped by the backend so
    /// [`RestartingWatch`] can emit the watch-reset signals on a reconnect.
    observability: Option<(&'static str, Arc<dyn ClusterMetrics>)>,
}

impl std::fmt::Debug for LeaderWatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The resubscribe factory is not `Debug`; report only its presence.
        f.debug_struct("LeaderWatch")
            .field("snapshot", &*self.snapshot.borrow())
            .field("resubscribe", &self.can_resubscribe())
            .finish_non_exhaustive()
    }
}

impl LeaderWatch {
    /// Creates a watch and its paired [`LeaderWatchSender`], plus the
    /// backend-side [`ResignReceiver`] that delivers consumer resign requests.
    ///
    /// `buffer` bounds the in-flight event buffer; `initial` is the leadership
    /// status the watch reports until the backend pushes its first transition
    /// (the initial state is `Follower` per the leadership state machine).
    ///
    /// # Panics
    /// Panics if `buffer` is zero — a bounded channel requires a non-zero buffer.
    #[must_use]
    pub fn channel(
        buffer: usize,
        initial: LeaderStatus,
    ) -> (LeaderWatchSender, ResignReceiver, Self) {
        assert!(
            buffer > 0,
            "LeaderWatch::channel requires a non-zero buffer"
        );
        // Two extra slots are reserved as permits for the terminal shutdown
        // events, so the consumer-visible in-flight buffer stays `buffer`.
        let (event_tx, event_rx) = mpsc::channel(buffer + TERMINAL_HEADROOM);
        let (snapshot_tx, snapshot_rx) = watch::channel(initial);
        // A single in-flight resign is enough: `resign(self)` consumes the watch.
        let (resign_tx, resign_rx) = mpsc::channel(1);
        // Reserve the terminal-event headroom up front. The channel is freshly
        // created with spare capacity, so both reservations always succeed; the
        // `unreachable!` would only fire on an internal capacity-accounting bug.
        let Ok(lost_permit) = event_tx.clone().try_reserve_owned() else {
            unreachable!("reserving terminal-event headroom on a fresh channel cannot fail");
        };
        let Ok(closed_permit) = event_tx.clone().try_reserve_owned() else {
            unreachable!("reserving terminal-event headroom on a fresh channel cannot fail");
        };
        let sender = LeaderWatchSender {
            events: event_tx,
            snapshot: snapshot_tx,
            lost_permit: Some(lost_permit),
            closed_permit: Some(closed_permit),
            dropped: 0,
        };
        let watch = Self {
            events: event_rx,
            snapshot: snapshot_rx,
            resign: resign_tx,
            resubscribe: None,
            observability: None,
        };
        (sender, ResignReceiver { rx: resign_rx }, watch)
    }

    /// Installs the resubscribe seam used by [`auto_restart`](Self::auto_restart).
    /// Crate-internal: the facade calls this so a reconnect re-runs the original
    /// `elect`/`elect_with_config` against the bound backend.
    pub(crate) fn set_resubscribe<F>(&mut self, factory: F)
    where
        F: Fn() -> ResubscribeFuture<LeaderWatch> + Send + Sync + 'static,
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
    pub(crate) fn try_resubscribe(&self) -> Option<ResubscribeFuture<LeaderWatch>> {
        self.resubscribe.as_ref().map(|factory| factory())
    }

    /// Wraps this watch in the opt-in [`RestartingWatch`] combinator, which
    /// transparently reconnects on retryable terminal closes per `policy`
    /// (DESIGN §3.9). The reconnected subscription's `status`/`is_leader`/`resign`
    /// remain available on the wrapper. The raw watch stays consumable without it.
    pub fn auto_restart(self, policy: RetryPolicy) -> RestartingWatch<Self> {
        RestartingWatch::new(self, policy)
    }

    /// Awaits the next leadership event.
    ///
    /// Infallible at the type level (ADR-003): transient backend errors are
    /// retried internally and never surface; terminal failures arrive as
    /// [`LeaderWatchEvent::Closed`]. If the backend drops its sender without a
    /// terminal `Closed`, this synthesizes (and thereafter keeps returning)
    /// `Closed(ClusterError::Shutdown)` so the contract still holds.
    pub async fn changed(&mut self) -> LeaderWatchEvent {
        match self.events.recv().await {
            Some(event) => event,
            None => LeaderWatchEvent::Closed(ClusterError::Shutdown),
        }
    }

    /// The cached leadership snapshot maintained by the backend's renewal task.
    /// Synchronous, no I/O.
    ///
    /// Once the backend has torn down its sender — whether via the graceful
    /// terminal `Status(Lost)` or an abrupt drop — this reports
    /// [`LeaderStatus::Lost`]: the renewal task is gone, so the claim can no
    /// longer be held and must not appear as stale leadership.
    ///
    /// **Advisory** — see [`is_leader`](Self::is_leader) for the staleness bound.
    #[must_use]
    pub fn status(&self) -> LeaderStatus {
        // A closed snapshot channel means every sender was dropped. Without
        // this guard an abrupt teardown (no terminal `Status(Lost)`) would
        // latch the last value — e.g. `Leader` — indefinitely, so a
        // gate-pattern consumer would never observe the loss.
        if self.snapshot.has_changed().is_err() {
            return LeaderStatus::Lost;
        }
        *self.snapshot.borrow()
    }

    /// `true` when the cached snapshot is [`LeaderStatus::Leader`].
    ///
    /// **Advisory — do NOT use for correctness-critical mutual exclusion.** The
    /// snapshot lags backend truth by up to one renewal interval plus a provider
    /// round-trip in steady state, and up to a full TTL under partition
    /// (DESIGN §3.3 staleness bound). Workloads where two simultaneous writers
    /// would corrupt state must combine the reactive pattern with
    /// `DistributedLockV1::try_lock` or `ClusterCacheV1::compare_and_swap`.
    #[must_use]
    pub fn is_leader(&self) -> bool {
        matches!(self.status(), LeaderStatus::Leader)
    }

    /// Explicitly resigns from the election, releasing the claim immediately so
    /// a successor is elected within a backend round-trip.
    ///
    /// Consumes the watch — no further observation is possible after resigning.
    ///
    /// # Errors
    /// Returns the backend's own result for the release when it replies. Two
    /// teardown cases are distinguished (DESIGN §3.7):
    ///
    /// - **Backend gone** — the request cannot even be delivered (the backend's
    ///   resign receiver was dropped, e.g. after cluster shutdown). Its renewal
    ///   task has stopped, so the claim can no longer be renewed and lapses via
    ///   TTL; this returns `Ok(())` on a best-effort basis (the post-shutdown
    ///   narrowing).
    /// - **Acknowledgement lost** — the backend accepted the request but
    ///   dropped the reply without responding (a crash or connection loss
    ///   mid-release). The release is *not* confirmed, so this propagates a
    ///   [`ClusterError::Provider`] rather than masking the failure as success;
    ///   the claim still lapses via TTL. §3.7 requires such mid-release
    ///   failures to surface rather than be hidden under the best-effort rule.
    pub async fn resign(self) -> Result<(), ClusterError> {
        let (reply_tx, reply_rx) = oneshot::channel();
        if self
            .resign
            .send(ResignRequest { reply: reply_tx })
            .await
            .is_err()
        {
            // Backend's resign receiver is gone, so its renewal task has
            // stopped: the claim can no longer be renewed and necessarily
            // lapses via TTL. Nothing more can be released — best-effort Ok.
            return Ok(());
        }
        match reply_rx.await {
            // The backend completed the release and reported its real outcome.
            Ok(result) => result,
            // The backend accepted the request but dropped the responder
            // without replying — it crashed or lost the connection mid-release.
            // §3.7 requires this to propagate, not be masked as success: the
            // release is unconfirmed and the claim only lapses via TTL.
            Err(_) => Err(ClusterError::Provider {
                kind: ProviderErrorKind::ConnectionLost,
                message: "leader-election backend dropped the resign \
                          acknowledgement without responding; the claim was not \
                          confirmed released and will lapse via TTL"
                    .to_owned(),
            }),
        }
    }

    /// Runs `work` only while this participant is the leader — the "run this
    /// singleton while I'm leader" convenience over the raw event stream.
    ///
    /// Consumes the watch and drives the reactive pattern (the DESIGN §3.3
    /// pattern 2 the facade documents) so consumers don't each re-implement it:
    /// `work` is spawned on a fresh child [`CancellationToken`] when leadership
    /// is acquired, and that token is cancelled when leadership is lost
    /// (`Status(Lost)`/`Status(Follower)`) or the watch closes terminally. If
    /// the cancelled work does not return within `stop_timeout` its task is
    /// aborted, so unresponsive leader work cannot wedge the loop (cf. the k8s
    /// elector's `stop_with_timeout`). `work` is re-invoked on re-election, hence
    /// the `Fn` bound; it receives the child token and should return promptly
    /// once it fires.
    ///
    /// Returns when the watch closes terminally (the backend tore down — e.g. on
    /// cluster shutdown). To reconnect across terminal closes, wrap the watch
    /// with [`auto_restart`](Self::auto_restart) and drive that. **Dropping the
    /// returned future** (e.g. losing a `select!` against a shutdown signal)
    /// cancels the in-flight work's token and aborts its task, so the worker
    /// never outlives the loop.
    ///
    /// Leadership is observed from the event stream, which the SDK backends drive
    /// by emitting the initial `Status` as the first event; a hand-built
    /// [`channel`](Self::channel) watch whose sender never emits a `Status` would
    /// therefore never start `work`.
    ///
    /// **Advisory — not mutual exclusion.** The leadership signal is a cached
    /// snapshot (see [`is_leader`](Self::is_leader)), so two participants can
    /// transiently run `work` at once. Workloads where that would corrupt state
    /// must additionally gate their writes on `DistributedLockV1::try_lock` or a
    /// `ClusterCacheV1::compare_and_swap`.
    pub async fn run_while_leader<F, Fut>(mut self, stop_timeout: Duration, work: F)
    where
        F: Fn(CancellationToken) -> Fut + Send,
        Fut: Future<Output = ()> + Send + 'static,
    {
        let mut active: Option<ActiveWork> = None;
        loop {
            match self.changed().await {
                LeaderWatchEvent::Status(LeaderStatus::Leader) => {
                    // (Re)start work unless it is already running. Reassigning
                    // `active` drops any prior worker; a worker that finished on
                    // its own makes its `Drop`'s cancel/abort no-ops.
                    if active.as_ref().is_none_or(ActiveWork::is_finished) {
                        let child = CancellationToken::new();
                        let handle = tokio::spawn(work(child.clone()));
                        active = Some(ActiveWork { child, handle });
                    }
                }
                LeaderWatchEvent::Status(LeaderStatus::Lost | LeaderStatus::Follower) => {
                    if let Some(active) = active.take() {
                        active.stop(stop_timeout).await;
                    }
                }
                LeaderWatchEvent::Closed(_) => {
                    if let Some(active) = active.take() {
                        active.stop(stop_timeout).await;
                    }
                    return;
                }
                // `Lagged`/`Reset` don't themselves change leadership; the next
                // `Status` event reconciles. Running work is left as-is, matching
                // the advisory semantics documented above.
                LeaderWatchEvent::Lagged { .. } | LeaderWatchEvent::Reset => {}
            }
        }
    }
}

/// A leader worker spawned by [`LeaderWatch::run_while_leader`], paired with the
/// token that cancels it.
///
/// Its `Drop` is the backstop for the loop future being dropped while work is in
/// flight: a bare [`JoinHandle`] drop *detaches* the task (and dropping the
/// token does not cancel it), so the worker could outlive the loop. Dropping
/// `ActiveWork` instead cancels the token and aborts the task, bounding it.
struct ActiveWork {
    child: CancellationToken,
    handle: JoinHandle<()>,
}

impl ActiveWork {
    fn is_finished(&self) -> bool {
        self.handle.is_finished()
    }

    /// Cancels the worker's token and awaits it, aborting the task if it does not
    /// return within `stop_timeout` so unresponsive leader work cannot wedge the
    /// caller (cf. the k8s elector's `stop_with_timeout`).
    async fn stop(mut self, stop_timeout: Duration) {
        self.child.cancel();
        if tokio::time::timeout(stop_timeout, &mut self.handle)
            .await
            .is_err()
        {
            self.handle.abort();
            let _aborted = (&mut self.handle).await;
        }
        // `self` drops here; its `Drop` re-cancels/aborts as a harmless backstop.
    }
}

impl Drop for ActiveWork {
    fn drop(&mut self) {
        self.child.cancel();
        self.handle.abort();
    }
}

/// The backend-side sender paired with a [`LeaderWatch`] by
/// [`LeaderWatch::channel`]. Drives both the event stream and the cached status
/// snapshot.
///
/// Intentionally **not** `Clone`: a single renewal task owns it, which is what
/// guarantees the snapshot/event ordering in [`send_status`](Self::send_status)
/// (concurrent senders could interleave a snapshot update ahead of an older
/// event). Per-watch fan-out, if ever needed, belongs in the backend, not here.
pub struct LeaderWatchSender {
    events: mpsc::Sender<LeaderWatchEvent>,
    snapshot: watch::Sender<LeaderStatus>,
    /// Reserved buffer headroom for the two terminal shutdown events, so the
    /// `Status(Lost)` → `Closed(Shutdown)` two-step in
    /// [`revoke_for_shutdown`](Self::revoke_for_shutdown) is always deliverable
    /// even against a full event buffer (ADR-003). Each slot is reserved at
    /// [`channel`](LeaderWatch::channel) construction and consumed at shutdown; on
    /// abrupt teardown they drop with the sender, releasing the slots so the
    /// receiver still observes channel closure.
    lost_permit: Option<mpsc::OwnedPermit<LeaderWatchEvent>>,
    closed_permit: Option<mpsc::OwnedPermit<LeaderWatchEvent>>,
    /// Events dropped because the consumer's buffer was full, owed to it as a
    /// [`LeaderWatchEvent::Lagged`] by the next
    /// [`try_send_status`](LeaderWatchSender::try_send_status) /
    /// [`try_send`](LeaderWatchSender::try_send) that finds room, or flushed
    /// proactively by [`flush_lagged`](LeaderWatchSender::flush_lagged) from the
    /// renewal tick when the election falls quiet (§6.8's drop-then-`Lagged`
    /// rule).
    ///
    /// Accumulated rather than reported eagerly, exactly as both plugin cache
    /// fan-outs do: "you missed one", repeated, is more sends into a channel that
    /// is already full, and the consumer's response to any count is the same —
    /// treat the leadership status as stale and wait for the next `Status`.
    dropped: u64,
}

impl std::fmt::Debug for LeaderWatchSender {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let reserved =
            usize::from(self.lost_permit.is_some()) + usize::from(self.closed_permit.is_some());
        f.debug_struct("LeaderWatchSender")
            .field("terminal_headroom_reserved", &reserved)
            .field("dropped", &self.dropped)
            .finish_non_exhaustive()
    }
}

impl LeaderWatchSender {
    /// Records a leadership transition: updates the cached snapshot **and**
    /// emits the matching [`LeaderWatchEvent::Status`], keeping
    /// [`LeaderWatch::status`] coherent with the last delivered event.
    ///
    /// # Errors
    /// Returns the unsent event back if the consumer has dropped the watch. The
    /// snapshot is updated regardless (a no-op if no receiver remains).
    pub async fn send_status(&self, status: LeaderStatus) -> Result<(), LeaderWatchEvent> {
        // Update the snapshot first so a synchronous `status()` racing the event
        // delivery never observes a value older than the event about to arrive.
        // `send_replace` latches the value even with no receiver, so it cannot fail.
        self.snapshot.send_replace(status);
        self.events
            .send(LeaderWatchEvent::Status(status))
            .await
            .map_err(|err| err.0)
    }

    /// Emits a non-status event (`Lagged` / `Reset` / `Closed`). These do not
    /// change the cached leadership status.
    ///
    /// Status transitions must go through [`send_status`](Self::send_status),
    /// which updates the snapshot and emits the matching event together; routing
    /// a `Status` through `send` would bypass the snapshot and let
    /// [`LeaderWatch::status`] diverge from the event stream. That invariant is
    /// asserted in debug builds.
    ///
    /// # Errors
    /// Returns the unsent event back if the consumer has dropped the watch.
    pub async fn send(&self, event: LeaderWatchEvent) -> Result<(), LeaderWatchEvent> {
        debug_assert!(
            !matches!(event, LeaderWatchEvent::Status(_)),
            "LeaderWatchSender::send must not carry Status events; use send_status so the \
             snapshot stays coherent with the event stream",
        );
        self.events.send(event).await.map_err(|err| err.0)
    }

    /// Records a leadership transition **without ever blocking**: the snapshot is
    /// updated, and the matching [`LeaderWatchEvent::Status`] is delivered if
    /// there is room or dropped and owed as a
    /// [`LeaderWatchEvent::Lagged`] if there is not.
    ///
    /// Returns `false` only when the consumer has dropped the watch — a full
    /// buffer is `true`, because the subscription is alive and the caller has
    /// nothing to stop for.
    ///
    /// # Why a state machine must use this and not [`send_status`](Self::send_status)
    ///
    /// The awaiting form puts the consumer in the renewal loop's critical path.
    /// [`LeaderWatch`] documents and permits a gate-pattern consumer that reads
    /// [`status`](LeaderWatch::status) synchronously and never drains
    /// [`changed`](LeaderWatch::changed) — so the buffer *will* fill, and one
    /// awaited send then parks the election task forever: it stops renewing, never
    /// re-claims, and cannot even observe its own shutdown signal, while the
    /// latched snapshot keeps answering `Leader` and a rival takes the lease. Two
    /// permanent leaders, reproduced. Dropping an event costs the consumer a
    /// re-read; awaiting one costs the claim.
    ///
    /// **The snapshot is updated whichever way the event goes**, which is what
    /// makes the drop safe for the gate-pattern consumer this exists for: it reads
    /// the snapshot, and the snapshot is never lossy. The event-stream consumer
    /// gets the `Lagged` instead, so nothing is lost silently either.
    pub fn try_send_status(&mut self, status: LeaderStatus) -> bool {
        // Snapshot first, on exactly `send_status`'s reasoning: a synchronous
        // `status()` racing this must never observe a value older than the event
        // about to arrive. `send_replace` latches with no receiver, so it cannot
        // fail.
        self.snapshot.send_replace(status);
        self.offer(LeaderWatchEvent::Status(status))
    }

    /// Emits a non-status event (`Lagged` / `Reset` / `Closed`) on
    /// [`try_send_status`](Self::try_send_status)'s terms: never blocking, dropped
    /// and owed a `Lagged` when the buffer is full, `false` only when the consumer
    /// is gone.
    ///
    /// Status transitions must go through
    /// [`try_send_status`](Self::try_send_status) for the same reason they must go
    /// through [`send_status`](Self::send_status) rather than
    /// [`send`](Self::send) — the snapshot and the event have to move together.
    /// Asserted in debug builds.
    pub fn try_send(&mut self, event: LeaderWatchEvent) -> bool {
        debug_assert!(
            !matches!(event, LeaderWatchEvent::Status(_)),
            "LeaderWatchSender::try_send must not carry Status events; use try_send_status so \
             the snapshot stays coherent with the event stream",
        );
        self.offer(event)
    }

    /// Delivers a **terminal** `Closed(err)` through the reserved headroom, so it
    /// cannot be dropped by a full buffer and cannot block the caller.
    ///
    /// The other terminal path beside
    /// [`revoke_for_shutdown`](Self::revoke_for_shutdown), and it spends the same
    /// `Closed` reservation: the two are mutually exclusive by construction — a
    /// backend that closes terminally stops, so it never revokes afterwards.
    ///
    /// Why not [`try_send`](Self::try_send): a dropped `Closed(err)` is not a gap
    /// the consumer can be told about. [`changed`](LeaderWatch::changed)
    /// synthesizes `Closed(Shutdown)` when the sender is dropped without a
    /// terminal event, so losing a `Closed(Provider{..})` would report a provider
    /// failure to the consumer as an orderly shutdown — the error is the whole
    /// content of a terminal event, and this is the one send where "drop and owe a
    /// `Lagged`" is the wrong trade.
    pub fn try_close(&mut self, err: ClusterError) {
        match self.closed_permit.take() {
            // `send` consumes the permit and returns the sender clone it held,
            // dropped here; the subscription's own sender lives in `events`.
            Some(permit) => {
                let _sender = permit.send(LeaderWatchEvent::Closed(err));
            }
            // The reservation was already spent — only possible if a caller
            // closed twice, which is not a state a second `Closed` can improve.
            None => {
                let _dropped = self.events.try_send(LeaderWatchEvent::Closed(err));
            }
        }
    }

    /// Flushes any outstanding lag notice proactively, delivering the owed
    /// [`LeaderWatchEvent::Lagged`] if there is now room and clearing the debt.
    ///
    /// [`offer`](Self::offer) pays the debt on the *next* event, which is the
    /// right ordering while events keep coming. But a stable leader's renewal
    /// ticks emit no event, so if the election falls quiet after a drop the next
    /// event may never arrive: a `changed()`-only consumer would drain a backlog
    /// whose last status is a stale `Status(Leader)` and never learn it fell
    /// behind — the silent-staleness failure ADR-003 exists to eliminate for a
    /// leadership gate. Both profiles' renewal loops therefore call this from the
    /// tick, so the announcement never depends on unrelated future traffic
    /// (invariant I1: the two profiles must not diverge on this).
    ///
    /// The synchronous [`status`](LeaderWatch::status) / [`is_leader`](LeaderWatch::is_leader)
    /// snapshot stays lossless regardless; this is confined to the event stream.
    /// A `Lagged` that still does not fit stays owed — the debt is only ever added
    /// to, never lost.
    pub fn flush_lagged(&mut self) {
        if self.dropped > 0
            && self
                .events
                .try_send(LeaderWatchEvent::Lagged {
                    dropped: self.dropped,
                })
                .is_ok()
        {
            self.dropped = 0;
        }
    }

    /// The one non-blocking send path: pay any outstanding lag notice, then offer
    /// `event`.
    ///
    /// The `Lagged` goes first so the consumer is told to re-read *before* it sees
    /// the next event rather than after — the same ordering the cache pumps use. A
    /// `Lagged` that does not fit stays owed; the debt is only ever added to,
    /// never lost.
    fn offer(&mut self, event: LeaderWatchEvent) -> bool {
        self.flush_lagged();
        match self.events.try_send(event) {
            Ok(()) => true,
            // The consumer is behind. Drop, account, and carry on: the claim must
            // not depend on the consumer's read cadence.
            Err(mpsc::error::TrySendError::Full(_)) => {
                self.dropped = self.dropped.saturating_add(1);
                true
            }
            // The consumer dropped the watch; the subscription is over and the
            // caller should tear down.
            Err(mpsc::error::TrySendError::Closed(_)) => false,
        }
    }

    /// Performs the graceful-shutdown revocation sequence (DESIGN §3.13,
    /// `cpt-cf-clst-fr-shutdown-revoke`) **without blocking on a slow consumer**.
    ///
    /// When `was_leader`, the snapshot is latched to [`LeaderStatus::Lost`] so a
    /// synchronous [`status`](LeaderWatch::status) observes the loss immediately,
    /// and a terminal `Status(Lost)` is delivered. A terminal `Closed(Shutdown)`
    /// is then delivered. Both events are sent through the buffer headroom
    /// reserved at [`channel`](LeaderWatch::channel) construction
    /// ([`TERMINAL_HEADROOM`]), so a backed-up event buffer can neither stall
    /// shutdown (the permit `send` is non-blocking) nor drop the terminal events:
    /// a pure event-stream consumer is guaranteed to observe the distinct
    /// `Status(Lost)` before `Closed(Shutdown)` (ADR-003), not just the snapshot
    /// guard / `changed()` synthesis that gate-pattern consumers rely on.
    ///
    /// Idempotent: the permits are consumed on the first call, so a second call
    /// (or a call after the consumer dropped the watch, which the permit `send`
    /// silently no-ops) does nothing further.
    pub fn revoke_for_shutdown(&mut self, was_leader: bool) {
        if was_leader {
            // Latches even with no receiver; a racing `status()` cannot observe
            // stale leadership after this returns.
            self.snapshot.send_replace(LeaderStatus::Lost);
            if let Some(permit) = self.lost_permit.take() {
                // `send` consumes the permit and returns the underlying sender,
                // which we drop — its reserved slot has served its purpose.
                let _sender = permit.send(LeaderWatchEvent::Status(LeaderStatus::Lost));
            }
        }
        if let Some(permit) = self.closed_permit.take() {
            let _sender = permit.send(LeaderWatchEvent::Closed(ClusterError::Shutdown));
        }
    }
}

/// The backend-side receiver of consumer [`LeaderWatch::resign`] requests,
/// paired by [`LeaderWatch::channel`]. The backend's renewal task selects on
/// [`recv`](Self::recv) and, on a request, releases the claim and replies with
/// the result.
#[derive(Debug)]
pub struct ResignReceiver {
    rx: mpsc::Receiver<ResignRequest>,
}

impl ResignReceiver {
    /// Awaits the next resign request, or `None` once the consumer has dropped
    /// the watch without resigning (leadership then lapses via TTL). Returns a
    /// [`ResignResponder`] the backend completes after releasing the claim.
    pub async fn recv(&mut self) -> Option<ResignResponder> {
        self.rx
            .recv()
            .await
            .map(|req| ResignResponder { reply: req.reply })
    }
}

/// The reply side of one resign request. The backend calls
/// [`respond`](Self::respond) with the outcome of releasing the claim, which is
/// returned to the consumer from [`LeaderWatch::resign`].
#[derive(Debug)]
pub struct ResignResponder {
    reply: oneshot::Sender<Result<(), ClusterError>>,
}

impl ResignResponder {
    /// Completes the resign request with the release outcome. A dropped
    /// consumer (no longer awaiting) is ignored.
    pub fn respond(self, result: Result<(), ClusterError>) {
        // The consumer may have stopped awaiting (dropped the resign future);
        // delivering to a gone receiver is a no-op, not an error.
        let _outcome = self.reply.send(result);
    }
}

#[cfg(test)]
#[path = "watch_tests.rs"]
mod watch_tests;
