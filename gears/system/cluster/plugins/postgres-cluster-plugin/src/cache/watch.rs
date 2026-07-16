//! LISTEN connection management, per-watcher fan-out, and the write-side
//! `NOTIFY` helper (DESIGN.md §4.3, §2.3).
//!
//! The plugin maintains one dedicated Postgres connection that issues `LISTEN
//! cluster_cache_changes` at startup. An async task reads notifications from
//! this connection in a loop and fans them out to per-watcher channels
//! registered here. Because every instance in the fleet runs its own copy of
//! this task against the same database, a write on any one instance reaches
//! every instance's local watchers via this same NOTIFY round-trip — Postgres
//! delivers a NOTIFY back to the sending session too, as long as that session
//! is itself `LISTENing` on the channel, which this dedicated connection always
//! is. So the mutation methods in `cache/mod.rs` never call into
//! [`WatchRegistry`] directly; they only execute SQL + `pg_notify`, and
//! delivery to local watchers happens the same way it does for watchers on
//! every other instance.
//!
//! **Exact watches only** (DESIGN.md §4.3): the native NOTIFY channel carries a
//! single key per payload, so `watch_prefix` is not serviceable natively —
//! callers get [`ClusterError::Unsupported`] and use `PollingPrefixWatch`
//! instead.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use cluster_sdk::{
    CacheEvent, CacheWatch, CacheWatchEvent, CacheWatchSender, CacheWatchTrySendError,
    ClusterError, ProviderErrorKind,
};
use dashmap::DashMap;
use sqlx::postgres::PgListener;
use tokio_util::sync::CancellationToken;

use crate::pg_error::map_sqlx_error;

/// The Postgres NOTIFY channel this plugin's cache uses (DESIGN.md §4.3).
pub const CHANNEL: &str = "cluster_cache_changes";

/// Postgres's actual, hardcoded NOTIFY payload limit (`MAX_NOTIFY_PAYLOAD_LENGTH`
/// in `src/backend/commands/async.c`), confirmed empirically against a real
/// server while writing `PG-SPEC-002`: `pg_notify('x', repeat('a', 7999))`
/// succeeds, `repeat('a', 8000)` fails with `payload string too long`. This
/// is a real Postgres constant, not `8192` (DESIGN.md §2.3's "8 KB" framing
/// rounds to a nearby power of two but overstates the actual, slightly
/// smaller hard limit by 193 bytes) — using `8192` here let
/// [`validate_key_len`] accept keys the database itself would then reject
/// mid-write, turning a clean startup/validation-time `InvalidName` into a
/// runtime `Provider` error from `pg_notify` instead.
const MAX_NOTIFY_PAYLOAD_BYTES: usize = 7999;
/// `<event_type>:` is always exactly two bytes (a one-character code plus the
/// separator), leaving the rest of the 8 KB budget for the key.
const EVENT_PREFIX_BYTES: usize = 2;
/// The longest key this plugin will accept, so its NOTIFY payload — one byte
/// event code, one byte `:`, then the key — never exceeds
/// [`MAX_NOTIFY_PAYLOAD_BYTES`] (DESIGN.md §2.3, `PG-SPEC-002`).
pub const MAX_KEY_BYTES: usize = MAX_NOTIFY_PAYLOAD_BYTES - EVENT_PREFIX_BYTES;

/// Rejects a key too long to fit this plugin's NOTIFY payload budget
/// (DESIGN.md §2.3). Called at write time by every cache mutation that would
/// otherwise `NOTIFY` an over-long key.
pub fn validate_key_len(key: &str) -> Result<(), ClusterError> {
    // `reason` is a `&'static str`, so the bound is a literal; this assertion
    // keeps it in sync with the actual enforced `MAX_KEY_BYTES` (PGR-M4 — the
    // message previously said 8190 while the code rejected at 7997).
    const _: () = assert!(MAX_KEY_BYTES == 7997);
    if key.len() > MAX_KEY_BYTES {
        return Err(ClusterError::InvalidName {
            name: key.to_owned(),
            reason: "key exceeds the 7997-byte maximum length (the Postgres NOTIFY payload \
                     limit minus the 2-byte event prefix; DESIGN.md sec 2.3)",
        });
    }
    Ok(())
}

/// The `<event_type>` byte of the NOTIFY payload format (DESIGN.md §2.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotifyEvent {
    Changed,
    Deleted,
    Expired,
}

impl NotifyEvent {
    fn code(self) -> char {
        match self {
            Self::Changed => 'C',
            Self::Deleted => 'D',
            Self::Expired => 'E',
        }
    }
}

/// Issues `NOTIFY cluster_cache_changes, '<event_type>:<key>'` (via the
/// parameterized `pg_notify(channel, payload)` function, which avoids any
/// literal-quoting concern for keys containing `'`) on `executor`. Callers run
/// this inside the same transaction as the write it announces (DESIGN.md §4.1)
/// so the notification is never observed without the write that caused it.
pub async fn notify<'e, E>(executor: E, event: NotifyEvent, key: &str) -> Result<(), ClusterError>
where
    E: sqlx::PgExecutor<'e>,
{
    let payload = format!("{}:{key}", event.code());
    sqlx::query("SELECT pg_notify($1, $2)")
        .bind(CHANNEL)
        .bind(payload)
        .execute(executor)
        .await
        .map_err(map_sqlx_error)?;
    Ok(())
}

/// A parsed NOTIFY payload (DESIGN.md §2.3): `<event_type>:<key>`, where
/// `<event_type>` is one of `C` (Changed), `D` (Deleted), `E` (Expired). An
/// empty or otherwise unrecognized payload — a bare `NOTIFY channel` with no
/// payload, an unrelated writer on the same channel, or a future format this
/// plugin's version doesn't know — maps to [`ParsedNotification::Reset`] rather
/// than being treated as a bug to panic on. (NOTIFY queue overflow does *not*
/// reach here as an empty payload: Postgres aborts the committing transaction
/// with an error and broadcasts nothing — overflow recovery is instead the
/// LISTEN task's reconnect-then-`Reset` path.)
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParsedNotification {
    Changed { key: String },
    Deleted { key: String },
    Expired { key: String },
    Reset,
}

/// Parses a raw NOTIFY payload per the `<event_type>:<key>` format (DESIGN.md
/// §2.3). Returns [`ParsedNotification::Reset`] for an empty or malformed
/// payload.
pub fn parse_notification(payload: &str) -> ParsedNotification {
    let Some((event_type, key)) = payload.split_once(':') else {
        return ParsedNotification::Reset;
    };
    match event_type {
        "C" => ParsedNotification::Changed {
            key: key.to_owned(),
        },
        "D" => ParsedNotification::Deleted {
            key: key.to_owned(),
        },
        "E" => ParsedNotification::Expired {
            key: key.to_owned(),
        },
        _ => ParsedNotification::Reset,
    }
}

/// One registered watcher: the sender plus a count of events dropped because
/// its buffer was full, drained as a synthesized [`CacheWatchEvent::Lagged`]
/// the next time delivery succeeds (DESIGN.md §4.3 / the `CacheWatchSender`
/// contract — "the backend should record the drop and emit a `Lagged` once
/// the buffer drains").
struct WatcherSlot {
    sender: CacheWatchSender,
    dropped: AtomicU64,
}

/// Delivers `event` to `slot` via `try_send` (a fan-out path must never block
/// on one slow consumer), first flushing any pending `Lagged` count. Returns
/// `false` when the slot should be pruned (the consumer dropped its
/// [`CacheWatch`]).
fn deliver(slot: &WatcherSlot, event: CacheWatchEvent) -> bool {
    let dropped = slot.dropped.load(Ordering::Relaxed);
    if dropped > 0 {
        match slot.sender.try_send(CacheWatchEvent::Lagged { dropped }) {
            Ok(()) => slot.dropped.store(0, Ordering::Relaxed),
            Err(CacheWatchTrySendError::Full) => {
                slot.dropped.fetch_add(1, Ordering::Relaxed);
                return true;
            }
            Err(CacheWatchTrySendError::Closed) => return false,
        }
    }
    match slot.sender.try_send(event) {
        Ok(()) => true,
        Err(CacheWatchTrySendError::Full) => {
            slot.dropped.fetch_add(1, Ordering::Relaxed);
            true
        }
        Err(CacheWatchTrySendError::Closed) => false,
    }
}

/// Registry of active per-key watchers, keyed by the exact key being watched.
/// The LISTEN fan-out task (spawned by [`spawn_listen_task`]) routes each
/// parsed notification to every sender registered under the notified key.
pub struct WatchRegistry {
    watchers: DashMap<String, Vec<WatcherSlot>>,
}

impl WatchRegistry {
    #[must_use]
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            watchers: DashMap::new(),
        })
    }

    /// Registers a new exact-key watch, returning the [`CacheWatch`] handed
    /// back to the caller of
    /// [`ClusterCacheBackend::watch`](cluster_sdk::cache::ClusterCacheBackend::watch).
    pub fn register(&self, key: &str) -> CacheWatch {
        let (sender, watch) = CacheWatch::channel(64);
        self.watchers
            .entry(key.to_owned())
            .or_default()
            .push(WatcherSlot {
                sender,
                dropped: AtomicU64::new(0),
            });
        watch
    }

    /// Fans a parsed notification out to every watcher on the affected key, or
    /// broadcasts + clears every subscription for [`ParsedNotification::Reset`]
    /// (DESIGN.md §4.3).
    ///
    /// `async` only because the terminal [`Reset`](ParsedNotification::Reset)
    /// branch guarantees delivery (see [`broadcast_and_clear`](Self::broadcast_and_clear));
    /// the per-key `Changed`/`Deleted`/`Expired` fan-out is still a non-blocking
    /// `try_send` and never awaits.
    pub async fn dispatch(&self, notification: &ParsedNotification) {
        match notification {
            ParsedNotification::Changed { key } => {
                self.deliver_to_key(
                    key,
                    &CacheWatchEvent::Event(CacheEvent::Changed { key: key.clone() }),
                );
            }
            ParsedNotification::Deleted { key } => {
                self.deliver_to_key(
                    key,
                    &CacheWatchEvent::Event(CacheEvent::Deleted { key: key.clone() }),
                );
            }
            ParsedNotification::Expired { key } => {
                self.deliver_to_key(
                    key,
                    &CacheWatchEvent::Event(CacheEvent::Expired { key: key.clone() }),
                );
            }
            ParsedNotification::Reset => {
                self.broadcast_and_clear(|| CacheWatchEvent::Reset).await;
            }
        }
    }

    fn deliver_to_key(&self, key: &str, event: &CacheWatchEvent) {
        let Some(mut slots) = self.watchers.get_mut(key) else {
            return;
        };
        slots.retain(|slot| deliver(slot, event.clone()));
        if slots.is_empty() {
            drop(slots);
            self.watchers.remove(key);
        }
    }

    /// Sends `make_event()` (a fresh clone per watcher, since
    /// [`CacheWatchEvent`] carries owned data) to every active watcher across
    /// every key, then clears every subscription — the watcher's own
    /// [`CacheWatch`] handle stays open but will receive nothing further until
    /// its owner calls `watch`/`watch_prefix` again (DESIGN.md §4.3:
    /// "consumers must resubscribe").
    ///
    /// Unlike the per-key [`deliver`] fan-out (which drops on a full buffer and
    /// coalesces a later `Lagged`), this delivers the terminal `Reset`/
    /// `Closed(Shutdown)` event as the **typed** event to every watcher that is
    /// draining — even one whose 64-slot buffer is momentarily full — rather than
    /// letting a full buffer degrade the terminal signal to a bare channel close
    /// (`None`) the consumer can't tell apart from a dropped sender (PGR-C4).
    /// `send` returns immediately when the buffer has room (the common case and
    /// every draining consumer), so the typed event lands at once.
    ///
    /// Each delivery is **bounded** by [`TERMINAL_GRACE`](Self) and they run
    /// concurrently: a consumer that is alive but has stopped draining (full
    /// buffer, watch not dropped) cannot stall shutdown/reset indefinitely — the
    /// blocking `send` used to hang `stop()` forever in that case. After the
    /// grace the sender is dropped and that consumer observes end-of-stream
    /// (`None`); it was not reading, so a reserved slot would not have reached it
    /// either. Senders are cloned out first so no `DashMap` shard lock is held
    /// across an `.await`; the map is cleared before the awaits, and the cloned
    /// senders keep each channel open until the terminal event lands or the grace
    /// elapses. A watcher that already dropped its [`CacheWatch`] returns an error
    /// from `send` immediately and is skipped.
    async fn broadcast_and_clear(&self, make_event: impl Fn() -> CacheWatchEvent) {
        /// Upper bound on how long a single terminal delivery waits for a full
        /// consumer to free a buffer slot before giving up (PGR-C4).
        const TERMINAL_GRACE: Duration = Duration::from_secs(5);

        let senders: Vec<CacheWatchSender> = self
            .watchers
            .iter()
            .flat_map(|entry| {
                entry
                    .value()
                    .iter()
                    .map(|slot| slot.sender.clone())
                    .collect::<Vec<_>>()
            })
            .collect();
        self.watchers.clear();

        let mut deliveries = tokio::task::JoinSet::new();
        for sender in senders {
            let event = make_event();
            deliveries.spawn(async move {
                let _delivered = tokio::time::timeout(TERMINAL_GRACE, sender.send(event)).await;
            });
        }
        while deliveries.join_next().await.is_some() {}
    }

    /// Closes every active watch terminally with [`ClusterError::Shutdown`]
    /// (DESIGN.md §10 step 2, `PG-LIFE-004`) before the LISTEN task exits.
    pub async fn close_all(&self) {
        self.broadcast_and_clear(|| CacheWatchEvent::Closed(ClusterError::Shutdown))
            .await;
    }
}

/// Backoff policy for the LISTEN task's own reconnect loop, used only when
/// [`PgListener`]'s internal (single-attempt) reconnect fails outright
/// (`PG-FAULT-005`) — its transparent same-call reconnect (`PG-FAULT-001`/
/// `PG-FAULT-004`) never reaches this loop at all. Not exposed as a config
/// knob (DESIGN.md §7 has none for it); revisit if operators need to tune it.
struct ListenRetryPolicy {
    initial_backoff: Duration,
    max_backoff: Duration,
    max_retries: u32,
}

impl ListenRetryPolicy {
    const DEFAULT: Self = Self {
        initial_backoff: Duration::from_secs(1),
        max_backoff: Duration::from_secs(30),
        max_retries: 10,
    };

    fn backoff_for(&self, attempt: u32) -> Duration {
        let factor = 2u32.saturating_pow(attempt);
        self.initial_backoff
            .checked_mul(factor)
            .map_or(self.max_backoff, |grown| grown.min(self.max_backoff))
    }
}

/// Establishes the dedicated LISTEN connection and spawns its fan-out loop
/// (DESIGN.md §4.3).
///
/// The initial `connect` + `LISTEN` is done **synchronously, awaited by the
/// caller**, before anything is spawned — not fired-and-forgotten inside the
/// spawned task. `build_and_start` awaits this, so by the time it resolves
/// the LISTEN registration is confirmed live with Postgres, per DESIGN.md
/// §3.2 step 5's guarantee ("by the time `build_and_start` resolves... the
/// LISTEN connection is live — there is no readiness gate or background-init
/// race for callers to reason about"). Doing the initial connect *inside*
/// the spawned task instead (as an earlier version of this function did)
/// broke exactly that guarantee: `tokio::spawn` only schedules the task, so
/// `build_and_start` could return — and a caller's very first `put` could
/// commit and `NOTIFY` — before the spawned task's own `connect_and_listen`
/// had actually finished subscribing, silently losing that NOTIFY forever
/// (Postgres does not queue/replay notifications for a session that starts
/// listening after they fired). `PG-WATCH-007` caught this directly: both
/// watchers on the same key would time out *together* (never just one),
/// exactly matching "the whole notification was never delivered to this
/// session," not a per-watcher delivery bug.
///
/// # Errors
/// Propagates a connection failure from the initial `connect`/`LISTEN` — the
/// caller decides how to treat a LISTEN connection that can't even establish
/// (this plugin's `build_and_start` implementations fail startup on it,
/// rather than starting a cache with no working watch capability).
///
/// [`PgListener`] already reconnects transparently on a single connection
/// blip and re-subscribes to `CHANNEL` — that path surfaces to us as
/// `try_recv()` returning `Ok(None)`, at which point we broadcast `Reset`
/// (events may have been missed during the gap) and resume. If the listener's
/// own reconnect attempt fails outright (`try_recv()` returns `Err`), this
/// task takes over with its own bounded backoff (`PG-FAULT-005`); exhausting
/// it broadcasts `Closed(Provider { kind: ConnectionLost, .. })` and exits.
pub async fn spawn_listen_task(
    connection_string: String,
    registry: Arc<WatchRegistry>,
    cancel: CancellationToken,
) -> Result<tokio::task::JoinHandle<()>, ClusterError> {
    let mut listener = connect_and_listen(&connection_string).await?;

    Ok(tokio::spawn(async move {
        let mut attempt: u32 = 0;
        loop {
            tokio::select! {
                () = cancel.cancelled() => return,
                received = listener.try_recv() => match received {
                    Ok(Some(notification)) => {
                        attempt = 0;
                        registry.dispatch(&parse_notification(notification.payload())).await;
                    }
                    Ok(None) => {
                        // `PgListener`'s own transparent reconnect just ran and
                        // succeeded; events during the gap may have been missed.
                        attempt = 0;
                        registry.dispatch(&ParsedNotification::Reset).await;
                    }
                    Err(_lost) => {
                        match reconnect_with_backoff(&connection_string, &ListenRetryPolicy::DEFAULT, &mut attempt, &cancel).await {
                            Some(reconnected) => {
                                listener = reconnected;
                                registry.dispatch(&ParsedNotification::Reset).await;
                            }
                            // `None` means either the retry budget is exhausted
                            // or `cancel` fired mid-backoff (graceful shutdown,
                            // not a connection-loss failure) — only the former
                            // is a real `Closed(ConnectionLost)`.
                            None if cancel.is_cancelled() => return,
                            None => {
                                registry.broadcast_and_clear(|| {
                                    CacheWatchEvent::Closed(ClusterError::Provider {
                                        kind: ProviderErrorKind::ConnectionLost,
                                        message: "LISTEN connection reconnect retry budget exhausted"
                                            .to_owned(),
                                    })
                                }).await;
                                return;
                            }
                        }
                    }
                },
            }
        }
    }))
}

async fn connect_and_listen(connection_string: &str) -> Result<PgListener, ClusterError> {
    let mut listener = PgListener::connect(connection_string)
        .await
        .map_err(map_sqlx_error)?;
    listener.listen(CHANNEL).await.map_err(map_sqlx_error)?;
    Ok(listener)
}

/// Retries [`connect_and_listen`] with exponential backoff, up to
/// `policy.max_retries` attempts. Returns `None` once the budget is exhausted
/// *or* `cancel` fires mid-backoff — either way the caller's shutdown path
/// (`return` on `cancel.cancelled()`, checked again on the next loop
/// iteration) takes it from there rather than a fabricated `Closed` event.
async fn reconnect_with_backoff(
    connection_string: &str,
    policy: &ListenRetryPolicy,
    attempt: &mut u32,
    cancel: &CancellationToken,
) -> Option<PgListener> {
    while *attempt < policy.max_retries {
        let backoff = policy.backoff_for(*attempt);
        *attempt += 1;
        tokio::select! {
            () = cancel.cancelled() => return None,
            () = tokio::time::sleep(backoff) => {}
        }
        if let Ok(listener) = connect_and_listen(connection_string).await {
            return Some(listener);
        }
    }
    None
}

#[cfg(test)]
#[path = "watch_tests.rs"]
mod tests;
