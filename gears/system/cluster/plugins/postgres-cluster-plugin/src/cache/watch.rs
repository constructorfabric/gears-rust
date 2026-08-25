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

use cluster_sdk::observability::{logs, primitive};
use cluster_sdk::{
    CacheEvent, CacheWatch, CacheWatchEvent, CacheWatchSender, CacheWatchTrySendError,
    ClusterError, ClusterMetrics, ProviderErrorKind,
};
use dashmap::DashMap;
use rand::RngExt as _;
use sqlx::postgres::PgListener;
use tokio_util::sync::CancellationToken;

use crate::limits::MAX_INDEXED_KEY_BYTES;
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
/// The longest key the NOTIFY payload budget alone would permit, so a payload —
/// one byte event code, one byte `:`, then the key — never exceeds
/// [`MAX_NOTIFY_PAYLOAD_BYTES`] (DESIGN.md §2.3).
const MAX_NOTIFY_KEY_BYTES: usize = MAX_NOTIFY_PAYLOAD_BYTES - EVENT_PREFIX_BYTES;

/// The longest key this plugin will accept (DESIGN.md §2.3, `PG-SPEC-002`).
///
/// Two independent Postgres limits apply to a cache key and the tighter one has
/// to win. The NOTIFY payload budget ([`MAX_NOTIFY_KEY_BYTES`], 7997) is the one
/// this constant used to be defined by outright, but `cluster_cache.key` is also
/// a `PRIMARY KEY`, so every key lands in a btree bound by the much smaller
/// index-tuple ceiling ([`MAX_INDEXED_KEY_BYTES`], see `limits.rs`). Bounding
/// only by the NOTIFY budget left a window — roughly 2705..=7997 bytes — where a
/// key passed this guard and then failed inside Postgres with SQLSTATE `54000`
/// mid-write, which is precisely what the guard exists to prevent.
pub const MAX_KEY_BYTES: usize = if MAX_NOTIFY_KEY_BYTES < MAX_INDEXED_KEY_BYTES {
    MAX_NOTIFY_KEY_BYTES
} else {
    MAX_INDEXED_KEY_BYTES
};

/// Rejects a key too long for this plugin's storage or notification budget
/// (DESIGN.md §2.3). Called at write time by every cache mutation, so an
/// over-long key fails as a clean `InvalidName` before the row is written and
/// before an un-sendable `NOTIFY` is attempted.
pub fn validate_key_len(key: &str) -> Result<(), ClusterError> {
    // `reason` is a `&'static str`, so the bound is a literal; this assertion
    // keeps it in sync with the actual enforced `MAX_KEY_BYTES`.
    const _: () = assert!(MAX_KEY_BYTES == 2048);
    if key.len() > MAX_KEY_BYTES {
        return Err(ClusterError::InvalidName {
            name: key.to_owned(),
            reason: "key exceeds the 2048-byte maximum length (the btree index-tuple limit on \
                     the cluster_cache primary key; DESIGN.md sec 2.1)",
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

/// [`notify`] for a batch of keys sharing one `event`, in **one** round-trip —
/// the cache TTL sweeper's form (`cache::reaper::sweep_chunk`).
///
/// `unnest` rather than a loop of single `pg_notify` calls, mirroring what
/// `lock::notify::notify_released_many` already does for the lock sweep. The
/// point of chunking that sweep was to bound how long one transaction holds row
/// locks on every key it is deleting, and a sequential `pg_notify` round-trip per
/// deleted key inside that transaction worked directly against it: a chunk's
/// lock-hold time scaled with the number of expired keys rather than staying flat
/// (DESIGN.md §11 lists this as an open improvement).
///
/// Payloads are formatted here rather than concatenated in SQL, so the
/// `<event_type>:<key>` format lives in exactly one place ([`NotifyEvent::code`]).
/// Postgres de-duplicates identical `(channel, payload)` pairs within a
/// transaction; keys come from a `RETURNING` on a primary-key column, so there are
/// no duplicates to fold anyway.
pub async fn notify_many<'e, E>(
    executor: E,
    event: NotifyEvent,
    keys: &[String],
) -> Result<(), ClusterError>
where
    E: sqlx::PgExecutor<'e>,
{
    let payloads: Vec<String> = keys
        .iter()
        .map(|key| format!("{}:{key}", event.code()))
        .collect();
    sqlx::query("SELECT pg_notify($1, payload) FROM unnest($2::text[]) AS t(payload)")
        .bind(CHANNEL)
        .bind(&payloads)
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
    /// Process-unique, so [`WatchRegistry::register`] can withdraw *its own* slot
    /// when it loses the race against a terminal close — see there.
    id: u64,
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

/// The DESIGN.md §8-contracted watch-reset signals — `cluster_watch_resets_total`
/// plus the `cluster.watch.reset` WARN — bundled so they can be handed to the
/// detached `Reset` fan-out and emitted *there*, once delivery has happened.
///
/// A value rather than a closure because it has to be `Clone + Send + 'static`:
/// one is moved into each spawned broadcast. The `RestartingWatch` combinator in
/// `cluster-sdk` does not cover any of this — it reacts only to a terminal
/// `Closed`, never to the listener's own internal `Reset`.
#[derive(Clone)]
pub struct WatchResetSignal {
    metrics: Arc<dyn ClusterMetrics>,
    provider: &'static str,
}

impl WatchResetSignal {
    fn emit(&self) {
        self.metrics.watch_reset(primitive::CACHE);
        tracing::warn!(
            name: logs::WATCH_RESET,
            provider = %self.provider,
            primitive = primitive::CACHE,
            "cluster watch reset"
        );
    }
}

/// Registry of active per-key watchers, keyed by the exact key being watched.
/// The LISTEN fan-out task (spawned by [`spawn_listen_task`]) routes each
/// parsed notification to every sender registered under the notified key.
pub struct WatchRegistry {
    watchers: DashMap<String, Vec<WatcherSlot>>,
    /// Source of [`WatcherSlot::id`].
    next_id: AtomicU64,
    /// Serializes every terminal broadcast — the `Reset` fan-out and
    /// [`close_all`](Self::close_all) — against each other.
    ///
    /// Without it those two interleave, and both interleavings are wrong. A
    /// `Reset` broadcast that had already collected its senders when `close_all`
    /// ran would `send` on the same channels *after* the terminal
    /// `Closed(Shutdown)`, which the SDK's `CacheWatch` contract forbids
    /// (`cluster-sdk/src/cache/watch.rs`: nothing follows a `Closed`). The other
    /// order is no better: the `Reset` empties the map first, so `close_all` finds
    /// nothing and delivers no terminal event at all — DESIGN.md §10 step 2 /
    /// `PG-LIFE-004`.
    ///
    /// It also gives `stop()` something to wait on. The `Reset` fan-out is
    /// deliberately run in a detached task (see
    /// [`dispatch_from_listener`](Self::dispatch_from_listener)), so nothing else
    /// joins it, and it can hold `TERMINAL_GRACE` past `stop()`. `close_all`
    /// blocking on this mutex is what turns that into a bounded wait `stop()`
    /// actually observes.
    terminal: tokio::sync::Mutex<()>,
    /// Set, under `terminal`, by the first terminal broadcast, to the very error
    /// that broadcast delivered. A registry that has closed stays closed: later
    /// `Reset` broadcasts are suppressed, and a `watch` arriving afterwards is
    /// handed its terminal event immediately rather than registering into a map
    /// nothing will ever dispatch to again.
    ///
    /// It holds the error rather than a bare flag because *which* error closed the
    /// registry is load-bearing: `close_all` closes with
    /// [`ClusterError::Shutdown`], while an exhausted LISTEN retry budget closes
    /// with `Provider { kind: ConnectionLost, .. }`, and only the latter is
    /// [`ClusterError::is_retryable`]. A late registration answered with a
    /// hardcoded `Shutdown` would tell `RestartingWatch` the subsystem is going
    /// away when the truth is a lost connection, so the consumer's own retry
    /// policy never gets to run and the reported cause is wrong.
    closed: std::sync::OnceLock<ClusterError>,
}

impl WatchRegistry {
    #[must_use]
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            watchers: DashMap::new(),
            next_id: AtomicU64::new(0),
            terminal: tokio::sync::Mutex::new(()),
            closed: std::sync::OnceLock::new(),
        })
    }

    /// Registers a new exact-key watch, returning the [`CacheWatch`] handed
    /// back to the caller of
    /// [`ClusterCacheBackend::watch`](cluster_sdk::cache::ClusterCacheBackend::watch).
    ///
    /// A `watch()` landing during or after [`close_all`](Self::close_all) must not
    /// produce a watcher that silently receives nothing forever, so registration
    /// is a check-insert-recheck against the [`closed`](Self::closed) flag. The
    /// recheck is what makes it airtight: this method is synchronous and cannot
    /// take the async `terminal` mutex, so the flag can be set between the first
    /// check and the insert. Losing that race is detected afterwards, and resolved
    /// by *whoever actually holds the slot* — [`take_slot`](Self::take_slot)
    /// returns `true` only if this call removed it, which means the terminal
    /// broadcast did not collect it and this call owes the watcher its terminal
    /// event. `false` means the broadcast took it and has already sent one, so
    /// sending again here would be the duplicate.
    ///
    /// The terminal event handed over on that path is the one the closing
    /// broadcast used, not a fixed `Shutdown` — see
    /// [`closed`](Self::closed) for why the distinction matters to the caller.
    pub fn register(&self, key: &str) -> CacheWatch {
        let (sender, watch) = CacheWatch::channel(64);
        if !self.is_closed() {
            let id = self.next_id.fetch_add(1, Ordering::Relaxed);
            self.watchers
                .entry(key.to_owned())
                .or_default()
                .push(WatcherSlot {
                    id,
                    sender: sender.clone(),
                    dropped: AtomicU64::new(0),
                });
            if !self.is_closed() || !self.take_slot(key, id) {
                return watch;
            }
        }
        // The registry is closed and this watcher's slot is ours to answer for.
        // `try_send` on a channel with an empty 64-slot buffer always has room.
        let _delivered = sender.try_send(CacheWatchEvent::Closed(self.terminal_error()));
        watch
    }

    /// Whether a terminal broadcast has already run.
    fn is_closed(&self) -> bool {
        self.closed.get().is_some()
    }

    /// The error the terminal broadcast closed with, for replaying to a
    /// registration that arrived too late to be collected by it.
    ///
    /// Falls back to [`ClusterError::Shutdown`] only for the unreachable case of
    /// being called before any terminal broadcast ran: every caller reaches this
    /// through an [`is_closed`](Self::is_closed) check, and `closed` is set under
    /// `terminal` before the broadcast collects a single sender.
    fn terminal_error(&self) -> ClusterError {
        self.closed.get().cloned().unwrap_or(ClusterError::Shutdown)
    }

    /// Removes the slot with `id` under `key`, reporting whether it was still
    /// there. See [`register`](Self::register) for why the answer matters.
    fn take_slot(&self, key: &str, id: u64) -> bool {
        let Some(mut slots) = self.watchers.get_mut(key) else {
            return false;
        };
        let before = slots.len();
        slots.retain(|slot| slot.id != id);
        let removed = slots.len() != before;
        if slots.is_empty() {
            drop(slots);
            self.watchers.remove(key);
        }
        removed
    }

    /// Fans a parsed notification out to every watcher on the affected key, or
    /// broadcasts + clears every subscription for [`ParsedNotification::Reset`]
    /// (DESIGN.md §4.3).
    ///
    /// `async` only because the terminal [`Reset`](ParsedNotification::Reset)
    /// branch guarantees delivery (see [`broadcast_and_clear`](Self::broadcast_and_clear));
    /// the per-key `Changed`/`Deleted`/`Expired` fan-out is still a non-blocking
    /// `try_send` and never awaits.
    ///
    /// **Test-only.** Production dispatch goes through
    /// [`dispatch_from_listener`](Self::dispatch_from_listener), which must not
    /// await the `Reset` path. This awaited form is what the unit tests want:
    /// spawning the broadcast would make "assert every watcher received `Reset`"
    /// a race against a detached task rather than a fact on return.
    #[cfg(test)]
    pub async fn dispatch(&self, notification: &ParsedNotification) {
        if matches!(notification, ParsedNotification::Reset) {
            let _broadcast = self.broadcast_and_clear(None).await;
            return;
        }
        self.deliver_event(notification);
    }

    /// [`dispatch`](Self::dispatch) as the **LISTEN reader loop** must call it:
    /// the per-key fan-out inline, a terminal `Reset` broadcast spawned.
    ///
    /// The reader loop must not `await` the `Reset` path. `broadcast_and_clear`
    /// is bounded by `TERMINAL_GRACE` (5s) per watcher that is alive but not
    /// draining, and for however long it runs this session reads *no*
    /// notifications. A `LISTEN` session that stops reading pins the tail of the
    /// notify queue — which is **cluster-wide**, shared by every database in the
    /// Postgres instance, and truncated only as far as its slowest listener
    /// allows. So a single non-draining consumer here could stall the queue for
    /// the whole cluster, and once it filled, *every* notifying commit anywhere
    /// on that server would start failing with "too many notifications in the
    /// NOTIFY queue". Not stalling the drain is the part this plugin controls
    /// (DESIGN.md §11).
    ///
    /// Spawning costs the strict ordering between a `Reset` and the events after
    /// it, which is sound because `Reset` is a superset signal, not a positional
    /// one: a watcher that receives a later `Changed` *before* the `Reset` still
    /// re-reads on the `Reset`. What is *not* spawned is `close_all` on the
    /// shutdown path — nothing needs draining then, and `stop()` genuinely wants
    /// to await delivery.
    ///
    /// What spawning must **not** cost is the terminal ordering, and that is what
    /// [`terminal`](Self::terminal) enforces: a `Reset` task that started before
    /// `stop()` cancelled either wins the mutex and completes before `close_all`
    /// collects anything, or loses it and is suppressed outright — never `Reset`
    /// after `Closed`, and never an emptied registry for `close_all` to find.
    ///
    /// `signal` is invoked only once the broadcast has actually been delivered, so
    /// `cluster_watch_resets_total` counts resets watchers were given rather than
    /// resets that were merely scheduled. Emitting it at the call site (as this
    /// used to) counted a reset before the fan-out was even spawned, including the
    /// ones a concurrent shutdown then suppressed.
    pub fn dispatch_from_listener(
        self: &Arc<Self>,
        notification: &ParsedNotification,
        signal: &WatchResetSignal,
    ) {
        if matches!(notification, ParsedNotification::Reset) {
            let registry = Arc::clone(self);
            let signal = signal.clone();
            tokio::spawn(async move {
                if registry.broadcast_and_clear(None).await {
                    signal.emit();
                }
            });
            return;
        }
        self.deliver_event(notification);
    }

    /// The non-terminal half of [`dispatch`](Self::dispatch): per-key fan-out for
    /// `Changed`/`Deleted`/`Expired`.
    ///
    /// Synchronous, because every send on this path is a non-blocking `try_send`
    /// — which is precisely what makes it safe to run inline in the reader loop.
    /// `Reset` is not a per-key event and is a no-op here; both callers above
    /// handle it themselves.
    fn deliver_event(&self, notification: &ParsedNotification) {
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
            ParsedNotification::Reset => {}
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
    /// either. Senders are taken out first so no `DashMap` shard lock is held
    /// across an `.await`, and they keep each channel open until the terminal
    /// event lands or the grace elapses. A watcher that already dropped its
    /// [`CacheWatch`] returns an error from `send` immediately and is skipped.
    ///
    /// `Some(err)` makes this a terminal `Closed(err)` — an event nothing may
    /// follow. It latches [`closed`](Self::closed) to `err`, which suppresses
    /// every later broadcast and hands `err` itself to later registrations;
    /// `None` is the non-terminal `Reset` fan-out. Returns whether the broadcast
    /// ran at all; `false` means the registry was already closed and the caller's
    /// event was deliberately dropped.
    async fn broadcast_and_clear(&self, terminal: Option<ClusterError>) -> bool {
        /// Upper bound on how long a single terminal delivery waits for a full
        /// consumer to free a buffer slot before giving up (PGR-C4).
        const TERMINAL_GRACE: Duration = Duration::from_secs(5);

        // Held across the whole broadcast — collection *and* delivery. See
        // [`terminal`](Self::terminal) for the two interleavings this excludes.
        let _serialized = self.terminal.lock().await;
        if self.is_closed() {
            return false;
        }
        if let Some(err) = terminal.clone() {
            let _latched = self.closed.set(err);
        }

        let senders = self.drain_senders();
        let mut deliveries = tokio::task::JoinSet::new();
        for sender in senders {
            let event = match &terminal {
                Some(err) => CacheWatchEvent::Closed(err.clone()),
                None => CacheWatchEvent::Reset,
            };
            deliveries.spawn(async move {
                let _delivered = tokio::time::timeout(TERMINAL_GRACE, sender.send(event)).await;
            });
        }
        while deliveries.join_next().await.is_some() {}
        true
    }

    /// Removes every registered watcher and returns their senders.
    ///
    /// Key by key via [`DashMap::remove`], **not** `iter()` then `clear()`. Those
    /// two are separate operations with no atomicity between them, and
    /// [`register`](Self::register) coordinates with neither: a watcher registering
    /// in the gap was collected by neither the iteration nor — having been
    /// inserted after it — spared by the clear. It was simply removed, with no
    /// event ever delivered, so its consumer saw end-of-stream `None` instead of
    /// `Reset`. `None` documents as "the sender was dropped", which gives a
    /// consumer handling `Reset` no reason to resubscribe: a raw [`CacheWatch`]
    /// user (no `auto_restart`) stopped receiving events permanently. The race
    /// predates spawning the `Reset` fan-out but that widened it from an inline
    /// section to a scheduling boundary.
    ///
    /// `remove` is atomic per key against `entry().or_default().push()`, which
    /// holds the same shard lock, so every registration now falls cleanly on one
    /// side: either it is collected here and receives the event, or it lands under
    /// an already-removed key and stays registered for whatever comes next.
    /// Neither outcome silently drops it.
    fn drain_senders(&self) -> Vec<CacheWatchSender> {
        let keys: Vec<String> = self
            .watchers
            .iter()
            .map(|entry| entry.key().clone())
            .collect();
        let mut senders = Vec::new();
        for key in keys {
            if let Some((_, slots)) = self.watchers.remove(&key) {
                senders.extend(slots.into_iter().map(|slot| slot.sender));
            }
        }
        senders
    }

    /// Closes every active watch terminally with [`ClusterError::Shutdown`]
    /// (DESIGN.md §10 step 2, `PG-LIFE-004`) before the LISTEN task exits, and
    /// latches the registry closed so nothing can follow it.
    pub async fn close_all(&self) {
        let _broadcast = self.broadcast_and_clear(Some(ClusterError::Shutdown)).await;
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
        let base = self
            .initial_backoff
            .checked_mul(factor)
            .map_or(self.max_backoff, |grown| grown.min(self.max_backoff));
        // Full jitter: every instance in the fleet runs this LISTEN task
        // against the same database, so without jitter a shared blip has every
        // instance retry on the same deterministic schedule and their
        // reconnect attempts stampede together.
        let u: f32 = rand::rng().random();
        base.mul_f32(1.0 - u)
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
///
/// Every `Reset` dispatched by this task — both DESIGN.md §4.3 triggers, the
/// empty/unrecognized-payload fallback and a LISTEN connection gap — also
/// emits the DESIGN.md §8-contracted `ClusterMetrics::watch_reset("cache")`
/// (backing `cluster_watch_resets_total`) and a `cluster.watch.reset` WARN
/// log, labelled with `provider`.
pub async fn spawn_listen_task(
    connection_string: String,
    registry: Arc<WatchRegistry>,
    metrics: Arc<dyn ClusterMetrics>,
    provider: &'static str,
    cancel: CancellationToken,
) -> Result<tokio::task::JoinHandle<()>, ClusterError> {
    let mut listener = connect_and_listen(&connection_string).await?;

    Ok(tokio::spawn(async move {
        // Carried into each spawned `Reset` fan-out and emitted there, once the
        // broadcast has actually been delivered — see `dispatch_from_listener`.
        let reset_signal = WatchResetSignal { metrics, provider };

        let mut attempt: u32 = 0;
        loop {
            tokio::select! {
                () = cancel.cancelled() => return,
                received = listener.try_recv() => match received {
                    Ok(Some(notification)) => {
                        attempt = 0;
                        // An empty/unrecognized payload also parses to `Reset`
                        // (DESIGN.md §4.3's "empty/unrecognized payload"
                        // trigger, distinct from the connection-loss trigger
                        // below) — §8 contracts the watch-reset signals for
                        // both, and `dispatch_from_listener` emits them on the
                        // `Reset` branch only.
                        let parsed = parse_notification(notification.payload());
                        // Never `await`ed from this loop — see
                        // `dispatch_from_listener` for why a stalled drain is a
                        // cluster-wide problem, not a local one.
                        registry.dispatch_from_listener(&parsed, &reset_signal);
                    }
                    Ok(None) => {
                        // `PgListener`'s own transparent reconnect just ran and
                        // succeeded; events during the gap may have been missed.
                        attempt = 0;
                        registry.dispatch_from_listener(&ParsedNotification::Reset, &reset_signal);
                    }
                    Err(_lost) => {
                        match reconnect_with_backoff(&connection_string, &ListenRetryPolicy::DEFAULT, &mut attempt, &cancel).await {
                            Some(reconnected) => {
                                listener = reconnected;
                                registry.dispatch_from_listener(&ParsedNotification::Reset, &reset_signal);
                            }
                            // `None` means either the retry budget is exhausted
                            // or `cancel` fired mid-backoff (graceful shutdown,
                            // not a connection-loss failure) — only the former
                            // is a real `Closed(ConnectionLost)`.
                            None if cancel.is_cancelled() => return,
                            None => {
                                // Terminal: latches the registry closed on this
                                // error, so a `Reset` still in flight cannot
                                // follow it and a later `watch()` is answered
                                // immediately — with this same retryable
                                // `ConnectionLost`, not a `Shutdown`.
                                let _broadcast = registry.broadcast_and_clear(Some(
                                    ClusterError::Provider {
                                        kind: ProviderErrorKind::ConnectionLost,
                                        message: "LISTEN connection reconnect retry budget exhausted"
                                            .to_owned(),
                                    },
                                )).await;
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

/// [`connect_and_listen`], but abandoned the instant `cancel` fires so a
/// shutdown never stalls waiting on a hung `PgListener::connect` network I/O.
/// Returns `None` if cancellation won the race — the caller must stop rather
/// than treat it as a connect failure to retry. Mirrors
/// `lock/notify.rs::connect_and_listen_cancellable`; kept as a separate copy
/// for the same reason that module gives for not sharing `RetryPolicy`
/// (`lock/notify.rs` lines ~179-184) — the two channels' payload formats
/// differ enough that unifying the tasks would add more indirection than it
/// would save.
async fn connect_and_listen_cancellable(
    connection_string: &str,
    cancel: &CancellationToken,
) -> Option<Result<PgListener, ClusterError>> {
    tokio::select! {
        () = cancel.cancelled() => None,
        result = connect_and_listen(connection_string) => Some(result),
    }
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
        match connect_and_listen_cancellable(connection_string, cancel).await {
            // Cancelled mid-connect: stop, don't spin the backoff loop.
            None => return None,
            Some(Ok(listener)) => return Some(listener),
            // Connect failed: fall through to the next backoff attempt.
            Some(Err(_lost)) => {}
        }
    }
    None
}

#[cfg(test)]
#[path = "watch_tests.rs"]
mod tests;
