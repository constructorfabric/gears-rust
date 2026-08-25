//! The **liveness beacon** (DESIGN.md §5.1): one advisory lock per *instance*,
//! held on a dedicated connection outside the pool, used for nothing except
//! proving this process is alive.
//!
//! ## What it is, and what it is not
//!
//! The beacon is not a lock. It is a **crash-triggered tombstone**, and an
//! advisory lock is the only Postgres primitive that is all four of:
//!
//! 1. **Established by one statement** — no schema, no row, no WAL, and no
//!    write ever again.
//! 2. **Readable from any other session in SQL** (`pg_locks`), and therefore
//!    joinable inside the acquire statement's own predicate. The liveness test
//!    is atomic with the acquire decision rather than a second round-trip to
//!    race against.
//! 3. **Deleted by the server** the instant the session ends. Nothing has to
//!    maintain it — no heartbeat keeping it alive, no TTL, no reaper. (The ping
//!    below is not maintenance; see "Why the ping".)
//! 4. **Unfiltered by privilege** — `pg_locks` shows every lock in the cluster
//!    to every role, unlike `pg_stat_activity`, which nulls most columns for
//!    other roles' sessions.
//!
//! Property 3 is the whole point: it is what gives a crashed holder's locks back
//! to the fleet without waiting out their TTL, and it is the one property that
//! cannot be rebuilt in application code without reintroducing an interval loop,
//! a heartbeat TTL, and a reaper for it.
//!
//! The key is ours to choose, so it is drawn fresh per **incarnation**. That is
//! what makes every row bearing it provably one this process wrote, on this
//! connection — the basis for the orphan sweep, the shutdown drain, and the
//! post-reconnect cleanup, none of which need any other fence.
//!
//! ## Hard invariants
//!
//! * **The beacon is never released explicitly.** Not when a lock is released,
//!   not when one expires, not at shutdown. Releasing it while this process is
//!   still running asserts that the instance is *dead*, and every lock it holds
//!   becomes stealable on sight. Closing the connection is the only correct way
//!   for it to end, and at shutdown that happens anyway.
//! * **Never the blocking `pg_advisory_lock`.** A blocking form would park this
//!   task inside Postgres waiting for a key nobody can hand over. Only
//!   `pg_try_advisory_lock`, whose `false` here means a key collision — draw
//!   another and retry.
//! * **The connection carries no lock traffic at all.** No lock name, no
//!   `holder_id`, no row, ever. It needs no request queue, no actor mailbox, no
//!   `search_path`, and — because it writes nothing — no `synchronous_commit`
//!   assertion or re-assertion either.
//!
//! ## Why the ping
//!
//! An idle `PgConnection` is never polled, so a beacon whose backend died is not
//! noticed by *this* process until something tries to use it — and nothing ever
//! does. Without local detection the semantics would quietly weaken to "you
//! remain the holder until someone with a live beacon takes it from you": our
//! own `renew` would keep matching its row and reporting success, because the
//! row still carries our `holder_id` and our now-dead key. No mutual exclusion
//! is violated — the moment another instance steals, the `holder_id` fence makes
//! the next `renew` fail — but the instance would go on believing it holds a
//! lock it can no longer defend, for as long as nobody else wants it.
//!
//! So this task pings its own connection once a second and treats a failed ping
//! as loss. The server needs no such prompting: it releases the beacon at
//! disconnect regardless. The ping is how *we* find out.
//!
//! ## The real bound on "immediate"
//!
//! A clean exit or socket close releases the beacon at once. A hard kill or a
//! partition that leaves the TCP connection half-open leaves the backend blocked
//! in `recv`, still holding the beacon, until keepalives fire — so the honest
//! statement of the guarantee is "immediate on clean disconnect,
//! keepalive-bounded otherwise, TTL-bounded in the worst case".
//!
//! Tightening that bound is a **server-side** lever, which is why
//! [`TCP_KEEPALIVE_SETTINGS`] are `SET` on this session rather than configured
//! on the client socket. Client-side `keepalives_*` would help us detect a dead
//! *server*, which the ping above already does faster; what matters here is
//! Postgres noticing that **we** are gone.

use std::sync::Arc;
use std::sync::atomic::{AtomicI32, AtomicU64, Ordering};
use std::time::Duration;

use cluster_sdk::observability::{self, ResourceId};
use cluster_sdk::{ClusterError, ClusterMetrics, ProviderErrorKind};
use dashmap::DashMap;
use sqlx::AssertSqlSafe;
use sqlx::{Connection, PgConnection, PgPool};
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};
use uuid::Uuid;

use crate::pg_error::map_sqlx_error;

/// How often this task pings its own connection — see the module doc's "Why the
/// ping".
///
/// A fixed constant rather than a config knob, and for the same reason the lock
/// session's health check was: this paces how long the instance can believe it
/// holds locks Postgres has already released, which is a correctness-adjacent
/// property rather than a tuning dial. One round-trip per second on a dedicated
/// connection is negligible next to what it bounds.
///
/// A cadence, not a hard bound: the timer uses `MissedTickBehavior::Delay`, so a
/// slow tick restarts the interval from when it finished rather than firing a
/// burst of catch-up pings at a backend that is already struggling. Worst-case
/// detection is therefore this interval *plus* [`STATEMENT_TIMEOUT`].
const PING_INTERVAL: Duration = Duration::from_secs(1);

/// Upper bound on one ping.
///
/// Client-side, deliberately, and **not** a server-side `statement_timeout`: the
/// failure this exists for is a peer that has stopped answering (paused,
/// partitioned, blackholed), and such a peer cannot enforce its own timeout.
/// `sqlx` applies no read timeout of its own, so without this an unresponsive
/// round-trip parks this task — and `stop()` with it — indefinitely.
///
/// Exceeding it is read as a lost connection, which is the only safe reading: a
/// timed-out statement leaves the wire protocol in an indeterminate state, so
/// the connection must be discarded rather than reused. Postgres releases the
/// beacon at that disconnect, exactly as for any other loss.
const STATEMENT_TIMEOUT: Duration = Duration::from_secs(5);

/// Upper bound on one connect attempt. More generous than [`STATEMENT_TIMEOUT`]
/// because establishing is TCP + TLS + startup/auth + the `SET`s + the lock —
/// several round-trips rather than one. `connect` has no timeout of its own, and
/// a blackholed peer leaves a TCP connect pending for minutes.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Upper bound on the graceful close in this task's epilogue. Short: the close
/// is a courtesy, and dropping the socket releases the beacon just the same —
/// what must not happen is `stop()` blocking on it.
const CLOSE_TIMEOUT: Duration = Duration::from_secs(2);

/// First reconnect delay after the beacon is lost; doubles up to
/// [`MAX_RECONNECT_BACKOFF`].
const INITIAL_RECONNECT_BACKOFF: Duration = Duration::from_millis(200);

/// Ceiling on the reconnect delay. The task retries **indefinitely** while it
/// has not been shut down: a lock backend with no beacon has no degraded mode —
/// it can acquire nothing until the beacon is back — so the only useful
/// behaviour is to keep trying at a bounded rate while failing acquisitions
/// fast.
const MAX_RECONNECT_BACKOFF: Duration = Duration::from_secs(5);

/// The `ResourceId` every beacon-level failure is attributed to. The beacon is
/// scoped to no lock name at all, so a bounded stand-in keeps the label
/// cardinality at one (ADR-004).
const BEACON_RESOURCE: &str = "lock_beacon";

/// How many keys to draw before giving up on establishing a beacon.
///
/// `pg_try_advisory_lock` returning `false` for a freshly random 62-bit key
/// means a collision with another live instance, which is vanishingly unlikely
/// once and essentially impossible eight times running. Reaching this bound
/// means something other than collision is going on, and failing beats looping.
const MAX_KEY_ATTEMPTS: usize = 8;

/// Server-side TCP keepalives, `SET` on the beacon's own session at
/// establishment (the GUCs are `USERSET`).
///
/// ≈11s worst-case detection, typically 5–7s, for one packet every 5s on one
/// connection per instance. Comfortably shorter than any plausible lock TTL,
/// which is the property that matters.
///
/// **Best-effort, never fatal** — see [`apply_keepalives`]. Fixed constants
/// rather than config knobs: getting these wrong costs recovery promptness and
/// never correctness, and promptness is *already* under caller control through
/// the lock TTL, which is a per-acquisition parameter on the trait. An operator
/// wanting faster reclamation should shorten the TTL rather than tune socket
/// timers they have no better basis to reason about.
///
/// `tcp_keepalives_*` rather than the tighter `tcp_user_timeout`: the latter has
/// narrower platform support, and setting a non-zero value where it is
/// unsupported is an *error* rather than a no-op — a poor trade for a plugin
/// expected to run anywhere.
const TCP_KEEPALIVE_SETTINGS: [&str; 3] = [
    "SET tcp_keepalives_idle = 5",
    "SET tcp_keepalives_interval = 2",
    "SET tcp_keepalives_count = 3",
];

/// The packed value meaning "this instance currently has no beacon".
///
/// Both halves of a real key are non-negative `i32`, so the largest packed value
/// any live beacon can take is `0x7FFF_FFFF_7FFF_FFFF` — this sentinel is
/// unreachable by construction rather than by convention.
const NO_BEACON: u64 = u64::MAX;

/// The two non-negative `i32` halves of this incarnation's advisory key, as
/// `pg_locks` exposes them (`classid`, `objid`) and as `cluster_lock` records
/// them (`holder_beacon_hi`, `holder_beacon_lo`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) struct BeaconKey {
    pub hi: i32,
    pub lo: i32,
}

impl BeaconKey {
    /// Draws a fresh per-incarnation key.
    ///
    /// From `Uuid::new_v4()`'s bits rather than a separate RNG dependency, with
    /// the sign bit masked off each half — 62 bits of key space, against which
    /// a collision across any realistic fleet is negligible (and detected at
    /// establishment anyway, since the `pg_try_advisory_lock` returns `false`).
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_possible_wrap,
        reason = "deliberate: two 31-bit slices of a random 128-bit value, masked \
                  non-negative so the pg_locks comparison needs no sign reinterpretation"
    )]
    fn random() -> Self {
        const NON_NEGATIVE: u32 = 0x7FFF_FFFF;
        let bits = Uuid::new_v4().as_u128();
        Self {
            hi: ((bits >> 64) as u32 & NON_NEGATIVE) as i32,
            lo: (bits as u32 & NON_NEGATIVE) as i32,
        }
    }

    fn pack(self) -> u64 {
        u64::from(self.hi.cast_unsigned()) << 32 | u64::from(self.lo.cast_unsigned())
    }

    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_possible_wrap,
        reason = "exact inverse of `pack`, which only ever stores two non-negative i32s"
    )]
    fn unpack(packed: u64) -> Self {
        Self {
            hi: ((packed >> 32) as u32) as i32,
            lo: (packed as u32) as i32,
        }
    }
}

/// Handle onto the live beacon: readable without a round-trip and replaced
/// atomically when the connection is re-established. Cheap to clone (three
/// `Arc`s).
///
/// Cloning does **not** extend the beacon's life — [`BeaconHandle`] owns that,
/// and its `shutdown` is what ends the task and closes the connection.
#[derive(Clone)]
pub(super) struct Beacon {
    /// The live key, packed, or [`NO_BEACON`]. Structurally the same shape the
    /// lock session's `generation` counter had, and for a related reason; the
    /// difference is that this value is also written into every row, so
    /// **Postgres** can check it rather than only the process that owns it.
    key: Arc<AtomicU64>,
    /// The beacon connection's backend pid, for the test seam that terminates
    /// exactly this connection, and as a log field.
    backend_pid: Arc<AtomicI32>,
    /// How many beacons this process has established. Incremented on every
    /// successful establishment, so a test can assert a loss was actually
    /// *observed* rather than inferring it from a downstream error, and so logs
    /// can name the incarnation.
    epoch: Arc<AtomicU64>,
}

impl Beacon {
    /// This incarnation's key, or `None` while there is no live beacon.
    ///
    /// A plain atomic load: acquire binds this on every attempt, so it must cost
    /// no round-trip.
    pub(super) fn key(&self) -> Option<BeaconKey> {
        match self.key.load(Ordering::Acquire) {
            NO_BEACON => None,
            packed => Some(BeaconKey::unpack(packed)),
        }
    }

    /// The beacon connection's backend pid (`0` before the first
    /// establishment).
    #[cfg(feature = "integration")]
    pub(super) fn backend_pid(&self) -> i32 {
        self.backend_pid.load(Ordering::Acquire)
    }

    /// How many beacons this process has established so far.
    #[cfg(feature = "integration")]
    pub(super) fn epoch(&self) -> u64 {
        self.epoch.load(Ordering::Acquire)
    }
}

/// The error every acquisition gets while this instance has no live beacon.
///
/// `ConnectionLost` (not `Other`): it is exactly what it says, and it is
/// retryable — `lock()`'s retry loop classifies it as transient
/// (`is_transient_session_loss`), so a blip inside a caller's timeout still
/// resolves into a successful acquisition rather than a spurious failure.
pub(super) fn beacon_unavailable(detail: &str) -> ClusterError {
    ClusterError::Provider {
        kind: ProviderErrorKind::ConnectionLost,
        message: format!("postgres lock beacon unavailable: {detail}"),
    }
}

/// Owns the beacon task's lifetime.
///
/// Held by the plugin handle rather than by `PostgresLock`, so the beacon can
/// deliberately outlive the shared shutdown token: the shutdown drain deletes
/// this instance's rows *by beacon key*, so it needs the key still readable.
pub struct BeaconHandle {
    cancel: CancellationToken,
    /// `Option` so [`shutdown`](Self::shutdown) can hand the handle back to the
    /// [`Drop`] impl below when its own future is dropped mid-await, and take it
    /// away once the task has genuinely exited.
    task: Option<tokio::task::JoinHandle<()>>,
}

impl BeaconHandle {
    /// Ends the beacon task and closes its connection, releasing the beacon.
    ///
    /// Call **after** the shutdown drain: the drain's `DELETE ... WHERE
    /// holder_beacon_* = $key` is what hands this instance's names back to the
    /// fleet promptly, and it reads the key from the still-live handle.
    ///
    /// Awaits the join through `&mut` rather than moving it out, so a *dropped*
    /// `shutdown()` future still leaves the handle intact for [`Drop`] to finish
    /// the job.
    pub async fn shutdown(mut self) {
        self.cancel.cancel();
        if let Some(task) = self.task.as_mut() {
            let _exited = task.await;
        }
        // Exited cleanly, so `Drop` has nothing left to abort.
        self.task = None;
    }
}

impl Drop for BeaconHandle {
    /// The backstop for a [`shutdown`](Self::shutdown) that never completed —
    /// DESIGN.md §11 recommends wrapping shutdown as
    /// `tokio::time::timeout(D, handle.stop())`, and when that budget elapses
    /// the `stop()` future is dropped, potentially while the `task.await` above
    /// had not yet resolved.
    ///
    /// Cancel *and* abort, in that order and both unconditionally. The cancel is
    /// the graceful path (it lets the task close the connection with a
    /// `Terminate`) and is a no-op if `shutdown` already fired it; the abort is
    /// what bounds the wait for a task parked inside an unresponsive statement,
    /// since `Drop` cannot await. Either way Postgres releases the beacon when
    /// the connection goes away.
    fn drop(&mut self) {
        self.cancel.cancel();
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

/// Everything the beacon task needs.
pub(super) struct BeaconConfig {
    /// The DSN the beacon (re)connects with — the same one the pool uses.
    pub connection_string: String,
    /// The write pool, for the post-reconnect cleanup below. The beacon's own
    /// connection never carries it: that connection writes nothing, ever.
    pub pool: PgPool,
    /// The schema-qualified `cluster_lock` table name.
    pub table: String,
    /// The registry purged the moment the beacon is lost (see
    /// [`BeaconTask::lose`]).
    pub local_holders: Arc<DashMap<String, Uuid>>,
    /// ADR-004 sink for `cluster_provider_errors_total` on beacon loss.
    pub metrics: Arc<dyn ClusterMetrics>,
    /// The bounded `provider` label.
    pub provider: &'static str,
}

/// Establishes this instance's beacon and spawns the task that keeps it.
///
/// The first connection is established **eagerly**, before returning, so a bad
/// DSN or an unreachable database fails `build_and_start` rather than surfacing
/// later as a lock backend that can acquire nothing — the same no-readiness-gate
/// guarantee DESIGN.md §3.2 makes for the LISTEN connections. Every *subsequent*
/// loss is handled by the task's own reconnect loop.
///
/// # Errors
/// Propagates a failed initial connect, an initial connect that outran
/// [`CONNECT_TIMEOUT`], a failed `pg_try_advisory_lock`, or [`MAX_KEY_ATTEMPTS`]
/// consecutive key collisions.
pub(super) async fn spawn_beacon(
    config: BeaconConfig,
) -> Result<(Beacon, BeaconHandle), ClusterError> {
    let beacon = Beacon {
        key: Arc::new(AtomicU64::new(NO_BEACON)),
        backend_pid: Arc::new(AtomicI32::new(0)),
        epoch: Arc::new(AtomicU64::new(0)),
    };
    // Bounded exactly as the reconnect path is: `establish` is TCP + TLS +
    // startup/auth + the `SET`s + the lock, none of which `connect` bounds on its
    // own, so a blackholed peer would otherwise park `build_and_start` for the OS
    // connect timeout instead of failing at a predictable point.
    let conn = tokio::time::timeout(
        CONNECT_TIMEOUT,
        establish(&config.connection_string, &beacon),
    )
    .await
    .map_err(|_elapsed| beacon_unavailable("initial connect attempt timed out"))??;
    let cancel = CancellationToken::new();

    let task = tokio::spawn(
        BeaconTask {
            conn: Some(conn),
            beacon: beacon.clone(),
            config,
            reconnect_backoff: INITIAL_RECONNECT_BACKOFF,
            lost_key: None,
        }
        .run(cancel.clone()),
    );

    Ok((
        beacon,
        BeaconHandle {
            cancel,
            task: Some(task),
        },
    ))
}

/// Opens one connection and takes a fresh beacon on it, publishing the key, the
/// backend pid, and the new epoch into `beacon` before returning.
///
/// Four statements, once per incarnation and never again: the keepalive `SET`s,
/// the `pg_try_advisory_lock` (retried on a key collision), and
/// `pg_backend_pid()`. No `search_path` and no `synchronous_commit`: this
/// connection touches no table and writes nothing.
async fn establish(connection_string: &str, beacon: &Beacon) -> Result<PgConnection, ClusterError> {
    let mut conn = PgConnection::connect(connection_string)
        .await
        .map_err(map_sqlx_error)?;
    apply_keepalives(&mut conn).await;

    let mut attempts = 0;
    let key = loop {
        let key = BeaconKey::random();
        let taken: bool = sqlx::query_scalar("SELECT pg_try_advisory_lock($1, $2)")
            .bind(key.hi)
            .bind(key.lo)
            .fetch_one(&mut conn)
            .await
            .map_err(map_sqlx_error)?;
        if taken {
            break key;
        }
        attempts += 1;
        // Nobody else holds a freshly random key, so `false` is a collision with
        // another live instance: draw another.
        if attempts >= MAX_KEY_ATTEMPTS {
            return Err(beacon_unavailable(&format!(
                "{MAX_KEY_ATTEMPTS} consecutive beacon keys were already held; \
                 pg_try_advisory_lock is refusing keys it should not be"
            )));
        }
    };

    let backend_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut conn)
        .await
        .map_err(map_sqlx_error)?;

    beacon.backend_pid.store(backend_pid, Ordering::Release);
    let epoch = beacon.epoch.fetch_add(1, Ordering::AcqRel) + 1;
    // Published last: a reader that sees a key must see the pid and epoch that
    // belong to it, and acquire binds this key into rows the fleet then judges.
    beacon.key.store(key.pack(), Ordering::Release);

    info!(
        beacon_hi = key.hi,
        beacon_lo = key.lo,
        backend_pid,
        epoch,
        "cluster.lock.beacon_established: this instance's liveness beacon is live; \
         cluster_lock rows carrying this key belong to this incarnation"
    );
    Ok(conn)
}

/// Applies [`TCP_KEEPALIVE_SETTINGS`], logging at DEBUG and carrying on if the
/// platform does not support them.
///
/// Best-effort by design, and the opposite of how `synchronous_commit` is
/// treated (DESIGN.md §3.4) — deliberately so. These GUCs are documented as
/// unsupported on some platforms, where the value must remain 0, and what a
/// failure costs is slower crash detection with the TTL still bounding it. That
/// is a promptness setting, not a durability invariant.
async fn apply_keepalives(conn: &mut PgConnection) {
    for statement in TCP_KEEPALIVE_SETTINGS {
        if let Err(err) = sqlx::query(statement).execute(&mut *conn).await {
            debug!(
                statement,
                error = %err,
                "cluster.lock.beacon_keepalive_unsupported: could not tighten the server-side TCP \
                 keepalives on the beacon connection; crash detection falls back to the platform \
                 default, still bounded by the lock TTL"
            );
        }
    }
}

/// The task owning the beacon connection.
struct BeaconTask {
    /// `None` while disconnected — the state in which acquisition fails fast
    /// rather than writing a row stamped with a key no longer in `pg_locks`.
    conn: Option<PgConnection>,
    beacon: Beacon,
    config: BeaconConfig,
    reconnect_backoff: Duration,
    /// The key of the incarnation that just died, until its rows have been
    /// handed over (see [`hand_over`](BeaconTask::hand_over)).
    lost_key: Option<BeaconKey>,
}

impl BeaconTask {
    /// Ping, notice loss, reconnect. That is the entire loop: there is no inbox,
    /// because nothing ever asks the beacon for anything.
    ///
    /// Every await is raced against `cancel`, including the connect attempt — a
    /// `select!` branch body cannot be preempted once it has begun, so an
    /// awaited connect against an unreachable peer would otherwise hold `stop()`
    /// for up to [`CONNECT_TIMEOUT`].
    async fn run(mut self, cancel: CancellationToken) {
        let mut ping =
            tokio::time::interval_at(tokio::time::Instant::now() + PING_INTERVAL, PING_INTERVAL);
        ping.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

        loop {
            if self.conn.is_none() {
                if self.reconnect(&cancel).await.is_break() {
                    break;
                }
                continue;
            }
            tokio::select! {
                () = cancel.cancelled() => break,
                _ = ping.tick() => self.ping_tick().await,
            }
        }

        // Graceful `Terminate` rather than a dropped socket: either way Postgres
        // releases the beacon, but closing means the backend is gone by the time
        // `stop()` returns instead of whenever the server notices the EOF.
        // Bounded, because an unresponsive peer would otherwise leave `stop()`
        // blocked here having survived everything above.
        if let Some(conn) = self.conn.take() {
            let _closed = tokio::time::timeout(CLOSE_TIMEOUT, conn.close()).await;
        }
    }

    /// One liveness ping. A failed one *is* a lost beacon — Postgres has already
    /// released it — so this invalidates rather than retrying on the same
    /// connection.
    async fn ping_tick(&mut self) {
        let Some(conn) = self.conn.as_mut() else {
            return;
        };
        let alive = matches!(
            tokio::time::timeout(STATEMENT_TIMEOUT, conn.ping()).await,
            Ok(Ok(()))
        );
        if !alive {
            self.lose("the beacon connection stopped answering");
        }
    }

    /// Waits out the backoff and makes one cancellable connect attempt.
    /// `Break` means shutdown won the race.
    async fn reconnect(&mut self, cancel: &CancellationToken) -> std::ops::ControlFlow<()> {
        tokio::select! {
            () = cancel.cancelled() => return std::ops::ControlFlow::Break(()),
            () = tokio::time::sleep(self.reconnect_backoff) => {}
        }
        let attempt = tokio::select! {
            () = cancel.cancelled() => return std::ops::ControlFlow::Break(()),
            outcome = tokio::time::timeout(
                CONNECT_TIMEOUT,
                establish(&self.config.connection_string, &self.beacon),
            ) => outcome,
        };
        match attempt {
            Ok(Ok(conn)) => {
                self.conn = Some(conn);
                self.reconnect_backoff = INITIAL_RECONNECT_BACKOFF;
                if let Some(lost) = self.lost_key.take() {
                    // Raced, like every other await here: `hand_over` runs a
                    // pooled `DELETE` and a `NOTIFY`, neither bounded, and
                    // `stop()` awaits `beacon.shutdown()` *before* it closes the
                    // pool — so an unresponsive backend here would hold `stop()`
                    // until the statement resolved. Losing the race costs
                    // promptness only: `hand_over` is best-effort by its own
                    // contract, and the lost incarnation's beacon is already gone,
                    // so its rows are unvouched and stealable on sight rather
                    // than waiting on this announcement.
                    tokio::select! {
                        () = cancel.cancelled() => {}
                        () = self.hand_over(lost) => {}
                    }
                }
            }
            outcome => {
                let err = match outcome {
                    Ok(Err(err)) => err,
                    _elapsed => beacon_unavailable("connect attempt timed out"),
                };
                observability::emit_provider_error(
                    &*self.config.metrics,
                    self.config.provider,
                    "lock_beacon_reconnect",
                    ResourceId::Name(BEACON_RESOURCE),
                    &err,
                );
                self.reconnect_backoff = (self.reconnect_backoff * 2).min(MAX_RECONNECT_BACKOFF);
            }
        }
        std::ops::ControlFlow::Continue(())
    }

    /// Marks this incarnation dead: clears the key, drops the connection, and
    /// purges every lock this instance believed it held.
    ///
    /// **Beacon loss means losing every lock**, deliberately, so a connection
    /// blip has one predictable outcome rather than a per-lock recovery path to
    /// reason about. Other instances can already reclaim these rows — the beacon
    /// vouching for them is gone from `pg_locks` — so the purge is not what makes
    /// them lose the locks; it is what stops this instance from advertising
    /// guards it can no longer defend. Consumers learn at their next `renew`,
    /// which is the only channel the SDK's guard offers.
    ///
    /// Clearing the key first is what makes acquisition fail fast instead of
    /// stamping a row with a beacon the fleet can already see is gone. The key is
    /// remembered so [`hand_over`](Self::hand_over) can announce those rows once
    /// there is a connection to do it with; a successor beacon draws a different
    /// key, so nothing that key matches can ever be a live lock again.
    fn lose(&mut self, detail: &str) {
        let lost = self.beacon.key();
        self.beacon.key.store(NO_BEACON, Ordering::Release);
        self.conn = None;
        self.lost_key = lost;

        let purged: Vec<String> = self
            .config
            .local_holders
            .iter()
            .map(|entry| entry.key().clone())
            .collect();
        for name in &purged {
            self.config.local_holders.remove(name);
        }

        let err = beacon_unavailable(detail);
        warn!(
            beacon_hi = lost.map(|key| key.hi),
            beacon_lo = lost.map(|key| key.lo),
            epoch = self.beacon.epoch.load(Ordering::Acquire),
            lost_locks = purged.len(),
            "cluster.lock.beacon_lost: this instance can no longer prove it is alive, so every \
             lock it held is now stealable by the fleet; acquisition fails until the beacon is \
             re-established, and each affected consumer learns at its next renew"
        );
        observability::emit_provider_error(
            &*self.config.metrics,
            self.config.provider,
            "lock_beacon_lost",
            ResourceId::Name(BEACON_RESOURCE),
            &err,
        );
    }
}

impl BeaconTask {
    /// Hands the previous incarnation's rows back to the fleet: one beacon-scoped
    /// `DELETE`, then a batched `cluster_lock_released` NOTIFY of the names.
    ///
    /// The same statement the shutdown drain runs, with a different parameter,
    /// and safely fenced for free — the dead key only ever matched rows *we*
    /// wrote, and any of them already stolen now carry the successor's beacon and
    /// are skipped.
    ///
    /// A courtesy rather than a requirement: those rows are unvouched and
    /// stealable on demand either way. What it buys is handing the names over on
    /// a NOTIFY instead of making waiters discover them by retry. Best-effort
    /// accordingly — a failure costs promptness, never correctness.
    async fn hand_over(&self, lost: BeaconKey) {
        let handed_over: Result<Vec<String>, ClusterError> =
            sqlx::query_scalar(AssertSqlSafe(format!(
                "DELETE FROM {table} \
             WHERE holder_beacon_hi = $1 AND holder_beacon_lo = $2 \
             RETURNING name",
                table = self.config.table,
            )))
            .bind(lost.hi)
            .bind(lost.lo)
            .fetch_all(&self.config.pool)
            .await
            .map_err(map_sqlx_error);

        let names = match handed_over {
            Ok(names) => names,
            Err(err) => {
                observability::emit_provider_error(
                    &*self.config.metrics,
                    self.config.provider,
                    "lock_beacon_handover",
                    ResourceId::Name(BEACON_RESOURCE),
                    &err,
                );
                return;
            }
        };
        if names.is_empty() {
            return;
        }
        info!(
            beacon_hi = lost.hi,
            beacon_lo = lost.lo,
            handed_over = names.len(),
            "cluster.lock.beacon_rows_handed_over: deleted and announced the rows written by this \
             instance's previous incarnation, so waiters take those names now rather than on their \
             own retry"
        );
        if let Err(err) = crate::lock::notify::notify_released_many(&self.config.pool, &names).await
        {
            observability::emit_provider_error(
                &*self.config.metrics,
                self.config.provider,
                "lock_beacon_handover_notify",
                ResourceId::Name(BEACON_RESOURCE),
                &err,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The packed representation must round-trip every key the generator can
    /// produce, including both extremes of the non-negative `i32` range — a
    /// packing bug would silently write one key into rows and compare another.
    #[test]
    fn beacon_key_packs_and_unpacks_losslessly() {
        for key in [
            BeaconKey { hi: 0, lo: 0 },
            BeaconKey { hi: 1, lo: 2 },
            BeaconKey {
                hi: i32::MAX,
                lo: i32::MAX,
            },
            BeaconKey {
                hi: 0,
                lo: i32::MAX,
            },
            BeaconKey {
                hi: i32::MAX,
                lo: 0,
            },
        ] {
            assert_eq!(BeaconKey::unpack(key.pack()), key);
        }
    }

    /// No key a generator can draw may pack to [`NO_BEACON`], or a live beacon
    /// would read as absent and every acquisition would fail.
    #[test]
    fn no_live_key_can_collide_with_the_absent_sentinel() {
        let widest = BeaconKey {
            hi: i32::MAX,
            lo: i32::MAX,
        };
        assert!(widest.pack() < NO_BEACON);
        for _ in 0..1_000 {
            assert_ne!(BeaconKey::random().pack(), NO_BEACON);
        }
    }

    /// Both halves must stay non-negative: the acquire predicate compares them
    /// against `pg_locks`'s unsigned `oid` columns without reinterpretation, and
    /// `cluster_lock_beacon_nonneg_check` rejects the row outright otherwise.
    #[test]
    fn generated_keys_are_always_non_negative() {
        for _ in 0..1_000 {
            let key = BeaconKey::random();
            assert!(key.hi >= 0 && key.lo >= 0, "negative half in {key:?}");
        }
    }

    /// Two draws must differ: the key's whole job is to identify one
    /// *incarnation*, so a generator that repeated itself would let a
    /// reconnected instance vouch for its own pre-disconnect rows.
    #[test]
    fn generated_keys_differ_across_draws() {
        let first = BeaconKey::random();
        assert!(
            (0..16).any(|_| BeaconKey::random() != first),
            "16 consecutive draws produced the same key"
        );
    }
}
