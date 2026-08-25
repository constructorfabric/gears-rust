//! `PostgresLock` — the native `DistributedLockBackend` implementation over the
//! `cluster_lock` lease row (DESIGN.md §5), plus the standalone
//! `PostgresLockPlugin` builder/handle (DESIGN.md §3.5) that lets an operator
//! route `lock` to Postgres independently of `cache`.
//!
//! ## The row is the arbiter
//!
//! A lock is held **iff** a `cluster_lock` row exists whose `expires_at` is in
//! the future *and* whose recorded beacon is still granted in `pg_locks`. Every
//! acquire, renew, and release is a single statement against the write pool,
//! with no session affinity and **no in-process state that is load-bearing for
//! exclusion**.
//!
//! Three mechanisms cooperate inside the acquire statement, and the primary key
//! does the least work of the three:
//!
//! 1. **`PRIMARY KEY (name)` detects the conflict**, giving `ON CONFLICT`
//!    something to fire on. It decides nothing.
//! 2. **The row lock serializes.** On conflict Postgres takes an exclusive lock
//!    on the conflicting tuple, so a competing transaction holding it makes us
//!    *block* until it commits or aborts.
//! 3. **The `WHERE` decides**, evaluated against the **latest committed**
//!    version of the row — not the snapshot the statement started with.
//!
//! Step 3 is what makes it correct, and it is `READ COMMITTED` behaviour
//! (asserted at startup by `pg_setup::assert_read_committed`): two acquirers
//! cannot both observe the lock as free, because the loser re-evaluates against
//! the winner's already-committed state. `RETURNING` is the answer — a row means
//! acquired, whether by insert or by steal; zero rows means contended. There is
//! no third case, and two tasks in *this* process race exactly as two instances
//! do, which is why no in-process claim registry is needed to arbitrate them.
//!
//! ## `pg_advisory_unlock` is never called — but rows are deleted constantly
//!
//! Two different things are called "lock" around here, so this is worth being
//! exact about. Releasing a lock still means **deleting its row**, and five
//! paths do exactly that: `release`, the TTL sweep, the orphan sweep, the
//! shutdown drain, and the beacon's post-reconnect hand-over.
//!
//! What no longer happens is the *advisory-lock* release: nothing per-lock is
//! ever `pg_advisory_lock`ed, so there is no session-scoped `pg_advisory_unlock`
//! to pair with it. (The one advisory lock this plugin takes is the per-instance
//! beacon, and that one is released only by its connection closing —
//! `lock::beacon`.)
//!
//! The consequence is what matters: deleting a row is something **any** instance
//! can do, whereas an advisory unlock could only ever be issued by the session
//! that took it. An expired or unvouched row is therefore stealable by the
//! acquire predicate itself, evaluated by whoever asks — no reclaim step, and no
//! reason reclamation has to route back to the instance that held the lock. That
//! is what lets a crashed *or merely wedged* holder's lock be taken by anyone,
//! rather than only by a healthy reaper on the owning instance.
//!
//! The one surviving in-process registry, [`PostgresLock::local_holders`], is an
//! input to garbage collection and **not** a participant in mutual exclusion —
//! see its doc.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use cluster_sdk::observability::{self, result, spans};
use cluster_sdk::{
    ClusterError, ClusterMetrics, DistributedLockBackend, LockFeatures, LockGuard,
    ProviderErrorKind,
};
use dashmap::DashMap;
use sqlx::AssertSqlSafe;
use sqlx::PgPool;
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;
use tracing::{Instrument, warn};
use uuid::Uuid;

pub mod beacon;
pub mod notify;
pub mod reaper;

use crate::config::PostgresLockConfig;
use crate::limits::MAX_INDEXED_KEY_BYTES;
use crate::pg_error::map_sqlx_error;
use crate::pg_setup::{
    assert_read_committed, base_pool_options, ensure_schema, lock_migrator,
    reject_pgbouncer_transaction_mode, run_migrator, warn_if_async_replication,
};
use crate::shutdown::{DropDiagnosis, cancel_and_diagnose_drop, close_pool};
use beacon::{Beacon, BeaconConfig, BeaconHandle, beacon_unavailable};
use notify::ReleaseWaiters;

/// Maximum lock-name length, in UTF-8 bytes, this backend accepts.
///
/// Two `PostgreSQL` limits bear on a lock name and this is the tighter:
/// `cluster_lock.name` is a `PRIMARY KEY`, so every name lands in a btree bound
/// by the index-tuple ceiling (see `limits.rs`). The looser one is the release
/// path — `NOTIFY cluster_lock_released, '<name>'` carries the bare name as its
/// payload, and `PostgreSQL` rejects payloads of 8000 bytes or more, so a name
/// that cannot fit could never be cleanly released: `release`'s single statement
/// would delete the row and fail on the `pg_notify` in the same breath,
/// returning an error for a lock that is already gone.
///
/// Bounding by the btree limit satisfies both. Names are rejected at
/// acquisition, before any lock state is mutated, so `release` never sees an
/// un-notifiable name and the metadata INSERT never trips its own CHECK.
const MAX_LOCK_NAME_BYTES: usize = MAX_INDEXED_KEY_BYTES;

/// Rejects a lock `name` too long to index or to notify on, so the acquisition
/// never enters a state its release cannot cleanly signal (see
/// [`MAX_LOCK_NAME_BYTES`]). Returns [`ClusterError::InvalidName`] without
/// touching any lock state.
fn validate_lock_name(name: &str) -> Result<(), ClusterError> {
    // `reason` is a `&'static str`, so the bound is a literal; keep it in sync
    // with the constant actually enforced.
    const _: () = assert!(MAX_LOCK_NAME_BYTES == 2048);
    if name.len() > MAX_LOCK_NAME_BYTES {
        return Err(ClusterError::InvalidName {
            name: name.to_owned(),
            reason: "lock name must be at most 2048 UTF-8 bytes (the btree index-tuple limit on \
                     the cluster_lock primary key)",
        });
    }
    Ok(())
}

/// Records the metric side of a finished lock op (duration + bounded-`result`
/// counter) and the shared provider-error signals, mirroring
/// `CasBasedDistributedLockBackend::record_lock` (`cluster/src/defaults/lock.rs`)
/// so the native Postgres lock emits the exact same ADR-004 signal set the
/// CAS-based default does (DESIGN.md §8). Used by both the backend
/// (`try_lock`/`lock`) and the per-guard task (`renew`/`release`).
fn record_lock<T>(
    metrics: &dyn ClusterMetrics,
    provider: &'static str,
    op: &'static str,
    lock: &str,
    started: std::time::Instant,
    outcome: &Result<T, ClusterError>,
) {
    metrics.lock_op_duration(op, started.elapsed().as_secs_f64());
    metrics.lock_op(op, result::label(outcome));
    if let Err(err) = outcome {
        observability::emit_provider_error(
            metrics,
            provider,
            op,
            observability::ResourceId::Lock(lock),
            err,
        );
    }
}

/// Converts a lock TTL to the millisecond lifetime bound to the write query
/// (`BIGINT`), which SQL adds to the database clock — see [`expires_at_sql`].
fn ttl_to_millis(ttl: Duration) -> Result<i64, ClusterError> {
    i64::try_from(ttl.as_millis()).map_err(|_| ClusterError::InvalidConfig {
        reason: format!("ttl {ttl:?} exceeds the storable millisecond range"),
    })
}

/// The SQL fragment computing `cluster_lock.expires_at` from a bound millisecond
/// lifetime (`$n`, a [`ttl_to_millis`] value). `n` is its 1-based bind position.
///
/// The deadline is computed **in SQL against the database clock**, never from
/// `chrono::Utc::now()` on the acquiring instance — exactly as
/// `cache::expires_at_sql` does for `cluster_cache` (PGR-C2). The reaper's sweep
/// compares this column to Postgres `now()`, so anchoring the write to a fleet
/// instance's own (possibly skewed) wall clock could reap a live lock early or
/// let a dead holder's lock linger. Unconditional, with no `NULL` branch: a lock
/// TTL is mandatory (`DistributedLockBackend` takes a `Duration`, not a `Ttl`),
/// so unlike the cache there is no indefinite case to encode.
fn expires_at_sql(n: usize) -> String {
    format!("now() + (${n}::bigint * interval '1 millisecond')")
}

/// The acquire statement's conflict predicate: may we take a row that already
/// exists?
///
/// Two ways to be free, and the order of the branches is load-bearing:
///
/// * the lease lapsed (`expires_at <= now()`), or
/// * the beacon vouching for it is no longer granted in `pg_locks`, which means
///   the holder's process is gone (`lock::beacon`) and its lock with it, without
///   waiting out the remaining TTL.
///
/// **`CASE`, not `OR`.** SQL does not guarantee left-to-right evaluation of `OR`
/// operands, and this needs the cheap indexed comparison to short-circuit the
/// `pg_locks` scan off the uncontended path — `pg_locks` is a function scan over
/// `pg_lock_status()` with no index, so paying it on every acquire would make
/// the common case `O(locks in the cluster)`. `CASE` does guarantee its branches
/// are evaluated only as needed; `PG-SPEC-012` holds it to that with
/// `EXPLAIN ANALYZE` rather than taking it on trust.
///
/// The subquery is correlated against a *single* row — the conflicting one,
/// located by primary key — so even when it does run, the cost is one function
/// scan per contended acquire rather than one per candidate row.
///
/// `objsubid = 2` selects the two-argument `pg_try_advisory_lock(int4, int4)`
/// form the beacon uses (`1` would be the single-`bigint` form), whose two keys
/// Postgres exposes as `classid` and `objid`. Both are `oid` (unsigned), which
/// is why `cluster_lock_beacon_nonneg_check` keeps both halves non-negative:
/// the comparison is then a plain cast with no sign reinterpretation.
fn stealable_predicate(table: &str) -> String {
    format!(
        "CASE WHEN {table}.expires_at <= now() THEN true \
              ELSE NOT EXISTS ( \
                     SELECT 1 FROM pg_locks \
                      WHERE locktype = 'advisory' AND objsubid = 2 AND granted \
                        AND classid = {table}.holder_beacon_hi::oid \
                        AND objid   = {table}.holder_beacon_lo::oid) \
         END"
    )
}

/// The native Postgres distributed-lock backend.
pub struct PostgresLock {
    pool: PgPool,
    /// The schema-qualified table name. See `PostgresCache::table`'s doc for
    /// the trust-boundary note — same reasoning applies here.
    table: String,
    /// This instance's liveness beacon (`beacon`'s module doc): the single
    /// advisory key whose presence in `pg_locks` is what tells the rest of the
    /// fleet this process is still alive. Every acquisition stamps the live key
    /// onto its `cluster_lock` row, so a row can be judged by anyone rather than
    /// only by the instance that wrote it.
    beacon: Beacon,
    /// The locks this process currently has a live guard for, name to
    /// `holder_id`.
    ///
    /// **Not authoritative, and deliberately named so no future reader mistakes
    /// it for it.** Exclusion is decided entirely by the `cluster_lock` row (the
    /// module doc): `renew` and `release` fence in SQL, the reaper does not need
    /// to know which locks are ours, and `cluster_postgres_lock_active_names` is
    /// table-derived rather than `len()` of this.
    ///
    /// It survives for exactly one consumer — the reaper's orphan sweep
    /// (`reaper::sweep_orphans`), which must distinguish a row with a live local
    /// guard from a row whose acquirer went away. That is the one question the
    /// database cannot answer about itself, and the only reason any local
    /// registry remains at all.
    local_holders: Arc<DashMap<String, Uuid>>,
    /// In-process wake-up registry for blocked `lock()` callers, fed by the
    /// `cluster_lock_released` LISTEN task (`notify::spawn_release_listen_task`,
    /// started by the plugin's `build_and_start` — DESIGN.md §5.3).
    release_waiters: Arc<ReleaseWaiters>,
    /// The ADR-004 metrics sink this lock emits `cluster_lock_ops_total` /
    /// `cluster_lock_op_duration_seconds` / `cluster_provider_errors_total`
    /// through (DESIGN.md §8). Native (not decorator-wrapped): `try_lock`/`lock`
    /// and the guard task's `renew`/`release` call [`record_lock`] directly.
    metrics: Arc<dyn ClusterMetrics>,
    /// The bounded `provider` label attached to every emitted signal.
    provider: &'static str,
    /// Cancelled on `stop()` (the same token the reapers/LISTEN tasks observe).
    /// Each guard task selects on it so, after shutdown, a guard whose consumer
    /// still holds its `LockGuard` exits promptly instead of parking on its
    /// `reassert_interval` timer until the guard drops (PGR-L2).
    guard_shutdown: CancellationToken,
    /// Signalled after every `try_acquire`/`renew` that writes a new
    /// `expires_at`, waking the TTL reaper so it re-reads the earliest deadline
    /// (`reaper`'s "Wake schedule").
    ///
    /// Without it the reaper only learns about deadlines that already existed at
    /// its last wake, so a lock whose whole lifetime fits inside one sleep
    /// (TTL ≲ `lock_reaper_interval_ms`) would not be reclaimed until that sleep
    /// ended — leaving its row in the table and its waiters unwoken until then.
    ///
    /// Purely local, and that is sufficient rather than partial: the sweep is
    /// promptness only. Any instance can now take an expired row on its own
    /// acquire (the module doc), so a hint nobody else hears costs at most a
    /// waiter's heartbeat, never a wedged name.
    ///
    /// Signalled *selectively*, not on every write — see [`should_hint`].
    deadline_hint: Arc<Notify>,
    /// The lock TTL reaper's interval — its metrics cadence and the cap on any one
    /// of its sleeps. Held here so [`spawn_reaper`](Self::spawn_reaper) and the
    /// [`should_hint`] gate in every [`GuardContext`] read the same value; two
    /// independently-passed copies could disagree, and the gate's correctness is a
    /// statement about the reaper's actual sleep bound.
    reaper_interval: Duration,
}

/// Everything [`PostgresLock::start`] needs. A struct rather than seven
/// positional parameters, and constructible from outside this module so
/// `plugin.rs`'s combined builder can call it too.
pub struct LockInit {
    /// The write pool, carrying every `cluster_lock` statement and `pg_notify`.
    pub pool: PgPool,
    /// The DSN the beacon connects with.
    pub connection_string: String,
    /// Schema holding `cluster_lock`.
    pub schema: String,
    /// The lock TTL reaper's interval — see [`PostgresLock::reaper_interval`].
    /// Passed once here rather than to `spawn_reaper`, so the reaper and the
    /// `deadline_hint` gate cannot be given different values.
    pub reaper_interval: Duration,
    /// The ADR-004 metrics sink (DESIGN.md §8).
    pub metrics: Arc<dyn ClusterMetrics>,
    /// The bounded `provider` label.
    pub provider: &'static str,
    /// The shared shutdown token guard tasks and reapers observe (PGR-L2). The
    /// *beacon* deliberately does not observe it — see
    /// [`BeaconHandle::shutdown`].
    pub guard_shutdown: CancellationToken,
}

/// What [`PostgresLock::start`] hands back: the backend plus the lifetime of the
/// one off-pool connection it owns.
///
/// The beacon handle is returned rather than stored on `PostgresLock`, because
/// it must be shut down *after* the drain (DESIGN.md §10) — which is the plugin
/// handle's business. Keeping it out of `PostgresLock` is what makes that
/// ordering unrepresentable to get wrong: nothing that only holds an
/// `Arc<PostgresLock>` can end the beacon, and ending it early would assert to
/// the whole fleet that this instance is dead.
pub struct LockRuntime {
    pub lock: Arc<PostgresLock>,
    pub beacon: BeaconHandle,
}

impl PostgresLock {
    /// Builds the lock backend and establishes this instance's liveness beacon.
    ///
    /// # Errors
    /// Propagates a failed beacon establishment — it connects eagerly, so a bad
    /// DSN fails `build_and_start` rather than the first `try_lock`.
    pub(crate) async fn start(init: LockInit) -> Result<LockRuntime, ClusterError> {
        let table = format!("{schema}.cluster_lock", schema = init.schema);
        let local_holders: Arc<DashMap<String, Uuid>> = Arc::new(DashMap::new());
        let (beacon, beacon_handle) = beacon::spawn_beacon(BeaconConfig {
            connection_string: init.connection_string,
            pool: init.pool.clone(),
            table: table.clone(),
            local_holders: Arc::clone(&local_holders),
            metrics: Arc::clone(&init.metrics),
            provider: init.provider,
        })
        .await?;

        let lock = Arc::new(Self {
            pool: init.pool,
            table,
            beacon,
            local_holders,
            release_waiters: ReleaseWaiters::new(),
            metrics: init.metrics,
            provider: init.provider,
            guard_shutdown: init.guard_shutdown,
            deadline_hint: Arc::new(Notify::new()),
            reaper_interval: init.reaper_interval,
        });
        Ok(LockRuntime {
            lock,
            beacon: beacon_handle,
        })
    }

    /// Bundles the per-guard fields into a [`GuardContext`] for `try_acquire` /
    /// `run_guard_task` (keeps their signatures within a sane arg count).
    fn guard_context(&self) -> GuardContext {
        GuardContext {
            pool: self.pool.clone(),
            beacon: self.beacon.clone(),
            metrics: Arc::clone(&self.metrics),
            provider: self.provider,
            shutdown: self.guard_shutdown.clone(),
            deadline_hint: Arc::clone(&self.deadline_hint),
            reaper_interval: self.reaper_interval,
        }
    }

    /// The underlying pool, for the shutdown path. `pub(crate)`: intra-crate
    /// wiring only (PGR-L3).
    #[must_use]
    pub(crate) fn pool(&self) -> PgPool {
        self.pool.clone()
    }

    /// The release-wake registry, for the LISTEN task
    /// (`notify::spawn_release_listen_task`). `pub(crate)`: intra-crate wiring
    /// only, and `ReleaseWaiters` is not a nameable public type (PGR-L3).
    #[must_use]
    pub(crate) fn release_waiters(&self) -> Arc<ReleaseWaiters> {
        Arc::clone(&self.release_waiters)
    }

    /// Hands every lock this instance holds back to the fleet, in one statement
    /// (DESIGN.md §10 step 4).
    ///
    /// Called by both `stop()` paths, *before* the beacon is shut down and the
    /// pool closed. Not load-bearing for `pool.close()` — a held lock pins no
    /// pool connection — but required for a *clean* handover: without it every
    /// name this instance holds stays taken until its beacon actually goes away
    /// and each waiter notices, rather than being announced on
    /// `cluster_lock_released` the moment we let go (`PG-LIFE-003`).
    ///
    /// **The beacon columns fence this for free**, which is what collapses the
    /// old multi-pass drain to a single `DELETE`. The key is per-incarnation, so
    /// this can only ever match rows *we* wrote; a row whose lock lapsed and was
    /// re-acquired elsewhere now carries the successor's beacon and is skipped —
    /// exactly what matching `holder_id` name-by-name had to do by hand.
    ///
    /// It is also strictly more correct than what it replaces, in two ways.
    ///
    /// * It reclaims **orphaned rows** — rows with no live local guard, left by
    ///   an acquisition cancelled after its INSERT committed — which a drain that
    ///   iterates a local map cannot see at all.
    /// * It needs no fixpoint loop. The old drain looped until the local map was
    ///   empty *and* no acquisition was mid-flight, because a straggler could
    ///   register after a pass had already collected its names. Here a straggler
    ///   that commits its row after this `DELETE` costs nothing: the beacon closes
    ///   moments later, leaving that row unvouched, so the next acquirer takes the
    ///   name on its own heartbeat retry rather than waiting out the TTL.
    pub async fn drain_beacon_rows(&self) {
        // No beacon means nothing vouches for this instance's rows already: they
        // are stealable on sight, so there is nothing to hand over and no key to
        // match on.
        let Some(key) = self.beacon.key() else {
            return;
        };

        // A connection this call **owns and closes explicitly**, because
        // `pool.close()` runs immediately afterwards: a `PoolConnection` returns
        // to the pool asynchronously, and one released microseconds before
        // `close()` can still be in flight when it returns — leaving a live
        // backend behind that nothing subsequently closes (`PG-LIFE-003` catches
        // exactly this). `detach` removes the race rather than narrowing it.
        let conn = match self.pool.acquire().await {
            Ok(conn) => conn,
            Err(err) => {
                self.report_drain_failure("drain_rows", &map_sqlx_error(err));
                return;
            }
        };
        let mut conn = conn.detach();

        let drained: Result<Vec<String>, ClusterError> =
            sqlx::query_scalar(AssertSqlSafe(format!(
                "DELETE FROM {table} \
             WHERE holder_beacon_hi = $1 AND holder_beacon_lo = $2 \
             RETURNING name",
                table = self.table,
            )))
            .bind(key.hi)
            .bind(key.lo)
            .fetch_all(&mut conn)
            .await
            .map_err(map_sqlx_error);

        match drained {
            Ok(names) => {
                // One batched NOTIFY for the whole drain rather than one per lock,
                // on the connection already in hand.
                if !names.is_empty()
                    && let Err(err) = notify::notify_released_many(&mut conn, &names).await
                {
                    self.report_drain_failure("drain_notify", &err);
                }
            }
            Err(err) => self.report_drain_failure("drain_rows", &err),
        }

        // Cleared after the delete, not before: nothing here is load-bearing for
        // exclusion (see `local_holders`), and an entry left behind would only
        // mislead the orphan sweep — which is no longer running by now anyway.
        self.local_holders.clear();

        // Graceful `Terminate` rather than a dropped socket, so the server-side
        // backend is gone by the time `stop()` returns.
        let _closed = sqlx::Connection::close(conn).await;
    }

    /// One ADR-004 failure signal for a drain step. Best-effort throughout: what
    /// a failed drain costs is a slower handover — the rows are unvouched the
    /// moment the beacon closes a step later, so the fleet reacquires those names
    /// on its own retry instead of on our NOTIFY.
    fn report_drain_failure(&self, op: &'static str, err: &ClusterError) {
        observability::emit_provider_error(
            &*self.metrics,
            self.provider,
            op,
            observability::ResourceId::Name(&self.table),
            err,
        );
        warn!(
            op,
            "cluster.lock.drain_incomplete: could not hand this instance's locks back on the way \
             out; they stop being vouched for when the beacon closes a moment later, so the fleet \
             reacquires those names by retry rather than by the release NOTIFY"
        );
    }

    /// Spawns the TTL reaper.
    ///
    /// A method on `PostgresLock` itself (delegating to
    /// `reaper::spawn_lock_reaper`) rather than a free function callers invoke
    /// with the pieces of a `PostgresLock` spread out — `reaper::ReaperContext`
    /// names types private to this module, so nothing outside `lock/` could even
    /// spell the signature.
    /// Takes no `interval`: it reads [`reaper_interval`](Self::reaper_interval),
    /// the same value the `deadline_hint` gate is derived from (see
    /// [`should_hint`]).
    #[must_use]
    pub(crate) fn spawn_reaper(
        self: &Arc<Self>,
        metrics: reaper::LockReaperMetrics,
        warn_threshold: i64,
        cancel: CancellationToken,
    ) -> tokio::task::JoinHandle<()> {
        reaper::spawn_lock_reaper(
            self.reaper_context(),
            self.reaper_interval,
            metrics,
            warn_threshold,
            reaper::ReaperWakeup {
                deadline_hint: Arc::clone(&self.deadline_hint),
                cancel,
            },
        )
    }

    /// Spawns the dedicated `cluster_lock_released` LISTEN task
    /// (`notify::spawn_release_listen_task`), which wakes blocked `lock()`
    /// callers on this instance.
    ///
    /// One job now, not two. It used to also nudge the reaper to reconcile locks
    /// whose rows another instance's sweep had deleted — a reconciliation that
    /// existed because only the owning instance could release its own advisory
    /// locks. With the row as the arbiter there is nothing to reconcile: whoever
    /// deletes the row frees the name, for everyone.
    #[must_use]
    pub(crate) async fn spawn_release_listener(
        &self,
        connection_string: String,
        cancel: CancellationToken,
    ) -> tokio::task::JoinHandle<()> {
        notify::spawn_release_listen_task(connection_string, self.release_waiters(), cancel).await
    }

    /// The handles the TTL reaper needs to sweep the table and to tell its own
    /// orphaned rows from live ones.
    fn reaper_context(&self) -> reaper::ReaperContext {
        reaper::ReaperContext {
            pool: self.pool.clone(),
            table: self.table.clone(),
            beacon: self.beacon.clone(),
            local_holders: Arc::clone(&self.local_holders),
            metrics: Arc::clone(&self.metrics),
            provider: self.provider,
        }
    }

    /// Test-only: this instance's live beacon key, or `None` while it has no
    /// beacon. Lets a test correlate a `cluster_lock` row's `holder_beacon_*`
    /// with the instance that wrote it, and assert that a reconnect draws a
    /// *different* key. Gated behind `--features integration` (PGR-M8).
    #[cfg(feature = "integration")]
    #[doc(hidden)]
    #[must_use]
    pub fn __test_beacon_key(&self) -> Option<(i32, i32)> {
        self.beacon.key().map(|key| (key.hi, key.lo))
    }

    /// Test-only: the beacon connection's backend pid, so a fault-injection test
    /// can `pg_terminate_backend` exactly this connection. Gated behind
    /// `--features integration` (PGR-M8).
    #[cfg(feature = "integration")]
    #[doc(hidden)]
    #[must_use]
    pub fn __test_beacon_backend_pid(&self) -> i32 {
        self.beacon.backend_pid()
    }

    /// Test-only: how many beacons this process has established. Lets a test
    /// assert a loss was actually *observed* (and a fresh beacon taken) rather
    /// than inferring it from a downstream error. Gated behind
    /// `--features integration` (PGR-M8).
    #[cfg(feature = "integration")]
    #[doc(hidden)]
    #[must_use]
    pub fn __test_beacon_epoch(&self) -> u64 {
        self.beacon.epoch()
    }

    /// Test-only: how many locks this instance currently has a live local guard
    /// for ([`local_holders`](Self::local_holders)). Not an exclusion fact — it
    /// is what the orphan sweep exempts — but it is the observable for a beacon
    /// loss having purged this instance's registry. Gated behind
    /// `--features integration` (PGR-M8).
    #[cfg(feature = "integration")]
    #[doc(hidden)]
    #[must_use]
    pub fn __test_local_holder_count(&self) -> usize {
        self.local_holders.len()
    }

    /// Test-only: writes a `cluster_lock` row under this instance's live beacon
    /// **without** registering a local holder — exactly the state a `try_acquire`
    /// cancelled between its committed INSERT and its registration leaves behind.
    /// Lets a test drive the orphan sweep (`reaper::sweep_orphans`) against a real
    /// orphan instead of trying to hit a microsecond-wide cancellation window.
    ///
    /// `acquired_at` is backdated by `age`, since the sweep will only reclaim a
    /// row that was already unregistered at its previous wake.
    ///
    /// Gated behind `--features integration` (PGR-M8).
    #[cfg(feature = "integration")]
    #[doc(hidden)]
    pub async fn __test_orphan_row(&self, name: &str, age: Duration) -> Result<(), ClusterError> {
        let key = self
            .beacon
            .key()
            .ok_or_else(|| beacon_unavailable("no live beacon to orphan a row under"))?;
        let age_ms = ttl_to_millis(age)?;
        sqlx::query(AssertSqlSafe(format!(
            "INSERT INTO {table} \
             (name, holder_id, acquired_at, expires_at, holder_beacon_hi, holder_beacon_lo) \
             VALUES ($1, $2, now() - ($3::bigint * interval '1 millisecond'), \
                     now() + interval '10 minutes', $4, $5)",
            table = self.table,
        )))
        .bind(name)
        .bind(Uuid::new_v4())
        .bind(age_ms)
        .bind(key.hi)
        .bind(key.lo)
        .execute(&self.pool)
        .await
        .map_err(map_sqlx_error)?;
        Ok(())
    }

    /// Test-only: `EXPLAIN (ANALYZE, VERBOSE)` of the **real** acquire statement,
    /// returned as plan text. Backs `PG-SPEC-012`, which holds the `CASE`
    /// predicate to actually short-circuiting the `pg_locks` subplan rather than
    /// merely being written to.
    ///
    /// `EXPLAIN ANALYZE` executes the statement, so this really does acquire (or
    /// contend for) `name` — which is what makes the plan honest.
    ///
    /// Gated behind `--features integration` (PGR-M8).
    #[cfg(feature = "integration")]
    #[doc(hidden)]
    pub async fn __test_explain_acquire(
        &self,
        name: &str,
        ttl: Duration,
    ) -> Result<String, ClusterError> {
        let key = self
            .beacon
            .key()
            .ok_or_else(|| beacon_unavailable("no live beacon to plan an acquire with"))?;
        let plan: Vec<String> = sqlx::query_scalar(AssertSqlSafe(format!(
            "EXPLAIN (ANALYZE, VERBOSE) {acquire}",
            acquire = acquire_sql(&self.table)
        )))
        .bind(name)
        .bind(Uuid::new_v4())
        .bind(ttl_to_millis(ttl)?)
        .bind(key.hi)
        .bind(key.lo)
        .fetch_all(&self.pool)
        .await
        .map_err(map_sqlx_error)?;
        Ok(plan.join("\n"))
    }

    /// Test-only: runs one orphan sweep (`reaper::sweep_orphans`) with a fence of
    /// `now() - fence_age`, returning how many rows it reclaimed. Driven directly
    /// so the assertion does not depend on how many reaper intervals elapsed.
    ///
    /// Gated behind `--features integration` (PGR-M8).
    #[cfg(feature = "integration")]
    #[doc(hidden)]
    pub async fn __test_sweep_orphans_once(
        &self,
        fence_age: Duration,
    ) -> Result<usize, ClusterError> {
        let ctx = self.reaper_context();
        let fence = reaper::__test_fence_before(&self.pool, fence_age).await?;
        reaper::sweep_orphans(&ctx, fence).await
    }

    /// Test-only: runs one complete TTL sweep — every batch, exactly as the
    /// reaper's own wake does — and returns how many expired rows it reclaimed.
    /// Lets `PG-SPEC-009` assert that a backlog larger than one
    /// `reaper::SWEEP_BATCH` is cleared by a *single* sweep's batch loop, rather
    /// than inferring it from how many reaper intervals happened to elapse.
    ///
    /// Gated behind `--features integration` (PGR-M8).
    #[cfg(feature = "integration")]
    #[doc(hidden)]
    pub async fn __test_sweep_once(&self) -> Result<usize, ClusterError> {
        reaper::sweep(&self.reaper_context(), &self.guard_shutdown).await
    }

    /// Test-only: the reaper's own "how long until the next lock is due" probe
    /// (`None` when no locks exist), which its wake schedule shortens sleeps
    /// with. Exercised directly by `PG-SPEC-009` because a regression in that
    /// query would otherwise be invisible — the reaper falls back to the plain
    /// interval on error, so nothing else would fail.
    ///
    /// Gated behind `--features integration` (PGR-M8).
    #[cfg(feature = "integration")]
    #[doc(hidden)]
    pub async fn __test_seconds_until_next_expiry(&self) -> Result<Option<f64>, ClusterError> {
        Ok(reaper::probe_schedule(&self.pool, &self.table)
            .await?
            .seconds_until_expiry)
    }

    /// Test-only: the number of blocked `lock()` callers currently registered as
    /// release-NOTIFY waiters for `name`. Lets `pg_lock_003` synchronize on the
    /// waiter having reached its registration before the holder releases (PGR-E3).
    /// Gated behind `--features integration` (PGR-M8).
    #[cfg(feature = "integration")]
    #[doc(hidden)]
    #[must_use]
    pub fn __test_release_waiter_count(&self, name: &str) -> usize {
        self.release_waiters.__test_registered_count(name)
    }
}

/// The per-guard context `try_acquire` hands to each spawned guard task:
/// everything a held lock needs beyond its own name and table. Grouped into one
/// value (rather than threaded as separate parameters) to keep
/// `try_acquire`/`run_guard_task` within a sane argument count. Cheap to clone
/// (a pool handle, three `Arc`s, a `&'static str`, and a `CancellationToken`
/// clone).
#[derive(Clone)]
struct GuardContext {
    /// The write pool, carrying every `cluster_lock` statement and `pg_notify`.
    pool: PgPool,
    /// See [`PostgresLock::beacon`] — bound by every acquisition into the row it
    /// writes, and by every `renew` as the fence proving we can still defend it.
    beacon: Beacon,
    /// The ADR-004 metrics sink (DESIGN.md §8).
    metrics: Arc<dyn ClusterMetrics>,
    /// The bounded `provider` label.
    provider: &'static str,
    /// The shutdown token guard tasks observe (PGR-L2).
    shutdown: CancellationToken,
    /// See [`PostgresLock::deadline_hint`] — signalled by `try_acquire` and by
    /// the guard task's `renew`.
    deadline_hint: Arc<Notify>,
    /// The lock TTL reaper's own interval, which is also the upper bound on how
    /// long any one of its sleeps can last. [`should_hint`] compares a TTL
    /// against it to decide whether signalling `deadline_hint` could possibly
    /// tell the reaper something it will not find out in time anyway.
    reaper_interval: Duration,
}

/// Whether writing a deadline `ttl` from now is worth signalling
/// [`PostgresLock::deadline_hint`] for, given the reaper's `interval`.
///
/// The hint exists for one case: a lock whose entire lifetime fits inside a sleep
/// the reaper computed before that lock existed. Outside that case it is pure
/// cost, and the cost is not small — `tokio::sync::Notify` holds one permit, so a
/// process signalling on every write keeps a permit permanently pending, the
/// reaper's `notified()` branch permanently ready, and every sleep collapsed to
/// the 100 ms wake floor. At a couple hundred renewals a second that is a full
/// sweep and next-expiry probe every 100 ms instead of every `interval` —
/// roughly fifty times the intended database load, permanently, on every
/// instance in the fleet.
///
/// So the test is exact rather than heuristic: the reaper's in-flight sleep is
/// capped at `interval` (`next_delay` takes `min(until_metrics, ..)` and
/// `until_metrics` never exceeds it), so a deadline at least `interval` away is
/// guaranteed to be re-read, from the table, before it falls due. Only a shorter
/// one can slip through, and only that one is signalled.
fn should_hint(ttl: Duration, interval: Duration) -> bool {
    ttl < interval
}

/// How `lock()` ends when its budget runs out: `ClusterError::LockTimeout`,
/// unless the last attempt never reached Postgres — in which case that outage is
/// the truthful answer, and the one that tells a caller to alert rather than to
/// retry. See `lock()`'s `last_transient`.
fn timed_out(
    last_transient: &mut Option<ClusterError>,
    name: &str,
    started: tokio::time::Instant,
) -> ClusterError {
    last_transient
        .take()
        .unwrap_or_else(|| ClusterError::LockTimeout {
            name: name.to_owned(),
            waited: started.elapsed(),
        })
}

/// Whether `err` is a beacon outage that will clear on its own — the beacon task
/// reconnects with a 200ms..5s capped backoff — and so should be retried inside
/// `lock()`'s budget rather than ending it.
///
/// `ConnectionLost` is the kind `beacon::beacon_unavailable` produces for "no live
/// beacon to stamp a row with"; a pool-side `ConnectionLost` is equally transient,
/// so the same treatment is right for it. Every other kind (`AuthFailure`,
/// `Timeout`, `Other`, and the non-provider variants) either will not clear by
/// waiting or is already the caller's answer.
fn is_transient_session_loss(err: &ClusterError) -> bool {
    matches!(
        err,
        ClusterError::Provider {
            kind: ProviderErrorKind::ConnectionLost,
            ..
        }
    )
}

/// Deletes the `cluster_lock` row an abandoned acquisition wrote, so a bail-out
/// leaves no metadata behind.
///
/// Fenced on `holder_id`, which is what makes it safe to call unconditionally:
/// if the name has already been re-acquired — here or on another instance — the
/// successor's `ON CONFLICT DO UPDATE` replaced `holder_id`, so this matches zero
/// rows rather than deleting a live holder's metadata.
///
/// Best-effort: the caller is already returning an error, and a row left behind
/// is reclaimed by the TTL sweep.
async fn discard_row(table: &str, name: &str, holder_id: Uuid, pool: &PgPool) {
    let _deleted = sqlx::query(AssertSqlSafe(format!(
        "DELETE FROM {table} WHERE name = $1 AND holder_id = $2"
    )))
    .bind(name)
    .bind(holder_id)
    .execute(pool)
    .await;
}

/// The acquire statement (DESIGN.md §5.1). One function so the `EXPLAIN ANALYZE`
/// seam (`PG-SPEC-012`) plans the statement acquisition actually runs, rather
/// than a copy of it that could drift.
///
/// Binds: `$1` name, `$2` `holder_id`, `$3` ttl ms, `$4`/`$5` beacon halves.
fn acquire_sql(table: &str) -> String {
    format!(
        "INSERT INTO {table} \
         (name, holder_id, acquired_at, expires_at, holder_beacon_hi, holder_beacon_lo) \
         VALUES ($1, $2, now(), {expires_at}, $4, $5) \
         ON CONFLICT (name) DO UPDATE SET holder_id = EXCLUDED.holder_id, \
         acquired_at = EXCLUDED.acquired_at, expires_at = EXCLUDED.expires_at, \
         holder_beacon_hi = EXCLUDED.holder_beacon_hi, \
         holder_beacon_lo = EXCLUDED.holder_beacon_lo \
         WHERE {stealable} \
         RETURNING 1",
        expires_at = expires_at_sql(3),
        stealable = stealable_predicate(table),
    )
}

/// Attempts the acquisition shared by `try_lock` and `lock`'s retry loop: one
/// conditional upsert against `cluster_lock`, and on success a `local_holders`
/// registration plus a guard task.
///
/// `Ok(None)` is contention — the row exists, is unexpired, and its beacon is
/// still granted — which is the same answer whether the rival is another
/// instance or another task in this process (the module doc).
async fn try_acquire(
    table: &str,
    local_holders: &Arc<DashMap<String, Uuid>>,
    name: &str,
    ttl: Duration,
    ctx: &GuardContext,
) -> Result<Option<LockGuard>, ClusterError> {
    // Reject un-notifiable names before mutating any lock state, so `release`
    // never reaches a lock it cannot cleanly signal (see `validate_lock_name`).
    validate_lock_name(name)?;
    let ttl_ms = ttl_to_millis(ttl)?;

    // Shutdown, checked *before* any lock work (PGR-L2). The re-checks below
    // exist to unwind an acquisition that raced `stop()`; this one keeps the
    // common case from starting one at all — no row to delete, nothing to undo.
    //
    // It is also what makes the answer honest. Without it an acquisition arriving
    // after `stop()` reaches the pool, fails with `ConnectionLost`, and — since
    // `lock()` retries that as transient — spends the caller's entire timeout
    // before reporting `LockTimeout` for a backend that is simply gone.
    if ctx.shutdown.is_cancelled() {
        return Err(ClusterError::Shutdown);
    }

    // The beacon vouching for the row this acquisition is about to write, read
    // before anything is mutated so there is nothing to unwind if it is absent.
    //
    // **Failing here is a correctness requirement, not a convenience.** A row
    // stamped with a dead incarnation's key — or with none — hands the caller a
    // guard for a lock every other instance can steal on sight, because the
    // liveness predicate finds that beacon absent from `pg_locks`. The consumer
    // would enter its critical section believing it holds a lock that is already
    // free to the fleet, and would not find out until its next `renew`. This is
    // the acquire-side mirror of the beacon fence in `renew`.
    //
    // `ConnectionLost` is transient (`is_transient_session_loss`), so a blip
    // inside a caller's `lock()` budget still resolves into a successful
    // acquisition rather than a spurious failure.
    let Some(beacon_key) = ctx.beacon.key() else {
        return Err(beacon_unavailable("no live beacon to stamp the row with"));
    };

    let holder_id = Uuid::new_v4();
    // The whole acquisition, in one statement (DESIGN.md §5.1).
    //
    // `ON CONFLICT (name) DO UPDATE ... WHERE <stealable>` rather than a bare
    // `INSERT`, and rather than a `SELECT` first. Three variants look equivalent
    // and are not:
    //
    // * `SELECT` to check, then `INSERT`, is a check-then-act race.
    // * Letting the primary key's unique violation *be* the contention signal
    //   cannot express "steal if expired", so a lapsed lock would be permanently
    //   unacquirable.
    // * `SELECT ... FOR UPDATE` then `UPDATE` needs an explicit transaction and
    //   locks nothing when no row exists yet, so two first-time acquirers both
    //   proceed and one takes a unique violation.
    //
    // `RETURNING 1` is the answer: a row means acquired (by insert or by steal),
    // zero rows means contended. The loser blocks on the winner's row lock and
    // then re-evaluates the predicate against the winner's *committed* state,
    // which is why there is no third case — and why `READ COMMITTED` is asserted
    // at startup (`pg_setup::assert_read_committed`).
    let acquired: Option<i32> = sqlx::query_scalar(AssertSqlSafe(acquire_sql(table)))
        .bind(name)
        .bind(holder_id)
        .bind(ttl_ms)
        .bind(beacon_key.hi)
        .bind(beacon_key.lo)
        .fetch_optional(&ctx.pool)
        .await
        .map_err(map_sqlx_error)?;

    if acquired.is_none() {
        return Ok(None);
    }

    // Serialize against shutdown (PGR-L2), in two parts for the same reason the
    // beacon check below is in two parts. First the cheap pre-registration bail:
    // if shutdown is already observed we delete the row and never register,
    // avoiding the work entirely in the common single-threaded case.
    if ctx.shutdown.is_cancelled() {
        discard_row(table, name, holder_id, &ctx.pool).await;
        return Err(ClusterError::Shutdown);
    }

    let (rx, guard) = LockGuard::channel(name.to_owned(), 4);
    local_holders.insert(name.to_owned(), holder_id);

    tokio::spawn(run_guard_task(
        name.to_owned(),
        Arc::clone(local_holders),
        table.to_owned(),
        ctx.clone(),
        holder_id,
        rx,
    ));

    // The beacon may have been lost while that row was being written, and the
    // loss purges `local_holders` — so an entry registered *after* the purge
    // would advertise a lock this instance can no longer defend. Re-checking
    // after the insert has no gap: a loss before it is caught here, and one after
    // it finds the entry and purges it.
    //
    // Compared by *key*, not merely presence: a fast reconnect between the two
    // reads leaves a live beacon that is nevertheless a different incarnation
    // from the one stamped on the row, and that row is already stealable.
    if ctx.beacon.key() != Some(beacon_key) {
        local_holders.remove_if(name, |_, id| *id == holder_id);
        discard_row(table, name, holder_id, &ctx.pool).await;
        return Err(beacon_unavailable(
            "the beacon was lost mid-acquisition, so this row is no longer vouched for",
        ));
    }

    // Second, the post-registration shutdown re-check, which closes the window
    // the pre-registration one cannot: on a multi-worker runtime a concurrent
    // `stop()` may cancel and finish draining *between* that check and this
    // `insert`. Returning the guard would hand back a lock shutdown has already
    // given up. `remove_if` is atomic with the drain's own clear, and the row
    // delete is `holder_id`-fenced, so running it either way is free.
    if ctx.shutdown.is_cancelled() {
        local_holders.remove_if(name, |_, id| *id == holder_id);
        // Deleted but not NOTIFYed: the drain announces the names it took, and a
        // waiter on this one falls back to its 250ms heartbeat. A rare race path
        // during shutdown is the wrong place to spend a round-trip.
        discard_row(table, name, holder_id, &ctx.pool).await;
        return Err(ClusterError::Shutdown);
    }

    // Wake the TTL reaper so this acquisition's deadline is visible to its next
    // sleep instead of only from its next interval wake (`deadline_hint`).
    // Signalled last, once the row is committed, registered, and definitely ours:
    // an earlier signal could send the reaper to probe a row one of the bails
    // above is about to delete.
    //
    // And only when the deadline could actually be missed — see [`should_hint`]
    // for why an unconditional signal pins the reaper at its wake floor forever.
    if should_hint(ttl, ctx.reaper_interval) {
        ctx.deadline_hint.notify_one();
    }

    Ok(Some(guard))
}

/// Drives one held lock's [`LockCommandReceiver`](cluster_sdk::LockCommandReceiver)
/// until [`Release`](cluster_sdk::LockRequest::Release) or the consumer drops the
/// [`LockGuard`] without releasing — in which case this task simply exits and the
/// row is left to lapse at its TTL, exactly per the trait's TTL safety-net
/// contract (no I/O in `Drop`).
///
/// Carries no `synchronous_commit` re-assertion timer of its own: every statement
/// it issues goes through the write pool, whose `after_connect`/`before_acquire`
/// hooks enforce the GUC on every checkout (DESIGN.md §3.4).
async fn run_guard_task(
    name: String,
    local_holders: Arc<DashMap<String, Uuid>>,
    table: String,
    ctx: GuardContext,
    holder_id: Uuid,
    mut commands: cluster_sdk::LockCommandReceiver,
) {
    loop {
        tokio::select! {
            // Graceful shutdown: exit promptly rather than waiting for the
            // consumer to act (PGR-L2). The row is left for `drain_beacon_rows`
            // (DESIGN.md §10 step 4) to delete and announce — exactly as it is for
            // a consumer that drops its guard without releasing, so renew/release
            // are no-ops from here.
            () = ctx.shutdown.cancelled() => return,
            request = commands.recv() => {
                let Some(request) = request else {
                    // Consumer dropped the guard without releasing: exit, leaving
                    // the row for the TTL sweep (or any other acquirer's own
                    // predicate) to reclaim.
                    return;
                };
                match request {
                    cluster_sdk::LockRequest::Renew { new_ttl, responder } => {
                        let span = tracing::info_span!(
                            spans::LOCK_RENEW, provider = %ctx.provider, lock = %name
                        );
                        let started = std::time::Instant::now();
                        let result =
                            renew(&local_holders, &name, &table, new_ttl, holder_id, &ctx)
                                .instrument(span)
                                .await;
                        record_lock(&*ctx.metrics, ctx.provider, "renew", &name, started, &result);
                        responder.respond(result);
                    }
                    cluster_sdk::LockRequest::Release { responder } => {
                        let span = tracing::info_span!(
                            spans::LOCK_RELEASE, provider = %ctx.provider, lock = %name
                        );
                        let started = std::time::Instant::now();
                        let result = release(&local_holders, &name, &table, holder_id, &ctx)
                            .instrument(span)
                            .await;
                        record_lock(&*ctx.metrics, ctx.provider, "release", &name, started, &result);
                        responder.respond(result);
                        return;
                    }
                }
            }
        }
    }
}

/// `Renew`: pushes `expires_at` out (and restamps `acquired_at`) without
/// relinquishing the lock.
///
/// Authoritative against a single truth, in one statement and with no probe. The
/// `WHERE` carries every fence there is:
///
/// * **`holder_id`** (PGR-L1) guards against a **successor** — if our TTL lapsed
///   and a newer holder took the name, resetting the deadline would extend *its*
///   lease under our name.
/// * **`expires_at > now()`** refuses to resurrect a lease that has already
///   lapsed. The row may still be sitting there unswept, but the fleet is
///   entitled to treat it as free, so renewing it would re-take a lock we might
///   simultaneously be losing.
/// * **`holder_beacon_*`** guards against **ourselves** — a different question,
///   and the one the old session-generation counter answered from process
///   memory. If this instance's beacon has been replaced since the acquisition,
///   the row carries the dead key and every other instance can already steal it;
///   reporting a healthy renewal for it would be the one way this design could
///   silently lose mutual exclusion.
///
/// Zero rows updated therefore means `LockExpired`, whichever fence failed: the
/// caller no longer holds this lock, and its correct response is to abort its
/// critical section rather than retry.
///
/// Signals `deadline_hint` on success. A renew usually pushes the deadline
/// *later*, which only ever makes the reaper's current sleep conservative — but
/// `renew` takes an arbitrary `new_ttl`, so a short one can move the earliest
/// deadline *earlier* than the sleep already in flight was told about.
async fn renew(
    local_holders: &Arc<DashMap<String, Uuid>>,
    name: &str,
    table: &str,
    new_ttl: Duration,
    holder_id: Uuid,
    ctx: &GuardContext,
) -> Result<(), ClusterError> {
    let expired = || ClusterError::LockExpired {
        name: name.to_owned(),
    };
    // No live beacon means this instance cannot defend anything it holds — the
    // rows are stealable on sight — so there is nothing to renew. `LockExpired`
    // rather than a `Provider` error, for the same reason as a lost row: the
    // consumer's correct response is to abort its critical section, not to retry
    // a renewal that can never succeed for this acquisition.
    let Some(beacon_key) = ctx.beacon.key() else {
        local_holders.remove_if(name, |_, id| *id == holder_id);
        return Err(expired());
    };

    let ttl_ms = ttl_to_millis(new_ttl)?;
    let updated: Option<i32> = sqlx::query_scalar(AssertSqlSafe(format!(
        "UPDATE {table} SET acquired_at = now(), expires_at = {expires_at} \
         WHERE name = $2 AND holder_id = $3 AND expires_at > now() \
           AND holder_beacon_hi = $4 AND holder_beacon_lo = $5 \
         RETURNING 1",
        expires_at = expires_at_sql(1),
    )))
    .bind(ttl_ms)
    .bind(name)
    .bind(holder_id)
    .bind(beacon_key.hi)
    .bind(beacon_key.lo)
    .fetch_optional(&ctx.pool)
    .await
    .map_err(map_sqlx_error)?;

    if updated.is_none() {
        // Whatever the reason, this acquisition is over: drop the local entry so
        // the registry does not keep advertising a guard for it. Fenced on our own
        // `holder_id`, so a re-acquisition of the same name by this instance is
        // left alone.
        local_holders.remove_if(name, |_, id| *id == holder_id);
        return Err(expired());
    }

    // Same gate as `try_acquire`'s — see [`should_hint`]. This call site is the
    // one that made an unconditional signal pathological rather than merely
    // wasteful: a renewal is a *heartbeat*, so it repeats for the whole life of
    // every held lock, and a fleet steadily renewing its leases kept a permit
    // permanently pending and the reaper permanently at its floor.
    if should_hint(new_ttl, ctx.reaper_interval) {
        ctx.deadline_hint.notify_one();
    }
    Ok(())
}

/// `Release`: deletes the lease row and wakes any blocked `lock()` waiters.
///
/// Fenced on `holder_id` (PGR-L1) in the statement itself, which is the whole
/// fence needed: a lock stolen after a beacon loss carries the *successor's*
/// `holder_id` and will not match, so a stale guard cannot delete a live holder's
/// row. No beacon check, deliberately — releasing a lock we may no longer hold is
/// harmless (the row is ours by `holder_id` or it is not ours at all), and
/// refusing to release during a blip would leave a row for the TTL for no gain.
///
/// Best-effort `Ok(())` when the local entry is already gone: the TTL sweep or a
/// successor got there first, which is the trait's TTL-lapsed narrowing
/// (DESIGN.md §3.7) rather than a failure.
async fn release(
    local_holders: &Arc<DashMap<String, Uuid>>,
    name: &str,
    table: &str,
    holder_id: Uuid,
    ctx: &GuardContext,
) -> Result<(), ClusterError> {
    // Removed first and atomically, so a concurrent sweep or drain cannot also
    // act on this entry. A non-matching (or absent) entry means one of them
    // already did.
    if local_holders
        .remove_if(name, |_, id| *id == holder_id)
        .is_none()
    {
        return Ok(());
    }

    // Delete and notify in **one** statement, so releasing costs one pool
    // checkout rather than two. What a blocked `lock()` caller elsewhere measures
    // is the delay from `release()` being called to its own wake, and every
    // checkout on this path adds a round-trip to that (plus the pool's
    // `before_acquire` `SET`) — `PG-LOCK-003` asserts the wake lands well inside
    // the 250ms heartbeat fallback, i.e. that waiters are woken by the NOTIFY and
    // not by the polling backstop.
    //
    // The data-modifying CTE runs unconditionally: PostgreSQL executes a `WITH`
    // statement that modifies data exactly once and to completion whether or not
    // the primary query reads its output. Committing both together also makes the
    // wake atomic with the row's disappearance, where two separate statements
    // left a window in which a woken waiter could still see the old row.
    //
    // Selecting `FROM released` makes the NOTIFY conditional on the DELETE having
    // matched, *without* costing a second statement or making the delete
    // conditional (per the paragraph above, the CTE still runs on an empty
    // result). A stale guard whose row a successor already stole therefore stops
    // announcing a release that did not happen — the channel is shared, so every
    // blocked `lock()` waiter on it wakes and retries for nothing, and §11 names
    // aggregate NOTIFY rate as the primary scaling risk.
    sqlx::query(AssertSqlSafe(format!(
        "WITH released AS (DELETE FROM {table} WHERE name = $1 AND holder_id = $2 RETURNING 1) \
         SELECT pg_notify($3, $1) FROM released"
    )))
    .bind(name)
    .bind(holder_id)
    .bind(notify::RELEASE_CHANNEL)
    .execute(&ctx.pool)
    .await
    .map_err(map_sqlx_error)?;
    Ok(())
}

#[async_trait]
impl DistributedLockBackend for PostgresLock {
    fn features(&self) -> LockFeatures {
        // A single conditional upsert on a primary key, against one Postgres
        // primary, under the same `synchronous_commit = on` enforcement as the
        // cache (DESIGN.md §3.4): one node, one serialization point, so the
        // exclusion decision is linearizable in exactly the sense the flag names
        // — "eventually-consistent backends may transiently grant the same lock
        // to two holders under partition" is the failure it rules out, and this
        // has no such mode. Unchanged by the move off advisory locks: same node,
        // same serialization point, and the same basis on which the CAS-based
        // default backend declares its own `true` (`cluster/src/defaults/lock.rs`).
        LockFeatures::new(true)
    }

    async fn try_lock(&self, name: &str, ttl: Duration) -> Result<LockGuard, ClusterError> {
        let span =
            tracing::info_span!(spans::LOCK_TRY_LOCK, provider = %self.provider, lock = %name);
        let started = std::time::Instant::now();
        let ctx = self.guard_context();
        let out = async {
            match try_acquire(&self.table, &self.local_holders, name, ttl, &ctx).await? {
                Some(guard) => Ok(guard),
                None => Err(ClusterError::LockContended {
                    name: name.to_owned(),
                }),
            }
        }
        .instrument(span)
        .await;
        record_lock(
            &*self.metrics,
            self.provider,
            "try_lock",
            name,
            started,
            &out,
        );
        out
    }

    async fn lock(
        &self,
        name: &str,
        ttl: Duration,
        timeout: Duration,
    ) -> Result<LockGuard, ClusterError> {
        // DESIGN.md §5.3's loop, adapted to sqlx's public API: rather than
        // `LISTEN`ing on a connection this task owns (sqlx's `PgListener` owns
        // its own single-connection pool, with no public way to hand it an
        // already-checked-out `PoolConnection`), wait on the in-process
        // `release_waiters` registry the dedicated LISTEN task
        // (`notify::spawn_release_listen_task`) feeds. A short heartbeat retry
        // runs alongside the wake-up as a safety net against a missed
        // notification (task not started yet, a dropped wake), so a lost wake
        // only costs latency up to the heartbeat interval, never correctness —
        // the loop always re-attempts the acquire statement itself
        // (`try_acquire`, §5.1) as the source of truth.
        const HEARTBEAT: Duration = Duration::from_millis(250);

        let span = tracing::info_span!(spans::LOCK_LOCK, provider = %self.provider, lock = %name);
        let op_started = std::time::Instant::now();
        let ctx = self.guard_context();
        let out = async {
            let started = tokio::time::Instant::now();
            let deadline = started + timeout;

            let mut first_attempt = true;
            // The most recent beacon outage, if the last attempt never
            // reached Postgres. It replaces `LockTimeout` at every exit below, so
            // a caller that waited out its whole budget against a *dead* backend
            // is told that, rather than being handed the same `LockTimeout` that
            // ordinary contention produces — the distinction `try_lock` already
            // makes, and the one a caller needs to decide between retrying and
            // alerting.
            //
            // Deliberately not a shorter give-up budget: retrying for the full
            // timeout is what carries a `lock()` through a Postgres failover
            // (commonly 10-30s), and cutting that short to improve an error code
            // would trade a real availability property for a cosmetic one.
            let mut last_transient: Option<ClusterError> = None;
            loop {
                // Bound each *subsequent* acquire attempt by the remaining budget,
                // not just the gap between attempts (PGR-L3): the metadata write
                // can block on `pool.acquire()` for up to `pool_acquire_timeout`
                // (default 5s), so checking `deadline` only after each full
                // `try_acquire` let a single attempt overshoot the caller's
                // `timeout`.
                //
                // What a cancelled attempt leaves behind depends on how far it got.
                // Cancelled *after* the upsert committed but *before* it
                // registered, it leaves a row this instance owns with no local
                // guard — an orphan, wedging its name for the whole fleet until the
                // TTL, since the row is unexpired and vouched for by a live beacon.
                // The reaper's orphan sweep (`reaper::sweep_orphans`) is what
                // reclaims it, exactly. Cancelled *after* the registration, the row
                // has a live entry and the sweep correctly leaves it alone; that
                // case is indistinguishable from a consumer dropping its
                // `LockGuard` without releasing, and resolves the same way — the
                // TTL sweep reclaims it, per the trait's safety-net contract.
                //
                // The *first* attempt always runs (bounded only by the pool's own
                // acquire timeout), even when `timeout` is 0/expired — matching the
                // CAS-based default's attempt-before-budget-check ordering, so
                // `lock(free_lock, ttl, Duration::ZERO)` still acquires instead of
                // returning `LockTimeout` without ever trying.
                let now = tokio::time::Instant::now();
                let remaining = deadline.saturating_duration_since(now);
                if remaining.is_zero() && !first_attempt {
                    return Err(timed_out(&mut last_transient, name, started));
                }

                let acquire = try_acquire(&self.table, &self.local_holders, name, ttl, &ctx);
                let outcome = if remaining.is_zero() {
                    // First attempt with no remaining budget: run it unbounded
                    // (pool-acquire-timeout bounds it in practice) rather than skip.
                    acquire.await
                } else {
                    match tokio::time::timeout(remaining, acquire).await {
                        Ok(result) => result,
                        Err(_elapsed) => {
                            return Err(timed_out(&mut last_transient, name, started));
                        }
                    }
                };
                let attempted = match outcome {
                    Ok(attempted) => {
                        // This attempt reached Postgres and got a real answer
                        // (acquired, or contended). Any earlier outage has been
                        // superseded, so a later timeout is genuine contention.
                        last_transient = None;
                        attempted
                    }
                    // A beacon between loss and reconnect fails every acquisition
                    // fast (`beacon::beacon_unavailable`), and the reconnect
                    // backoff is 200ms..5s — well inside a typical `lock` budget. A
                    // caller that asked for 30s of patience should get it, so this is
                    // treated exactly like contention: wait, then retry. Only the
                    // caller's own deadline ends the loop, as `LockTimeout`.
                    // Retained so that, if the budget runs out while the beacon is
                    // still unavailable, the caller gets this rather than a
                    // `LockTimeout` that would read as ordinary contention.
                    Err(err) if is_transient_session_loss(&err) => {
                        last_transient = Some(err);
                        None
                    }
                    Err(err) => return Err(err),
                };
                first_attempt = false;
                if let Some(guard) = attempted {
                    return Ok(guard);
                }

                let now = tokio::time::Instant::now();
                if now >= deadline {
                    return Err(timed_out(&mut last_transient, name, started));
                }

                let wait_for_release = self.release_waiters.wait_for(name);
                let remaining = deadline - now;
                let heartbeat = HEARTBEAT.min(remaining);
                tokio::select! {
                    _ = wait_for_release => {}
                    () = tokio::time::sleep(heartbeat) => {}
                }
            }
        }
        .instrument(span)
        .await;
        record_lock(
            &*self.metrics,
            self.provider,
            "lock",
            name,
            op_started,
            &out,
        );
        out
    }
}

/// Standalone lock-only plugin (DESIGN.md §3.5): lets an operator route `lock`
/// to Postgres independently of `cache`. Migrates only `0002_cluster_lock.sql`
/// — a lock-only deployment never creates `cluster_cache`.
pub struct PostgresLockPlugin;

impl PostgresLockPlugin {
    // No `#[must_use]` here: `PostgresLockBuilder` itself already carries a
    // `#[must_use = "..."]` message, so a bare attribute on this function
    // would be a `clippy::double_must_use` no-op.
    pub fn builder(config: PostgresLockConfig) -> PostgresLockBuilder {
        PostgresLockBuilder {
            config,
            reaper_meter: None,
        }
    }
}

/// Fluent builder for [`PostgresLockPlugin`].
#[must_use = "a builder starts nothing until `.build_and_start()` is called"]
pub struct PostgresLockBuilder {
    config: PostgresLockConfig,
    /// Optional override for the meter the lock TTL reaper emits its
    /// plugin-local gauge/histogram through (DESIGN.md §8). `None` in
    /// production (uses the process-global meter); tests inject a meter over an
    /// in-memory reader so `pg_spec_006` can read the gauge back in isolation
    /// from other tests' reapers.
    reaper_meter: Option<opentelemetry::metrics::Meter>,
}

impl PostgresLockBuilder {
    /// Test-only: routes the lock reaper's plugin-local metrics through `meter`
    /// instead of the process-global meter, so a test can attach an in-memory
    /// reader and observe `cluster_postgres_lock_active_names` without
    /// contention from other concurrently-running tests' reapers.
    ///
    /// Gated behind `--features integration` (PGR-M8).
    #[cfg(feature = "integration")]
    #[doc(hidden)]
    pub fn __with_reaper_meter(mut self, meter: opentelemetry::metrics::Meter) -> Self {
        self.reaper_meter = Some(meter);
        self
    }
}

impl PostgresLockBuilder {
    /// Builds the plugin: opens its own dedicated `sqlx::PgPool`, runs the
    /// `0002_cluster_lock.sql` migration, enforces `synchronous_commit = on`
    /// (DESIGN.md §3.4), and starts the lock TTL reaper.
    ///
    /// # Errors
    /// - [`ClusterError::InvalidConfig`] if `pgbouncer_transaction_mode: true`
    ///   is set (DESIGN.md §5.4) or the connection string is invalid
    ///   (`PG-LIFE-006`).
    pub async fn build_and_start(self) -> Result<PostgresLockHandle, ClusterError> {
        let config = self.config;
        let reaper_meter = self.reaper_meter;
        reject_pgbouncer_transaction_mode(config.pgbouncer_transaction_mode)?;
        // Reject an unsafe schema (PGR-L4) and a zero lock reaper interval
        // (PGR-E2) before opening the pool or spawning the reaper.
        config.validate()?;

        let pool = base_pool_options(&config.schema)
            .max_connections(config.pool_max_size)
            .acquire_timeout(config.pool_acquire_timeout())
            .connect(&config.connection_string)
            .await
            .map_err(map_sqlx_error)?;

        // Before any DDL: acquire is a `READ COMMITTED` guarded upsert, so a
        // stricter server default is a startup error rather than a per-acquire
        // serialization failure later (§3.2).
        assert_read_committed(&pool).await?;

        // Create the configured schema (if non-`public`) before the migrator's
        // unqualified `CREATE TABLE` runs — the pool's `search_path` already
        // points every connection at it (PGR-L4). Only the `migrations/lock/`
        // Migrator — never `migrations/cache/` — so a lock-only deployment
        // never creates `cluster_cache`.
        ensure_schema(&pool, &config.schema).await?;
        run_migrator(lock_migrator(), &pool).await?;
        warn_if_async_replication(&pool, config.replication_mode).await?;

        // The single ADR-004 metrics sink, shared by the native lock backend
        // (its `try_lock`/`lock`/`renew`/`release` signals, DESIGN.md §8) and
        // the lock TTL reaper (its `emit_provider_error` failure signals).
        let metrics: Arc<dyn ClusterMetrics> = Arc::new(
            cluster_sdk::observability::otel::OtelClusterMetrics::from_global_meter(
                crate::provider::PROVIDER_NAME,
            ),
        );

        // Created before the lock so guard tasks can observe the same shutdown
        // signal the reaper/LISTEN tasks do (PGR-L2).
        let shutdown = CancellationToken::new();
        // Establishes this instance's liveness beacon (DESIGN.md §3.3, §5.1)
        // eagerly, so an unreachable database fails here rather than on the first
        // `try_lock`.
        let LockRuntime { lock, beacon } = PostgresLock::start(LockInit {
            pool,
            connection_string: config.connection_string.clone(),
            schema: config.schema.clone(),
            reaper_interval: config.lock_reaper_interval(),
            metrics: Arc::clone(&metrics),
            provider: crate::provider::PROVIDER_NAME,
            guard_shutdown: shutdown.clone(),
        })
        .await?;

        let meter = reaper_meter.unwrap_or_else(reaper::reaper_meter);
        let reaper = lock.spawn_reaper(
            reaper::LockReaperMetrics::new(
                &meter,
                crate::provider::PROVIDER_NAME,
                Arc::clone(&metrics),
            ),
            i64::from(config.lock_name_cardinality_warn_threshold),
            shutdown.clone(),
        );
        let release_listener = lock
            .spawn_release_listener(config.connection_string.clone(), shutdown.clone())
            .await;

        Ok(PostgresLockHandle {
            lock,
            beacon: Some(beacon),
            reaper: Some(reaper),
            release_listener: Some(release_listener),
            shutdown,
            stopped: false,
        })
    }
}

/// The running standalone lock plugin. Carries the same `stopped: bool` field
/// and ADR-006 `Drop` guard as [`crate::PostgresClusterHandle`] (DESIGN.md
/// §3.5) — it owns its own pool and lock TTL reaper independently.
pub struct PostgresLockHandle {
    lock: Arc<PostgresLock>,
    /// This instance's liveness beacon (`lock::beacon`, DESIGN.md §3.3).
    /// Deliberately owned here rather than by `PostgresLock`, and shut down
    /// *after* the drain — the drain deletes this instance's rows by beacon key,
    /// so the key must still be readable when it runs, and releasing the beacon
    /// early would assert to the fleet that this instance is dead. `Option` for
    /// the same `Drop`-impl reason as the join handles below.
    beacon: Option<BeaconHandle>,
    // `Option` (not a bare `JoinHandle`) because `PostgresLockHandle` owns a
    // `Drop` impl below, and you cannot move a field out of a type that
    // implements `Drop` — `stop` uses `.take()` to drain it in place, mirroring
    // `ClusterHandle::stop`'s `std::mem::take` (`cluster/src/wiring.rs`).
    reaper: Option<tokio::task::JoinHandle<()>>,
    release_listener: Option<tokio::task::JoinHandle<()>>,
    shutdown: CancellationToken,
    /// Set by `stop` so the `Drop` guard can tell a graceful shutdown apart
    /// from a forgotten one (ADR-006 §Confirmation).
    stopped: bool,
}

impl PostgresLockHandle {
    /// The native lock backend.
    #[must_use]
    pub fn lock(&self) -> Arc<dyn DistributedLockBackend> {
        Arc::clone(&self.lock) as Arc<dyn DistributedLockBackend>
    }

    /// Test-only access to the concrete [`PostgresLock`], for the beacon and
    /// sweep seams (the `dyn` [`lock`](Self::lock) has none of them).
    /// Gated behind `--features integration` (PGR-M8).
    #[cfg(feature = "integration")]
    #[doc(hidden)]
    #[must_use]
    pub fn __test_lock(&self) -> Arc<PostgresLock> {
        Arc::clone(&self.lock)
    }

    /// Test-only access to the write pool, so `PG-LOCK-009`/`PG-SPEC-005` can
    /// assert `synchronous_commit` on the connections this plugin's statements
    /// actually run on. Mirrors `PostgresClusterHandle::__test_pool`.
    ///
    /// The pool is where that assertion belongs now: with the lock session gone,
    /// every statement the lock issues is a pooled one, and the beacon — the only
    /// long-lived connection left — writes nothing at all (`lock::beacon`).
    ///
    /// Gated behind `--features integration` (PGR-M8).
    #[cfg(feature = "integration")]
    #[doc(hidden)]
    #[must_use]
    pub fn __test_pool(&self) -> PgPool {
        self.lock.pool()
    }

    /// Cancels the lock TTL reaper and release-wake listener, hands back every
    /// lock still held, then closes the beacon and the pool (DESIGN.md §10).
    ///
    /// The order matters: the drain deletes and announces this instance's rows
    /// *by beacon key, on the pool*, so both the key and the pool have to still be
    /// live when it runs. Anything it misses stops being vouched for the moment
    /// the beacon connection closes a step later.
    pub async fn stop(mut self) {
        self.shutdown.cancel();
        for task in [self.reaper.take(), self.release_listener.take()]
            .into_iter()
            .flatten()
        {
            let _exited = task.await;
        }
        self.lock.drain_beacon_rows().await;
        // After the drain, never before: releasing the beacon while this process
        // is still running asserts that the instance is dead (`lock::beacon`).
        if let Some(beacon) = self.beacon.take() {
            beacon.shutdown().await;
        }
        close_pool(&self.lock.pool()).await;
        self.stopped = true;
    }
}

impl Drop for PostgresLockHandle {
    fn drop(&mut self) {
        // `cancel_and_diagnose_drop` owns the cancel-before-diagnosis ordering
        // for both handles — see its doc for why that ordering is the point and
        // why the decision comes back as a value instead of being emitted there.
        match cancel_and_diagnose_drop(self.stopped, &self.shutdown) {
            DropDiagnosis::StoppedCleanly => {}
            DropDiagnosis::DuringPanic => warn!(
                "PostgresLockHandle dropped during panic unwind without stop(); \
                 skipping debug panic to avoid double-panic abort"
            ),
            DropDiagnosis::Unstopped => {
                #[cfg(debug_assertions)]
                panic!("PostgresLockHandle dropped without stop() - programming error");
                #[cfg(not(debug_assertions))]
                warn!(
                    "PostgresLockHandle dropped without stop() - programming error; \
                     background tasks may leak"
                );
            }
        }
    }
}

#[cfg(test)]
#[path = "lock_tests.rs"]
mod tests;
