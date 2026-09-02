//! `PostgresLock` — the native `DistributedLockBackend` implementation over the
//! `cluster_lock` lease row (DESIGN.md §5), plus the standalone
//! `PostgresLockPlugin` builder/handle (DESIGN.md §3.5) that lets an operator
//! route `lock` to Postgres independently of `cache`.
//!
//! ## The row is the arbiter, and the row is a lease
//!
//! A lock is held **iff** a `cluster_lock` row exists whose `expires_at` is in the
//! future. That is the whole predicate: `expires_at` is the only liveness
//! authority, so no process vouches for a lease and no process's death ends one
//! (invariant I7, DESIGN.md). Every acquire, renew, and
//! release is a single statement against the write pool, with no session affinity
//! and **no in-process state that is load-bearing for exclusion**.
//!
//! The row *is* the [`LeaseRecord`](cluster_sdk::lease::LeaseRecord) of §5.8.1,
//! held in columns rather than in an encoded cache value: `owner` and `fence` are
//! the [`LeaseToken`] the holder presents, and `expires_at` is its deadline. Both
//! halves of [`DistributedLockBackend`] are served from that one lease —
//! [`acquire`](DistributedLockBackend::acquire) hands the token back for a caller
//! that must renew from somewhere other than the acquiring task (a remote one, or
//! a *different cluster replica that never saw the acquire*), while
//! [`try_lock`](DistributedLockBackend::try_lock) takes the same lease and wraps
//! the token in a guard task because [`LockGuard`]'s fields are private and cannot
//! carry one.
//!
//! ## What was removed here, and what it cost
//!
//! An earlier revision carried a **liveness beacon**: one per-incarnation advisory
//! key held on a dedicated connection, stamped onto every row this instance wrote,
//! whose disappearance from `pg_locks` published "the process that took this lock
//! is gone". The acquire predicate joined against it, so a crashed holder's lock
//! became stealable before its TTL.
//!
//! It was sound precisely when the process holding the beacon was the process
//! using the lock. Brokered, that stops being true — cluster's beacon would vouch
//! for locks held by other, live consumers, so its restart would revoke the
//! fleet's locks — and the predicate therefore changes **for everyone**, because
//! keeping it for in-process acquisitions and dropping it for brokered ones would
//! mean the same code and the same config reclaim a dead holder's lock in
//! milliseconds in one deployment and at the TTL in another (§5.8.2, Goal 2).
//!
//! Three things went with it, and the cost of each is stated rather than buried:
//!
//! * **Sub-TTL reclaim of a crashed holder's lock.** Gone. The bound is now the
//!   lease TTL the holder chose, in every profile — the same bound every
//!   non-Postgres backend already had. Keep lock TTLs tight (ADR-012).
//! * **The shutdown drain.** Gone, and its removal is the point rather than a
//!   consequence: deleting this instance's rows on the way out *is* the revocation
//!   §5.8.2 exists to prevent. `stop()` now leaves held lease rows exactly where
//!   they are, so a restart costs subscribers a re-subscribe and nothing else.
//! * **The incarnation-keyed orphan sweep.** Gone with the key it filtered on. A
//!   row left by an acquisition cancelled after its INSERT committed is now
//!   reclaimed by the TTL sweep like any other lapsed lease, so that name is
//!   wedged until its TTL instead of until the next reaper wake. That is the same
//!   trade as the first bullet, applied to a local mishap rather than a remote
//!   crash.
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
//! exact about. Releasing a lock means **deleting its row**, and two paths do
//! exactly that: `release` (fenced on the token) and the TTL sweep.
//!
//! What no longer happens is the *advisory-lock* release: nothing is ever
//! `pg_advisory_lock`ed by this module at all, so there is no session-scoped
//! `pg_advisory_unlock` to pair with it, and — since the beacon went — no advisory
//! lock outside the pool either.
//!
//! The consequence is what matters: deleting a row is something **any** instance
//! can do, whereas an advisory unlock could only ever be issued by the session
//! that took it. A lapsed row is therefore stealable by the acquire predicate
//! itself, evaluated by whoever asks — no reclaim step, and no reason reclamation
//! has to route back to the instance that held the lock. That is what lets a
//! crashed *or merely wedged* holder's lock be taken by anyone, rather than only by
//! a healthy reaper on the owning instance, and it is the same property that lets
//! any cluster replica serve any lease operation.
//!
//! **No in-process registry survives.** The one that used to
//! (`local_holders`) existed solely to feed the incarnation-keyed orphan sweep, and
//! went with it. Nothing here remembers which locks this process holds, because
//! nothing needs to: every predicate is over stored state.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use cluster_sdk::observability::{self, result, spans};
use cluster_sdk::{
    ClusterError, ClusterMetrics, DistributedLockBackend, LeaseToken, LockFeatures, LockGuard,
    ProviderErrorKind,
};
use sqlx::AssertSqlSafe;
use sqlx::PgPool;
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;
use tracing::{Instrument, warn};
use uuid::Uuid;

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
use notify::ReleaseWaiters;

/// The fence a lease name starts at, matching the cache-backed default's
/// `FIRST_FENCE` (`cluster/src/defaults/lease.rs`). Non-zero so that zero stays
/// available as "no lease held", and the migration's
/// `cluster_lock_fence_positive_check` is the database-side backstop for it.
const FIRST_FENCE: i64 = 1;

/// In-flight command buffer for each [`LockGuard`], matching the cache-backed
/// default's `COMMAND_BUFFER`.
const GUARD_COMMAND_BUFFER: usize = 4;

/// Mints the `owner` an in-process acquisition claims its lease under.
///
/// A fresh id **per acquisition** rather than one per process, matching
/// `CasBasedDistributedLockBackend`: two guards held concurrently here are then
/// distinct owners, so neither can renew or release the other's lease, and a
/// re-entrant `try_lock` contends exactly as it always has. A *brokered*
/// acquisition supplies its own owner instead — the caller's `ClientId` (§5.4),
/// passed straight through by [`DistributedLockBackend::acquire`].
fn fresh_owner() -> String {
    Uuid::new_v4().to_string()
}

/// Converts a stored `fence` to the token's `u64`, rejecting a value the database
/// cannot have written.
///
/// `cluster_lock.fence` is a `BIGINT` constrained positive and only ever written as
/// `1` or `fence + 1`, so this cannot fail against a table this crate migrated. It
/// is a `Provider` error rather than a panic because the table is shared, mutable
/// state an operator can reach with `psql`.
fn fence_to_u64(fence: i64) -> Result<u64, ClusterError> {
    u64::try_from(fence).map_err(|_| ClusterError::Provider {
        kind: ProviderErrorKind::Other,
        message: format!(
            "cluster_lock.fence is {fence}, which is not a valid lease fence; the column is \
             constrained positive, so the row was written by something other than this plugin"
        ),
    })
}

/// Converts a token's `fence` to the `BIGINT` bound into a lease predicate.
///
/// A token whose fence exceeds `i64::MAX` can match no row this plugin wrote, so
/// the predicate it would build is unsatisfiable — and every caller of this treats
/// "matched nothing" as its own answer. Returning the saturated bound keeps that
/// answer a single round-trip instead of a special case.
fn fence_to_i64(fence: u64) -> i64 {
    i64::try_from(fence).unwrap_or(i64::MAX)
}

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
/// **One way to be free — the lease lapsed** (§5.8.2). One branch, one indexed
/// comparison, no `pg_locks` scan, no correlated subquery, and no columns beyond
/// the deadline itself.
///
/// This is what the beacon removal reduces to, and the reduction is the point
/// rather than a side effect (see the module doc). What it buys beyond uniformity
/// across profiles: the acquire path loses its only unindexed scan. `pg_locks` is a
/// function scan over `pg_lock_status()` with no index, so the old predicate made a
/// contended acquire `O(advisory locks on the server)` and needed a `CASE` — not an
/// `OR`, whose operand evaluation order SQL does not guarantee — purely to keep
/// that cost off the uncontended path. Neither the `CASE` nor the reasoning behind
/// it is needed now (§7.2.4).
///
/// Still a function rather than an inlined string, for the reason it always was:
/// `__test_explain_acquire` plans the statement acquisition actually runs, so the
/// "no `pg_locks` scan" claim above is checked against a real query plan rather
/// than asserted.
fn stealable_predicate(table: &str) -> String {
    format!("{table}.expires_at <= now()")
}

/// The native Postgres distributed-lock backend.
pub struct PostgresLock {
    pool: PgPool,
    /// The schema-qualified table name. See `PostgresCache::table`'s doc for
    /// the trust-boundary note — same reasoning applies here.
    table: String,
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
    /// How long a lapsed row is retained so its `fence` outlives the lease
    /// (`config::PostgresLockConfig::fence_retention_ms`,
    /// DESIGN.md). Read by the reaper's sweep predicate and
    /// by the [`should_hint`] gate, which is a statement about when a row becomes
    /// *reapable* rather than when its lease lapses.
    fence_retention: Duration,
}

/// Everything [`PostgresLock::new`] needs. A struct rather than six positional
/// parameters, and constructible from outside this module so `plugin.rs`'s
/// combined builder can call it too.
pub struct LockInit {
    /// The write pool, carrying every `cluster_lock` statement and `pg_notify`.
    pub pool: PgPool,
    /// Schema holding `cluster_lock`.
    pub schema: String,
    /// The lock TTL reaper's interval — see [`PostgresLock::reaper_interval`].
    /// Passed once here rather than to `spawn_reaper`, so the reaper and the
    /// `deadline_hint` gate cannot be given different values.
    pub reaper_interval: Duration,
    /// The fence-retention window — see [`PostgresLock::fence_retention`].
    /// Passed here for the same reason: the sweep predicate and the hint gate are
    /// two readings of one window.
    pub fence_retention: Duration,
    /// The ADR-004 metrics sink (DESIGN.md §8).
    pub metrics: Arc<dyn ClusterMetrics>,
    /// The bounded `provider` label.
    pub provider: &'static str,
    /// The shared shutdown token guard tasks and reapers observe (PGR-L2).
    pub guard_shutdown: CancellationToken,
}

impl PostgresLock {
    /// Builds the lock backend.
    ///
    /// Infallible and synchronous, which it was not while the beacon existed: that
    /// connected eagerly, so a bad DSN failed here rather than on the first
    /// `try_lock`. Nothing this backend owns is established up front any more —
    /// every lease operation is a single statement on the pool the caller already
    /// opened, and the pool's own `connect` is where an unreachable database is
    /// still caught.
    pub(crate) fn new(init: LockInit) -> Arc<Self> {
        Arc::new(Self {
            pool: init.pool,
            table: format!("{schema}.cluster_lock", schema = init.schema),
            release_waiters: ReleaseWaiters::new(),
            metrics: init.metrics,
            provider: init.provider,
            guard_shutdown: init.guard_shutdown,
            deadline_hint: Arc::new(Notify::new()),
            reaper_interval: init.reaper_interval,
            fence_retention: init.fence_retention,
        })
    }

    /// Bundles the per-guard fields into a [`GuardContext`] for `acquire_lease` /
    /// `run_guard_task` (keeps their signatures within a sane arg count).
    fn guard_context(&self) -> GuardContext {
        GuardContext {
            pool: self.pool.clone(),
            metrics: Arc::clone(&self.metrics),
            provider: self.provider,
            shutdown: self.guard_shutdown.clone(),
            deadline_hint: Arc::clone(&self.deadline_hint),
            reaper_interval: self.reaper_interval,
            fence_retention: self.fence_retention,
        }
    }

    /// The non-blocking acquisition both [`try_lock`](DistributedLockBackend::try_lock)
    /// and [`acquire`](DistributedLockBackend::acquire) run, with contention turned
    /// into [`ClusterError::LockContended`].
    ///
    /// Instrumented here rather than at each caller so the guard-returning and
    /// token-returning halves share one span and one metric series — they are the
    /// same operation against the same lease, and `op` is a bounded label
    /// (invariant I15). Mirrors `CasBasedDistributedLockBackend::acquire_lease`.
    async fn acquire_once(
        &self,
        name: &str,
        owner: &str,
        ttl: Duration,
        ctx: &GuardContext,
    ) -> Result<LeaseToken, ClusterError> {
        let span =
            tracing::info_span!(spans::LOCK_TRY_LOCK, provider = %self.provider, lock = %name);
        let started = std::time::Instant::now();
        let out = async {
            match acquire_lease(&self.table, name, owner, ttl, ctx).await? {
                Some(token) => Ok(token),
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

    /// The blocking acquisition both [`lock`](DistributedLockBackend::lock) and
    /// [`acquire_waiting`](DistributedLockBackend::acquire_waiting) run: retry until
    /// the lease lands or `timeout` elapses. See [`Self::wait_for_lease`] for the
    /// loop itself.
    async fn acquire_waiting_for(
        &self,
        name: &str,
        owner: &str,
        ttl: Duration,
        timeout: Duration,
        ctx: &GuardContext,
    ) -> Result<LeaseToken, ClusterError> {
        let span = tracing::info_span!(spans::LOCK_LOCK, provider = %self.provider, lock = %name);
        let op_started = std::time::Instant::now();
        let out = self
            .wait_for_lease(name, owner, ttl, timeout, ctx)
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

    /// The uninstrumented wait loop [`Self::acquire_waiting_for`] spans and measures
    /// (DESIGN.md §5.3).
    ///
    /// Adapted to sqlx's public API: rather than `LISTEN`ing on a connection this
    /// task owns (sqlx's `PgListener` owns its own single-connection pool, with no
    /// public way to hand it an already-checked-out `PoolConnection`), it waits on
    /// the in-process `release_waiters` registry the dedicated LISTEN task
    /// (`notify::spawn_release_listen_task`) feeds. A short heartbeat retry runs
    /// alongside the wake-up as a safety net against a missed notification (task not
    /// started yet, a dropped wake), so a lost wake only costs latency up to the
    /// heartbeat interval, never correctness — the loop always re-attempts the
    /// acquire statement itself ([`acquire_lease`], §5.1) as the source of truth.
    ///
    /// **The heartbeat is also what covers a lapsing lease**, and that matters more
    /// now than it did. A lease lapsing writes nothing, so no `NOTIFY` announces it:
    /// a waiter listening only to the release channel would sleep past a lease it
    /// could have taken. The cache-backed default has to cap each wait by the
    /// incumbent's observed deadline for exactly this reason
    /// (`cluster/src/defaults/lease.rs`); here the pre-existing heartbeat already
    /// bounds it, so the worst case is one `HEARTBEAT` of latency past the deadline
    /// rather than an indefinite sleep. Capping on the incumbent's `expires_at` would
    /// tighten that, and is available cheaply if the latency ever shows up — the
    /// acquire statement would have to return the conflicting row's deadline.
    async fn wait_for_lease(
        &self,
        name: &str,
        owner: &str,
        ttl: Duration,
        timeout: Duration,
        ctx: &GuardContext,
    ) -> Result<LeaseToken, ClusterError> {
        const HEARTBEAT: Duration = Duration::from_millis(250);

        let started = tokio::time::Instant::now();
        let deadline = started + timeout;

        let mut first_attempt = true;
        // The most recent transient outage, if the last attempt never reached
        // Postgres. It replaces `LockTimeout` at every exit below, so a caller that
        // waited out its whole budget against a *dead* backend is told that, rather
        // than being handed the same `LockTimeout` that ordinary contention produces
        // — the distinction `try_lock` already makes, and the one a caller needs to
        // decide between retrying and alerting.
        //
        // Deliberately not a shorter give-up budget: retrying for the full timeout is
        // what carries a `lock()` through a Postgres failover (commonly 10-30s), and
        // cutting that short to improve an error code would trade a real availability
        // property for a cosmetic one.
        let mut last_transient: Option<ClusterError> = None;
        loop {
            // Bound each *subsequent* acquire attempt by the remaining budget, not
            // just the gap between attempts (PGR-L3): the write can block on
            // `pool.acquire()` for up to `pool_acquire_timeout` (default 5s), so
            // checking `deadline` only after each full attempt let a single attempt
            // overshoot the caller's `timeout`.
            //
            // A cancelled attempt whose upsert had already committed leaves a lease
            // this process owns and no longer has a token for — reclaimed by the TTL
            // sweep like any other lapsed lease, so that name is taken until its TTL.
            // The incarnation-keyed orphan sweep used to reclaim it sooner; it went
            // with the beacon, and this is the residual cost (the module doc).
            // Indistinguishable, now, from a consumer dropping its `LockGuard`
            // without releasing, which resolves the same way and always has.
            //
            // The *first* attempt always runs (bounded only by the pool's own acquire
            // timeout), even when `timeout` is 0/expired — matching the CAS-based
            // default's attempt-before-budget-check ordering, so
            // `lock(free_lock, ttl, Duration::ZERO)` still acquires instead of
            // returning `LockTimeout` without ever trying.
            let now = tokio::time::Instant::now();
            let remaining = deadline.saturating_duration_since(now);
            if remaining.is_zero() && !first_attempt {
                return Err(timed_out(&mut last_transient, name, started));
            }

            let attempt = acquire_lease(&self.table, name, owner, ttl, ctx);
            let outcome = if remaining.is_zero() {
                // First attempt with no remaining budget: run it unbounded (the
                // pool acquire timeout bounds it in practice) rather than skip.
                attempt.await
            } else {
                match tokio::time::timeout(remaining, attempt).await {
                    Ok(result) => result,
                    Err(_elapsed) => {
                        return Err(timed_out(&mut last_transient, name, started));
                    }
                }
            };
            let acquired = match outcome {
                Ok(acquired) => {
                    // This attempt reached Postgres and got a real answer (acquired,
                    // or contended). Any earlier outage has been superseded, so a
                    // later timeout is genuine contention.
                    last_transient = None;
                    acquired
                }
                // A pool-side connection loss will plausibly clear inside a typical
                // `lock` budget (a failover, a restarted backend). A caller that
                // asked for 30s of patience should get it, so this is treated exactly
                // like contention: wait, then retry. Only the caller's own deadline
                // ends the loop. Retained so that, if the budget runs out while the
                // database is still unreachable, the caller gets this rather than a
                // `LockTimeout` that would read as ordinary contention.
                Err(err) if is_transient_session_loss(&err) => {
                    last_transient = Some(err);
                    None
                }
                Err(err) => return Err(err),
            };
            first_attempt = false;
            if let Some(token) = acquired {
                return Ok(token);
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

    /// Turns a freshly acquired `token` into the consumer-facing [`LockGuard`],
    /// unwinding the acquisition if `stop()` won the race (PGR-L2).
    ///
    /// The re-check closes the window the pre-acquire one in [`acquire_lease`] cannot:
    /// on a multi-worker runtime a concurrent `stop()` may cancel *between* that check
    /// and the upsert committing. Handing back a guard then would hand a consumer a
    /// lock this backend has already given up on, with no task left to serve its
    /// renew or release.
    ///
    /// One check rather than the two this used to need. The old pair straddled the
    /// `local_holders` registration, because the shutdown drain cleared that registry
    /// concurrently and an entry inserted after the clear would have advertised a lock
    /// nothing would hand back. There is no registry and no drain now, so the only
    /// question left is whether to keep the lease — and the answer is no, because no
    /// consumer received the guard.
    ///
    /// Discarding here is *not* in tension with §5.8.2's "shutdown revokes nothing":
    /// this lease is provably ours and provably unheld, so deleting it is this owner
    /// releasing its own claim. Contrast [`DistributedLockBackend::acquire`], which
    /// hands the token to a caller and therefore must leave the lease to lapse on its
    /// deadline instead (see [`discard_lease`]).
    async fn guard_from(
        &self,
        ctx: GuardContext,
        token: LeaseToken,
    ) -> Result<LockGuard, ClusterError> {
        if ctx.shutdown.is_cancelled() {
            discard_lease(&self.table, &token, &ctx.pool).await;
            return Err(ClusterError::Shutdown);
        }
        let (rx, guard) = LockGuard::channel(token.name.clone(), GUARD_COMMAND_BUFFER);
        tokio::spawn(run_guard_task(self.table.clone(), ctx, token, rx));
        Ok(guard)
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

    /// The handles the TTL reaper needs to sweep the table.
    fn reaper_context(&self) -> reaper::ReaperContext {
        reaper::ReaperContext {
            pool: self.pool.clone(),
            table: self.table.clone(),
            metrics: Arc::clone(&self.metrics),
            provider: self.provider,
            fence_retention: self.fence_retention,
        }
    }

    /// Test-only: `EXPLAIN (ANALYZE, VERBOSE)` of the **real** acquire statement,
    /// returned as plan text.
    ///
    /// Backs `PG-SPEC-012`, which now holds the acquire path to issuing **no
    /// `pg_locks` scan** — one of `L2`'s exit criteria, and a claim about a query
    /// plan rather than about source text, so it is checked against the plan.
    /// Before the beacon removal this seam existed to prove the `CASE` predicate
    /// short-circuited that scan off the uncontended path; there is now no scan to
    /// short-circuit, which is the stronger property and the easier assertion.
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
        let plan: Vec<String> = sqlx::query_scalar(AssertSqlSafe(format!(
            "EXPLAIN (ANALYZE, VERBOSE) {acquire}",
            acquire = acquire_sql(&self.table)
        )))
        .bind(name)
        .bind(fresh_owner())
        .bind(ttl_to_millis(ttl)?)
        .fetch_all(&self.pool)
        .await
        .map_err(map_sqlx_error)?;
        Ok(plan.join("\n"))
    }

    /// Test-only: reads the lease row's token halves and deadline, so a test can
    /// assert the fence actually increased across a steal, or that `stop()` left a
    /// held lease exactly where it was. `None` when no row exists.
    ///
    /// Gated behind `--features integration` (PGR-M8).
    #[cfg(feature = "integration")]
    #[doc(hidden)]
    pub async fn __test_lease_row(
        &self,
        name: &str,
    ) -> Result<Option<(String, i64)>, ClusterError> {
        sqlx::query_as(AssertSqlSafe(format!(
            "SELECT owner, fence FROM {table} WHERE name = $1",
            table = self.table
        )))
        .bind(name)
        .fetch_optional(&self.pool)
        .await
        .map_err(map_sqlx_error)
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
        Ok(
            reaper::probe_schedule(&self.pool, &self.table, self.fence_retention)
                .await?
                .seconds_until_expiry,
        )
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
    /// See [`PostgresLock::fence_retention`] - the other half of what
    /// [`should_hint`] compares against the interval.
    fence_retention: Duration,
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
///
/// # Why `retention` is in the comparison
///
/// The reaper no longer has anything to do at `expires_at`: a lapsed row is left
/// in place for the fence-retention window and only becomes work at
/// `expires_at + retention` (§5.8.1, `reaper::sweep_batch`). So the deadline the
/// hint is about is the *reapable* one, and hinting on the lease deadline would
/// wake the reaper for a row it must not touch yet — paying the full cost above
/// for a sweep guaranteed to delete nothing.
///
/// With a production window (an hour) against a production interval (five
/// seconds) this is always `false`, which is the honest outcome rather than a
/// missing feature: nothing can slip through a sleep capped at five seconds when
/// the earliest possible work is an hour out. The gate stays because a deployment
/// (or a test) with a small window puts it back in play, and because a rule that
/// silently stops applying is worse than one that evaluates to `false`.
fn should_hint(ttl: Duration, retention: Duration, interval: Duration) -> bool {
    ttl.saturating_add(retention) < interval
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

/// Whether `err` is an outage that will plausibly clear on its own, and so should
/// be retried inside `lock()`'s budget rather than ending it.
///
/// Now exactly one case: a pool-side `ConnectionLost`. Retrying it for the caller's
/// full timeout is what carries a `lock()` through a Postgres failover (commonly
/// 10-30s). Every other kind (`AuthFailure`, `Timeout`, `Other`, and the
/// non-provider variants) either will not clear by waiting or is already the
/// caller's answer.
///
/// It used to also cover the beacon's own "no live beacon to stamp a row with",
/// which was the common case rather than the rare one — the beacon reconnected with
/// a 200ms..5s backoff and every acquisition failed fast in the meantime. With the
/// beacon gone, an acquisition can no longer fail for a reason local to this
/// process at all.
fn is_transient_session_loss(err: &ClusterError) -> bool {
    matches!(
        err,
        ClusterError::Provider {
            kind: ProviderErrorKind::ConnectionLost,
            ..
        }
    )
}

/// Deletes the lease row an abandoned in-process acquisition wrote, so a bail-out
/// leaves no lease nobody holds.
///
/// Fenced on the token, which makes it safe to call unconditionally: if the
/// name has already been re-acquired — here or on another instance — the
/// successor's `ON CONFLICT DO UPDATE` moved `owner`/`fence` on, so this matches
/// zero rows rather than deleting a live holder's lease.
///
/// **Only the guard-returning half calls this.** A bail-out there means no consumer
/// ever received the `LockGuard`, so the lease is provably ours and provably unheld:
/// deleting it is this owner releasing its own lease, not a revocation. The
/// token-returning half deliberately does *not* discard — a token that reached its
/// caller names a lease that caller now owns, and one whose *response* was lost
/// still names a lease that must lapse on its deadline rather than be yanked
/// (invariant I7, and why no acquisition method is `#[retryable]`).
///
/// Silent and unNOTIFYed, deliberately: the caller is already returning an error, a
/// row left behind is reclaimed by the TTL sweep, and a waiter on this name falls
/// back to its heartbeat rather than costing a round-trip on a rare race path.
async fn discard_lease(table: &str, token: &LeaseToken, pool: &PgPool) {
    let _deleted = sqlx::query(AssertSqlSafe(format!(
        "DELETE FROM {table} WHERE name = $1 AND owner = $2 AND fence = $3"
    )))
    .bind(&token.name)
    .bind(&token.owner)
    .bind(fence_to_i64(token.fence))
    .execute(pool)
    .await;
}

/// The acquire statement — insert-or-steal-if-lapsed, in one round trip
/// (DESIGN.md §5.1). One function so the
/// `EXPLAIN ANALYZE` seam (`PG-SPEC-012`) plans the statement acquisition actually
/// runs, rather than a copy of it that could drift.
///
/// **`fence` is read off the row's own current value, not bound by the acquirer**:
/// `fence = {table}.fence + 1` in the `DO UPDATE`, so a steal strictly increases the
/// counter within the one statement that performs it. Postgres evaluates that
/// against the latest committed version of the conflicting tuple (the module doc's
/// step 3), so two racing stealers cannot land on the same fence — the loser blocks
/// on the winner's row lock, then re-evaluates `WHERE {stealable}` against the
/// winner's committed row and matches nothing. This is what the cache-backed default
/// achieves with a value-guarded conditional write — a `compare_and_swap` on the
/// exact bytes that were read, since `CacheEntry::version` resets on a
/// delete-and-recreate (`cluster/src/defaults/lease.rs`); here the upsert is the CAS.
///
/// `RETURNING fence` is what makes the token mintable from the same statement: the
/// INSERT path returns [`FIRST_FENCE`], the steal path returns the incremented
/// value, and zero rows means contended.
///
/// Binds: `$1` name, `$2` owner, `$3` ttl ms.
fn acquire_sql(table: &str) -> String {
    format!(
        "INSERT INTO {table} (name, owner, fence, acquired_at, expires_at) \
         VALUES ($1, $2, {FIRST_FENCE}, now(), {expires_at}) \
         ON CONFLICT (name) DO UPDATE SET owner = EXCLUDED.owner, \
         fence = {table}.fence + 1, \
         acquired_at = EXCLUDED.acquired_at, expires_at = EXCLUDED.expires_at \
         WHERE {stealable} \
         RETURNING fence",
        expires_at = expires_at_sql(3),
        stealable = stealable_predicate(table),
    )
}

/// Attempts the acquisition every entry point shares — `try_lock`, `lock`'s retry
/// loop, `acquire` and `acquire_waiting`: one conditional upsert against
/// `cluster_lock`, minting the [`LeaseToken`] from the fence the statement returns.
///
/// `Ok(None)` is contention — the row exists and its lease has not lapsed — which is
/// the same answer whether the rival is another instance, another cluster replica,
/// or another task in this process (the module doc). It is also the answer when the
/// live lease is `owner`'s own, which makes a re-entrant acquisition contend
/// exactly as it always has.
///
/// Spawns nothing and registers nothing. Wrapping the token in a guard task is the
/// guard-returning half's business ([`PostgresLock::spawn_guard`]), which
/// lets both halves of the trait serve one lease.
async fn acquire_lease(
    table: &str,
    name: &str,
    owner: &str,
    ttl: Duration,
    ctx: &GuardContext,
) -> Result<Option<LeaseToken>, ClusterError> {
    // Reject un-notifiable names before mutating any lease state, so `release`
    // never reaches a lock it cannot cleanly signal (see `validate_lock_name`).
    validate_lock_name(name)?;
    let ttl_ms = ttl_to_millis(ttl)?;

    // Shutdown, checked *before* any lease work (PGR-L2). The re-check in the
    // guard-returning half exists to unwind an acquisition that raced `stop()`;
    // this one keeps the common case from starting one at all — no row to delete,
    // nothing to undo.
    //
    // It is also what makes the answer honest. Without it an acquisition arriving
    // after `stop()` reaches the pool, fails with `ConnectionLost`, and — since
    // `lock()` retries that as transient — spends the caller's entire timeout
    // before reporting `LockTimeout` for a backend that is simply gone.
    if ctx.shutdown.is_cancelled() {
        return Err(ClusterError::Shutdown);
    }

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
    // `RETURNING fence` is the answer: a row means acquired (by insert or by
    // steal), zero rows means contended. The loser blocks on the winner's row lock
    // and then re-evaluates the predicate against the winner's *committed* state,
    // which is why there is no third case — and why `READ COMMITTED` is asserted
    // at startup (`pg_setup::assert_read_committed`).
    let fence: Option<i64> = sqlx::query_scalar(AssertSqlSafe(acquire_sql(table)))
        .bind(name)
        .bind(owner)
        .bind(ttl_ms)
        .fetch_optional(&ctx.pool)
        .await
        .map_err(map_sqlx_error)?;

    let Some(fence) = fence else {
        return Ok(None);
    };

    // Wake the TTL reaper so this acquisition's deadline is visible to its next
    // sleep instead of only from its next interval wake (`deadline_hint`), and only
    // when the deadline could actually be missed — see [`should_hint`] for why an
    // unconditional signal pins the reaper at its wake floor forever.
    if should_hint(ttl, ctx.fence_retention, ctx.reaper_interval) {
        ctx.deadline_hint.notify_one();
    }

    Ok(Some(LeaseToken::new(name, owner, fence_to_u64(fence)?)))
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
    table: String,
    ctx: GuardContext,
    token: LeaseToken,
    mut commands: cluster_sdk::LockCommandReceiver,
) {
    loop {
        tokio::select! {
            // Graceful shutdown: exit promptly rather than waiting for the
            // consumer to act (PGR-L2). The lease row is left exactly where it is
            // — there is no drain any more, and that is deliberate (§5.8.2): the
            // lease outlives this process and lapses on its own deadline, so a
            // restart under a held lock revokes nothing. Same outcome as a consumer
            // dropping its guard without releasing.
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
                        let result =
                            instrumented_renew(&table, &ctx, &token, new_ttl).await;
                        responder.respond(result);
                    }
                    cluster_sdk::LockRequest::Release { responder } => {
                        let result = instrumented_release(&table, &ctx, &token).await;
                        responder.respond(result);
                        return;
                    }
                }
            }
        }
    }
}

/// [`renew_lease`] with its span and its ADR-004 signals, shared by the guard task
/// and by [`DistributedLockBackend::renew`].
///
/// One wrapper rather than one per caller, so the two halves of the trait emit a
/// single `renew` metric series over a single lease — they are the same operation,
/// and `op` is a bounded label (invariant I15).
async fn instrumented_renew(
    table: &str,
    ctx: &GuardContext,
    token: &LeaseToken,
    new_ttl: Duration,
) -> Result<(), ClusterError> {
    let span = tracing::info_span!(
        spans::LOCK_RENEW, provider = %ctx.provider, lock = %token.name
    );
    let started = std::time::Instant::now();
    let result = renew_lease(table, ctx, token, new_ttl)
        .instrument(span)
        .await;
    record_lock(
        &*ctx.metrics,
        ctx.provider,
        "renew",
        &token.name,
        started,
        &result,
    );
    result
}

/// [`release_lease`] with its span and its ADR-004 signals — see
/// [`instrumented_renew`].
async fn instrumented_release(
    table: &str,
    ctx: &GuardContext,
    token: &LeaseToken,
) -> Result<(), ClusterError> {
    let span = tracing::info_span!(
        spans::LOCK_RELEASE, provider = %ctx.provider, lock = %token.name
    );
    let started = std::time::Instant::now();
    let result = release_lease(table, ctx, token).instrument(span).await;
    record_lock(
        &*ctx.metrics,
        ctx.provider,
        "release",
        &token.name,
        started,
        &result,
    );
    result
}

/// `Renew`: resets `expires_at` to `new_ttl` from now (and restamps `acquired_at`)
/// without relinquishing the lease.
///
/// One conditional write predicated on `(name, owner, fence, expires_at > now())`
/// and on **nothing any process remembers**, which is the property that matters
/// (§5.8.1, invariant I7): the answer is entirely a function of the row, so every
/// replica gives the same one and a lease acquired through one can be renewed
/// through another that never saw the acquire.
///
/// The `WHERE` carries two fences and one liveness test:
///
/// * **`owner`** keeps one holder from renewing another's lease.
/// * **`fence`** guards against a **successor** — if our lease lapsed and a newer
///   holder stole the name, it did so at `fence + 1`, so resetting the deadline
///   here would otherwise extend *its* lease under our token.
/// * **`expires_at > now()`** refuses to resurrect a lease that has already lapsed.
///   The row may still be sitting there unswept, but the fleet is entitled to treat
///   it as free, so renewing it would re-take a lock we might simultaneously be
///   losing.
///
/// The third beacon fence that used to be here — "has *this instance's* beacon been
/// replaced since the acquisition?" — is gone with the beacon, and its absence is
/// the change: a renew now fails only because the *lease* moved on, never because
/// the process that took it had a bad moment. Nothing local can invalidate a lease.
///
/// Zero rows updated therefore means `LockExpired`, whichever fence failed. Lapsed,
/// stolen and never-yours are indistinguishable and all three mean the same thing
/// (§6.9): the caller no longer holds this lease and must abort its critical section
/// rather than retry.
///
/// Signals `deadline_hint` on success. A renew usually pushes the deadline
/// *later*, which only ever makes the reaper's current sleep conservative — but
/// `renew` takes an arbitrary `new_ttl`, so a short one can move the earliest
/// deadline *earlier* than the sleep already in flight was told about.
async fn renew_lease(
    table: &str,
    ctx: &GuardContext,
    token: &LeaseToken,
    new_ttl: Duration,
) -> Result<(), ClusterError> {
    let ttl_ms = ttl_to_millis(new_ttl)?;
    let updated: Option<i32> = sqlx::query_scalar(AssertSqlSafe(format!(
        "UPDATE {table} SET acquired_at = now(), expires_at = {expires_at} \
         WHERE name = $2 AND owner = $3 AND fence = $4 AND expires_at > now() \
         RETURNING 1",
        expires_at = expires_at_sql(1),
    )))
    .bind(ttl_ms)
    .bind(&token.name)
    .bind(&token.owner)
    .bind(fence_to_i64(token.fence))
    .fetch_optional(&ctx.pool)
    .await
    .map_err(map_sqlx_error)?;

    if updated.is_none() {
        return Err(ClusterError::LockExpired {
            name: token.name.clone(),
        });
    }

    // Same gate as `try_acquire`'s — see [`should_hint`]. This call site is the
    // one that made an unconditional signal pathological rather than merely
    // wasteful: a renewal is a *heartbeat*, so it repeats for the whole life of
    // every held lock, and a fleet steadily renewing its leases kept a permit
    // permanently pending and the reaper permanently at its floor.
    if should_hint(new_ttl, ctx.fence_retention, ctx.reaper_interval) {
        ctx.deadline_hint.notify_one();
    }
    Ok(())
}

/// `Release`: deletes the lease row and wakes any blocked `lock()` waiters.
///
/// Fenced on `(name, owner, fence)` in the statement itself, which is the whole
/// fence needed: a lease stolen after this one lapsed carries the *successor's*
/// owner and a higher fence and will not match, so a stale token cannot delete a
/// live holder's lease.
///
/// **Liveness is deliberately not in the predicate**, matching the cache-backed
/// default: a lapsed row still bearing this token is still this holder's, and
/// removing it frees the name immediately instead of making the next acquirer steal
/// it.
///
/// **Absence is `Ok`** (idempotent by absence, §6.10). A retried release, a release
/// bearing a fenced-out token, and a release of a lease the TTL sweep already
/// reclaimed all delete nothing and all succeed — never `LockExpired`, never a
/// not-found. That is the trait's contract and it is why nothing is checked before
/// the statement runs: the predicate *is* the check, so there is no local state
/// whose absence could turn a legitimate release into a silent no-op.
///
/// This is also the operation `L3` will have to revisit. §5.8.1 asks that a fence
/// value never be reused within `fence_retention` of its lease **ending**, but
/// specifies `release` as a `DELETE` — which drops the fence immediately. See
/// ADR-012: the two sentences are settled there in favour of the delete, and the
/// guarantee narrowed to lapsing.
async fn release_lease(
    table: &str,
    ctx: &GuardContext,
    token: &LeaseToken,
) -> Result<(), ClusterError> {
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
    // result). A stale token whose lease a successor already stole therefore stops
    // announcing a release that did not happen — the channel is shared, so every
    // blocked `lock()` waiter on it wakes and retries for nothing, and §11 names
    // aggregate NOTIFY rate as the primary scaling risk.
    //
    // It is also what keeps "absence is `Ok`" from being noisy: a no-op release
    // returns success and sends nothing.
    sqlx::query(AssertSqlSafe(format!(
        "WITH released AS (DELETE FROM {table} \
          WHERE name = $1 AND owner = $2 AND fence = $3 RETURNING 1) \
         SELECT pg_notify($4, $1) FROM released"
    )))
    .bind(&token.name)
    .bind(&token.owner)
    .bind(fence_to_i64(token.fence))
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
        let ctx = self.guard_context();
        let token = self.acquire_once(name, &fresh_owner(), ttl, &ctx).await?;
        self.guard_from(ctx, token).await
    }

    async fn acquire(
        &self,
        name: &str,
        owner: &str,
        ttl: Duration,
    ) -> Result<LeaseToken, ClusterError> {
        // The same lease `try_lock` takes, minus the guard task: this caller holds
        // the token itself and renews against it from wherever it likes. No discard
        // on bail-out here — see `discard_lease`.
        self.acquire_once(name, owner, ttl, &self.guard_context())
            .await
    }

    async fn acquire_waiting(
        &self,
        name: &str,
        owner: &str,
        ttl: Duration,
        timeout: Duration,
    ) -> Result<LeaseToken, ClusterError> {
        self.acquire_waiting_for(name, owner, ttl, timeout, &self.guard_context())
            .await
    }

    async fn renew(&self, token: &LeaseToken, ttl: Duration) -> Result<(), ClusterError> {
        // Predicated entirely on the row, so this succeeds against a lease acquired
        // through a *different* `PostgresLock` handle — a different process, a
        // different cluster replica — which is the whole point (invariant I7).
        //
        // Cross-checking that the transport caller is `token.owner` is the serving
        // gear's authorization decision (§4.6), not this predicate's.
        instrumented_renew(&self.table, &self.guard_context(), token, ttl).await
    }

    async fn release(&self, token: &LeaseToken) -> Result<(), ClusterError> {
        instrumented_release(&self.table, &self.guard_context(), token).await
    }

    async fn lock(
        &self,
        name: &str,
        ttl: Duration,
        timeout: Duration,
    ) -> Result<LockGuard, ClusterError> {
        let ctx = self.guard_context();
        let token = self
            .acquire_waiting_for(name, &fresh_owner(), ttl, timeout, &ctx)
            .await?;
        self.guard_from(ctx, token).await
    }

    /// `SELECT 1` against this lock's **own** pool
    /// (DESIGN.md).
    ///
    /// The pool matters more than the statement here. A `lock: { provider:
    /// postgres }` binding is always standalone and never shares a co-located
    /// cache pool (DESIGN.md §3.5), so this is the only probe that can observe
    /// the lock database at all: a profile pairing a healthy standalone cache
    /// with an unreachable lock DSN reports `Serving` unless this method answers
    /// for it. Same contract as
    /// [`PostgresCache::probe`](crate::cache::PostgresCache::probe) — no cluster
    /// table, reachability only.
    async fn probe(&self) -> Result<(), ClusterError> {
        sqlx::query_scalar::<_, i32>("SELECT 1")
            .fetch_one(&self.pool)
            .await
            .map_err(map_sqlx_error)?;
        Ok(())
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
        let lock = PostgresLock::new(LockInit {
            pool,
            schema: config.schema.clone(),
            reaper_interval: config.lock_reaper_interval(),
            fence_retention: config.fence_retention(),
            metrics: Arc::clone(&metrics),
            provider: crate::provider::PROVIDER_NAME,
            guard_shutdown: shutdown.clone(),
        });

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

    /// Test-only access to the concrete [`PostgresLock`], for the sweep and lease-row
    /// seams (the `dyn` [`lock`](Self::lock) has none of them).
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
    /// The pool is where that assertion belongs: every statement the lock issues is
    /// a pooled one. There is no long-lived off-pool connection left at all now that
    /// the beacon is gone.
    ///
    /// Gated behind `--features integration` (PGR-M8).
    #[cfg(feature = "integration")]
    #[doc(hidden)]
    #[must_use]
    pub fn __test_pool(&self) -> PgPool {
        self.lock.pool()
    }

    /// Cancels the lock TTL reaper and release-wake listener, then closes the pool
    /// (DESIGN.md §10).
    ///
    /// **Held lease rows are deliberately left in place**, and that is the whole of
    /// §5.8.2: a cluster restart is not a lease event. This used to drain them — one
    /// `DELETE` keyed on the outgoing incarnation's beacon, announced on
    /// `cluster_lock_released` — which was a clean handover while the process holding
    /// a lock was the process using it, and is a fleet-wide revocation the moment
    /// locks are brokered. Every remaining lease now lapses on its own deadline,
    /// renewed in the meantime by whichever holder still owns it, through whichever
    /// replica answers next (invariant I7).
    ///
    /// The cost, stated: a name this instance held is taken until its TTL rather than
    /// released the moment we let go, so a waiter elsewhere waits out the deadline
    /// instead of being woken. That is the same bound a crashed holder now has, and
    /// the same one every non-Postgres backend always had.
    pub async fn stop(mut self) {
        self.shutdown.cancel();
        for task in [self.reaper.take(), self.release_listener.take()]
            .into_iter()
            .flatten()
        {
            let _exited = task.await;
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
