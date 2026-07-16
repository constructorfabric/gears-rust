//! `PostgresLock` — the native `DistributedLockBackend` implementation over
//! `pg_advisory_lock` (DESIGN.md §5), plus the standalone `PostgresLockPlugin`
//! builder/handle (DESIGN.md §3.5) that lets an operator route `lock` to
//! Postgres independently of `cache`.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use cluster_sdk::observability::{self, result, spans};
use cluster_sdk::{ClusterError, ClusterMetrics, DistributedLockBackend, LockFeatures, LockGuard};
use dashmap::DashMap;
use sqlx::PgPool;
use sqlx::pool::PoolConnection;
use tokio_util::sync::CancellationToken;
use tracing::{Instrument, warn};
use uuid::Uuid;

pub mod notify;
pub mod reaper;

use crate::config::PostgresLockConfig;
use crate::pg_error::map_sqlx_error;
use crate::pg_setup::{
    base_pool_options, ensure_schema, lock_migrator, reject_pgbouncer_transaction_mode,
    run_migrator, warn_if_async_replication,
};
use notify::ReleaseWaiters;

/// Hashes a lock `name` to the two `i32` halves of a 64-bit value, for the
/// two-argument `pg_try_advisory_lock(key1, key2)` form (DESIGN.md §5.1).
///
/// Deterministic and stable across calls/processes: uses `xxh3_64` (already a
/// workspace dependency) over the name's UTF-8 bytes — the specific algorithm
/// is an implementation detail (DESIGN.md §5.1) as long as it stays fixed once
/// shipped, since a change would silently stop recognizing locks held under
/// the old mapping. `key1` is the high 32 bits, `key2` the low 32 bits, using
/// the full 64-bit hash space rather than `hashtext()`'s 32 bits (§5.1).
// Deliberately truncating: splitting a 64-bit hash into its high/low 32-bit
// halves, each reinterpreted (not clamped) as `i32` for
// `pg_try_advisory_lock`'s two-argument form (DESIGN.md §5.1) — not a
// narrowing bug, so the `as` casts below are exactly what's wanted.
#[must_use]
#[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
pub fn lock_key(name: &str) -> (i32, i32) {
    let hash = xxhash_rust::xxh3::xxh3_64(name.as_bytes());
    let key1 = (hash >> 32) as u32 as i32;
    let key2 = hash as u32 as i32;
    (key1, key2)
}

/// Maximum lock-name length, in UTF-8 bytes, this backend accepts.
///
/// A release notifies waiters via `NOTIFY cluster_lock_released, '<name>'`
/// (`notify::notify_released`), carrying the bare lock name as the payload.
/// `PostgreSQL` rejects NOTIFY payloads of 8000 bytes or more, so a name that
/// cannot fit could never be cleanly released — `release` would unlock and
/// delete the row and only *then* fail on the notify, returning an error for a
/// lock that is already gone. Reject such names at acquisition instead, before
/// any lock state is mutated, so `release` never sees an un-notifiable name.
const MAX_LOCK_NAME_BYTES: usize = 7999;

/// Rejects a lock `name` that cannot fit `PostgreSQL`'s NOTIFY payload limit, so
/// the acquisition never enters a state its release cannot cleanly signal (see
/// [`MAX_LOCK_NAME_BYTES`]). Returns [`ClusterError::InvalidName`] without
/// touching any lock state.
fn validate_lock_name(name: &str) -> Result<(), ClusterError> {
    if name.len() > MAX_LOCK_NAME_BYTES {
        return Err(ClusterError::InvalidName {
            name: name.to_owned(),
            reason: "lock name must be at most 7999 UTF-8 bytes (Postgres NOTIFY payload limit)",
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

/// Converts a lock TTL to the `ttl_ms` column value (`BIGINT`).
fn duration_to_ttl_ms(ttl: Duration) -> Result<i64, ClusterError> {
    i64::try_from(ttl.as_millis()).map_err(|_| ClusterError::InvalidConfig {
        reason: format!("ttl {ttl:?} exceeds the storable millisecond range"),
    })
}

/// A lock this process currently believes it holds: the pinned connection
/// (checked out of the pool for the lock's full duration, per DESIGN.md §3.3
/// — advisory locks are session-scoped, so only the connection that acquired
/// one can release it) plus the advisory-lock key pair needed to unlock it.
///
/// The connection lives behind an async [`tokio::sync::Mutex`] so no accessor
/// ever holds the `DashMap` shard lock across an `.await`. Every operation that
/// touches the connection (`renew`, `release`, the guard task's
/// `synchronous_commit` re-assertion, and the reaper's `reclaim`) clones this
/// `Arc` out of the map under a brief synchronous `get`/`remove`, then awaits on
/// the async mutex — so a concurrent reaper `remove` on the same key/shard can
/// never block a shard lock held across an `.await`, which would deadlock the
/// single worker under a current-thread runtime (`GAP-SOLUTIONS.md` §5). The
/// mutex serializes the accessors with a plain `.await`, not a thread-blocking
/// wait.
struct HeldLock {
    conn: Arc<tokio::sync::Mutex<PoolConnection<sqlx::Postgres>>>,
    key1: i32,
    key2: i32,
    /// The random UUID stamped on this acquisition's `cluster_lock` row and on
    /// the guard task that owns it. Used as an end-to-end ownership fence: after
    /// a TTL lapse frees this name and a *newer* holder re-acquires it, the
    /// original holder's stale guard (or a foreign reaper) must not renew,
    /// release, or reclaim the successor's live lock. Every `renew`/`release`/
    /// reaper path therefore matches this id before touching the entry, so a
    /// stale actor's operation is a no-op instead of silently unlocking the
    /// live holder and breaking mutual exclusion (PGR-L1 ownership fence).
    holder_id: String,
}

/// The native Postgres distributed-lock backend.
pub struct PostgresLock {
    pool: PgPool,
    /// The schema-qualified table name. See `PostgresCache::table`'s doc for
    /// the trust-boundary note — same reasoning applies here.
    table: String,
    /// Every lock this process currently holds, keyed by name. Both the
    /// per-guard command task (on `Release`) and the TTL reaper race to
    /// `remove` a given entry — `DashMap::remove` is atomic per key, so
    /// exactly one of them ever gets `Some` for a given lock, which is the
    /// whole basis for safely handing the pinned connection between them
    /// (see `reaper.rs`'s module doc for why the reaper needs this at all).
    held: Arc<DashMap<String, HeldLock>>,
    /// In-process wake-up registry for blocked `lock()` callers, fed by the
    /// `cluster_lock_released` LISTEN task (`notify::spawn_release_listen_task`,
    /// started by the plugin's `build_and_start` — DESIGN.md §5.3).
    release_waiters: Arc<ReleaseWaiters>,
    /// Cadence at which each guard task re-asserts `synchronous_commit = on` on
    /// its own pinned connection (DESIGN.md §3.4 residual gap). Bounds the GUC
    /// re-assertion window to this interval; shares the value with the TTL
    /// reaper (`lock_reaper_interval_ms`, default 5s).
    reassert_interval: Duration,
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
}

impl PostgresLock {
    #[must_use]
    pub fn new(
        pool: PgPool,
        schema: &str,
        reassert_interval: Duration,
        metrics: Arc<dyn ClusterMetrics>,
        provider: &'static str,
        guard_shutdown: CancellationToken,
    ) -> Arc<Self> {
        Arc::new(Self {
            pool,
            table: format!("{schema}.cluster_lock"),
            held: Arc::new(DashMap::new()),
            release_waiters: ReleaseWaiters::new(),
            reassert_interval,
            metrics,
            provider,
            guard_shutdown,
        })
    }

    /// Bundles the per-guard fields into a [`GuardContext`] for `try_acquire` /
    /// `run_guard_task` (keeps their signatures within a sane arg count).
    fn guard_context(&self) -> GuardContext {
        GuardContext {
            reassert_interval: self.reassert_interval,
            metrics: Arc::clone(&self.metrics),
            provider: self.provider,
            shutdown: self.guard_shutdown.clone(),
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

    /// Drains every lock this process currently holds: unlocks each advisory
    /// lock on its own pinned connection, wakes any blocked `lock()` waiters,
    /// and drops the connection back to the pool (DESIGN.md §10 step 4).
    ///
    /// Called by both `stop()` paths *before* `pool.close()`. It is required,
    /// not optional: `sqlx::Pool::close()` waits for every checked-out
    /// connection to be returned, and a held lock's connection is pinned in
    /// `held` (checked out for the lock's full duration, §3.3) entirely outside
    /// the pool's tracking — so without this drain, `close()` blocks forever on
    /// a lock still held at shutdown (`PG-LIFE-003`; the same block was the
    /// unresolved `PG-LOCK-007`/`PG-SPEC-004` hang).
    ///
    /// Unconditional, unlike the TTL reaper's sweep (which only reclaims
    /// expired rows) — but it reuses the reaper's exact per-lock `reclaim`
    /// path. Collecting names first and then `held.remove` per name keeps the
    /// same `DashMap::remove` atomic hand-off the guard task's `release()` and
    /// the reaper's `sweep` already rely on: if a guard's `release()` wins the
    /// race for a given name, `remove` here returns `None` and there is simply
    /// nothing left to reclaim.
    pub async fn drain_held(&self) {
        // Loop until `held` is observed empty rather than draining a single
        // snapshot (PGR-L2): a `try_acquire` that passed its shutdown check just
        // before `stop()` cancelled can still register a lock after an earlier
        // pass collected its names, so one pass cannot guarantee `pool.close()`
        // won't then block on that stranded pinned connection. New acquisitions
        // are rejected once `guard_shutdown` is cancelled (which `stop()` does
        // before calling this), so the loop converges.
        loop {
            let names: Vec<String> = self.held.iter().map(|entry| entry.key().clone()).collect();
            if names.is_empty() {
                break;
            }
            for name in names {
                if let Some((_, held_lock)) = self.held.remove(&name) {
                    reaper::reclaim(&name, held_lock, &*self.metrics, self.provider).await;
                }
            }
        }
    }

    /// Spawns the TTL reaper for this lock's `held` registry.
    ///
    /// A method on `PostgresLock` itself (delegating to `reaper::spawn_lock_reaper`),
    /// not a free function callers invoke with the pieces of a `PostgresLock`
    /// spread out — `HeldLock` is private to this module and its descendants
    /// (`reaper`), so nothing outside `lock/` could even name the type a
    /// free-function signature would need to expose.
    #[must_use]
    pub(crate) fn spawn_reaper(
        self: &Arc<Self>,
        interval: Duration,
        metrics: reaper::LockReaperMetrics,
        warn_threshold: i64,
        cancel: CancellationToken,
    ) -> tokio::task::JoinHandle<()> {
        reaper::spawn_lock_reaper(
            self.pool.clone(),
            self.table.clone(),
            Arc::clone(&self.held),
            interval,
            metrics,
            warn_threshold,
            cancel,
        )
    }

    /// Test-only: reads `SHOW synchronous_commit` on the named held lock's own
    /// pinned connection, returning `None` if this process doesn't currently
    /// hold `name`. There is no production API that hands out a raw pinned
    /// `PoolConnection`, so `pg_spec_005` reaches the GUC through this seam.
    ///
    /// Gated behind `--features integration` (the `tests/` suite's feature) so
    /// this seam is compiled out of release builds entirely (PGR-M8).
    #[cfg(feature = "integration")]
    #[doc(hidden)]
    pub async fn __test_synchronous_commit(&self, name: &str) -> Option<String> {
        let conn = Arc::clone(&self.held.get(name)?.conn);
        let mut conn = conn.lock().await;
        sqlx::query_scalar("SHOW synchronous_commit")
            .fetch_one(&mut **conn)
            .await
            .ok()
    }

    /// Test-only: flips `synchronous_commit` to `off` on the named held lock's
    /// pinned connection, simulating an external mid-session GUC mutation
    /// (DESIGN.md §3.4).
    ///
    /// Gated behind `--features integration` (PGR-M8): this in particular would
    /// otherwise let any release-build consumer flip `synchronous_commit=off` on
    /// a live lock connection — the exact durability footgun the plugin exists
    /// to prevent.
    #[cfg(feature = "integration")]
    #[doc(hidden)]
    pub async fn __test_flip_synchronous_commit_off(&self, name: &str) {
        let Some(conn) = self.held.get(name).map(|entry| Arc::clone(&entry.conn)) else {
            return;
        };
        let mut conn = conn.lock().await;
        let _flipped = sqlx::query("SET synchronous_commit = off")
            .execute(&mut **conn)
            .await;
    }

    /// Test-only: runs one re-assertion pass — exactly the code the guard task's
    /// interval arm runs. `pg_spec_005` drives it directly (with a long reaper
    /// interval so the guard's own timer never fires during the test) so the
    /// correction is deterministic and never races the guard task's own
    /// `held.get_mut`-across-`.await` (a DashMap current-thread deadlock hazard,
    /// `GAP-SOLUTIONS.md` §5).
    ///
    /// Gated behind `--features integration` (PGR-M8).
    #[cfg(feature = "integration")]
    #[doc(hidden)]
    pub async fn __test_reassert_synchronous_commit(&self, name: &str) {
        // Resolve the current holder for `name` and pass it through the same
        // `holder_id` fence the guard task uses (PGR-L1), so the seam exercises
        // the matching path rather than bypassing the fence.
        let Some(holder_id) = self.held.get(name).map(|entry| entry.holder_id.clone()) else {
            return;
        };
        let _reasserted = reassert_synchronous_commit(&self.held, name, &holder_id).await;
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
/// everything a held lock needs beyond its own name/pool/table. Grouped into
/// one value (rather than threaded as separate parameters) to keep
/// `try_acquire`/`run_guard_task` within a sane argument count. Cheap to clone
/// (an `Arc`, a `&'static str`, a `Duration`, and a `CancellationToken` clone).
#[derive(Clone)]
struct GuardContext {
    /// See [`PostgresLock::reassert_interval`].
    reassert_interval: Duration,
    /// The ADR-004 metrics sink (DESIGN.md §8).
    metrics: Arc<dyn ClusterMetrics>,
    /// The bounded `provider` label.
    provider: &'static str,
    /// The shutdown token guard tasks observe (PGR-L2).
    shutdown: CancellationToken,
}

/// Attempts the non-blocking acquisition shared by `try_lock` and `lock`'s
/// retry loop: `pg_try_advisory_lock`, then (on success) the metadata insert,
/// registering the pinned connection in `held` and spawning the guard task.
async fn try_acquire(
    pool: &PgPool,
    table: &str,
    held: &Arc<DashMap<String, HeldLock>>,
    name: &str,
    ttl: Duration,
    ctx: &GuardContext,
) -> Result<Option<LockGuard>, ClusterError> {
    // Reject un-notifiable names before mutating any lock state, so `release`
    // never reaches a lock it cannot cleanly signal (see `validate_lock_name`).
    validate_lock_name(name)?;
    let (key1, key2) = lock_key(name);
    let ttl_ms = duration_to_ttl_ms(ttl)?;

    let mut conn = pool.acquire().await.map_err(map_sqlx_error)?;
    let acquired: bool = sqlx::query_scalar("SELECT pg_try_advisory_lock($1, $2)")
        .bind(key1)
        .bind(key2)
        .fetch_one(&mut *conn)
        .await
        .map_err(map_sqlx_error)?;

    if !acquired {
        // `conn` drops here, returning to the pool.
        return Ok(None);
    }

    let holder_id = Uuid::new_v4().to_string();
    // `ON CONFLICT (name) DO UPDATE`, not a bare `INSERT`: reaching this line
    // means `pg_try_advisory_lock` just told us, authoritatively, that no
    // live session holds this name's advisory lock right now — Postgres
    // itself already ruled out a live foreign holder. A pre-existing
    // `cluster_lock` row for `name` can therefore only be stale metadata left
    // behind by a crashed holder whose session died before its own
    // `release()` could delete it (DESIGN.md §5.2 point 4's crash/disconnect
    // safety net — the advisory lock auto-releases on disconnect, but
    // nothing else automatically cleans up this bookkeeping row). A bare
    // `INSERT` would fail that row's `PRIMARY KEY (name)` constraint on every
    // such re-acquisition, turning a documented crash-recovery path into a
    // permanent `Provider` error until the TTL reaper happens to sweep the
    // stale row first (`PG-LOCK-007`/`PG-SPEC-004` exercise exactly this).
    let insert_result = sqlx::query(&format!(
        "INSERT INTO {table} (name, holder_id, acquired_at, ttl_ms) VALUES ($1, $2, now(), $3) \
         ON CONFLICT (name) DO UPDATE SET holder_id = EXCLUDED.holder_id, \
         acquired_at = EXCLUDED.acquired_at, ttl_ms = EXCLUDED.ttl_ms"
    ))
    .bind(name)
    .bind(&holder_id)
    .bind(ttl_ms)
    .execute(&mut *conn)
    .await;

    if let Err(err) = insert_result {
        // Compensate: we hold the advisory lock but failed to record it, so
        // release it now rather than leak a lock the reaper can never find
        // (its sweep only ever looks at `cluster_lock` rows). Surface a failed
        // compensating unlock the same way `reaper::reclaim` does (PGR-L1) —
        // otherwise the advisory lock silently leaks until session disconnect.
        let compensating_unlock: Result<bool, ClusterError> =
            sqlx::query_scalar("SELECT pg_advisory_unlock($1, $2)")
                .bind(key1)
                .bind(key2)
                .fetch_one(&mut *conn)
                .await
                .map_err(map_sqlx_error);
        if let Err(unlock_err) = &compensating_unlock {
            observability::emit_provider_error(
                &*ctx.metrics,
                ctx.provider,
                "try_lock_compensating_unlock",
                observability::ResourceId::Lock(name),
                unlock_err,
            );
        }
        return Err(map_sqlx_error(err));
    }

    // Serialize against shutdown (PGR-L2): `stop()` cancels `shutdown` and then
    // drains a snapshot of `held` (looping until empty) before `pool.close()`.
    // If we registered this lock after that drain observed `held` empty, the
    // guard task would exit immediately on the already-cancelled token and leave
    // its pinned connection stranded in the pool's checked-out set, so
    // `pool.close()` would block forever.
    //
    // Two-part guard. First, a cheap pre-insert bail: if shutdown is already
    // observed here we undo the advisory lock + row and never register, avoiding
    // the work entirely (the common single-thread case).
    if ctx.shutdown.is_cancelled() {
        let _unlocked: Result<bool, _> = sqlx::query_scalar("SELECT pg_advisory_unlock($1, $2)")
            .bind(key1)
            .bind(key2)
            .fetch_one(&mut *conn)
            .await;
        let _deleted = sqlx::query(&format!(
            "DELETE FROM {table} WHERE name = $1 AND holder_id = $2"
        ))
        .bind(name)
        .bind(&holder_id)
        .execute(&mut *conn)
        .await;
        return Err(ClusterError::Shutdown);
    }

    let (rx, guard) = LockGuard::channel(name.to_owned(), 4);
    held.insert(
        name.to_owned(),
        HeldLock {
            conn: Arc::new(tokio::sync::Mutex::new(conn)),
            key1,
            key2,
            holder_id: holder_id.clone(),
        },
    );

    tokio::spawn(run_guard_task(
        name.to_owned(),
        Arc::clone(held),
        table.to_owned(),
        ctx.clone(),
        holder_id.clone(),
        rx,
    ));

    // Second, a post-insert re-check that closes the multi-thread window the
    // pre-insert check alone cannot: on a multi-worker runtime a concurrent
    // `stop()` may cancel and finish draining *between* the check above and this
    // `held.insert`. If shutdown is observed after the insert we reject this
    // acquisition (returning the guard would hand back a lock that shutdown is
    // about to — or already did — forcibly reclaim), reclaiming the connection
    // ourselves if we still hold it so it is never stranded (PGR-L2).
    // `remove_if` is atomic with `drain_held`'s own `remove`, so at most one path
    // reclaims it (`None` here means `drain_held` already did). The consumer's
    // `guard` is dropped on this early return, unwinding its guard task too.
    if ctx.shutdown.is_cancelled() {
        if let Some((_, held_lock)) = held.remove_if(name, |_, entry| entry.holder_id == holder_id)
        {
            reaper::reclaim(name, held_lock, &*ctx.metrics, ctx.provider).await;
        }
        return Err(ClusterError::Shutdown);
    }

    Ok(Some(guard))
}

/// Drives one held lock's [`LockCommandReceiver`](cluster_sdk::LockCommandReceiver)
/// until [`Release`](cluster_sdk::LockRequest::Release) or the consumer drops
/// the [`LockGuard`] without releasing — in which case this task simply exits,
/// leaving the connection in `held` for the TTL reaper to reclaim, exactly per
/// the trait's TTL safety-net contract (no I/O in `Drop`).
async fn run_guard_task(
    name: String,
    held: Arc<DashMap<String, HeldLock>>,
    table: String,
    ctx: GuardContext,
    holder_id: String,
    mut commands: cluster_sdk::LockCommandReceiver,
) {
    // Re-assert `synchronous_commit = on` on this lock's own pinned connection
    // on its own interval (DESIGN.md §3.4 residual gap): `before_acquire` fires
    // only at checkout, but a lock connection is checked out once and pinned
    // for the lock's whole lifetime (§3.3), so an external mid-session flip
    // would otherwise persist unchecked. Done *here*, on the guard task, rather
    // than in the TTL reaper's sweep (`GAP-SOLUTIONS.md` §5). The pinned
    // connection is behind an async mutex (`HeldLock`), so this autonomous
    // re-assertion never holds the `DashMap` shard lock across its `.await` and
    // cannot deadlock a concurrent reaper `remove`. First tick is one full
    // interval out, so a just-acquired connection (already `on`) is not
    // needlessly re-asserted immediately.
    let mut reassert = tokio::time::interval_at(
        tokio::time::Instant::now() + ctx.reassert_interval,
        ctx.reassert_interval,
    );
    reassert.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            // Graceful shutdown: exit promptly rather than parking on the next
            // reassert tick (PGR-L2). The pinned connection is left in `held`
            // for `drain_held` (DESIGN.md §10 step 4) to unlock and return —
            // exactly as it is for a consumer that drops its guard without
            // releasing, so renew/release/reassert are all no-ops from here.
            () = ctx.shutdown.cancelled() => return,
            _ = reassert.tick() => {
                if let Err(err) = reassert_synchronous_commit(&held, &name, &holder_id).await {
                    observability::emit_provider_error(
                        &*ctx.metrics,
                        ctx.provider,
                        "reassert_synchronous_commit",
                        observability::ResourceId::Lock(&name),
                        &err,
                    );
                }
            }
            request = commands.recv() => {
                let Some(request) = request else {
                    // Consumer dropped the guard without releasing: exit, leaving
                    // the connection in `held` for the TTL reaper to reclaim.
                    return;
                };
                match request {
                    cluster_sdk::LockRequest::Renew { new_ttl, responder } => {
                        let span = tracing::info_span!(
                            spans::LOCK_RENEW, provider = %ctx.provider, lock = %name
                        );
                        let started = std::time::Instant::now();
                        let result = renew(&held, &name, &table, new_ttl, &holder_id)
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
                        let result = release(&held, &name, &table, &holder_id)
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

/// Re-asserts `synchronous_commit = on` on the lock's own pinned connection
/// (DESIGN.md §3.4). A no-op if the entry is gone — the TTL reaper reclaimed it
/// or `release` already ran. Clones the connection handle out under a brief
/// `get` and awaits on its async mutex (see [`HeldLock`]), so it never holds the
/// `DashMap` shard lock across the `.await`.
///
/// Fences on `holder_id` (PGR-L1), exactly as `renew`/`release` do: after a TTL
/// lapse frees `name` and a *newer* holder re-acquires it, the original holder's
/// stale guard task keeps ticking. Without this fence it would resolve the
/// successor's entry and run `SET` on the successor's pinned connection,
/// contending with that holder's own renew/release. A non-matching (or absent)
/// entry means the lock is no longer ours — no-op.
async fn reassert_synchronous_commit(
    held: &Arc<DashMap<String, HeldLock>>,
    name: &str,
    holder_id: &str,
) -> Result<(), ClusterError> {
    // Clone the connection handle out under a brief synchronous `get`, then
    // release the shard lock before awaiting (see `HeldLock`): this is what
    // keeps the autonomous, timer-driven re-assertion from deadlocking the
    // reaper's `remove` of an expired-but-still-guarded key under a
    // current-thread runtime.
    let conn = {
        let Some(entry) = held.get(name) else {
            return Ok(());
        };
        if entry.holder_id != holder_id {
            return Ok(());
        }
        Arc::clone(&entry.conn)
    };
    let mut conn = conn.lock().await;
    sqlx::query("SET synchronous_commit = on")
        .execute(&mut **conn)
        .await
        .map_err(map_sqlx_error)?;
    Ok(())
}

/// `Renew`: resets `acquired_at`/`ttl_ms` on our own pinned connection without
/// relinquishing it (`held.get_mut`, not `remove`). Zero rows updated means the
/// TTL reaper already reclaimed this lock out from under us — the row is gone,
/// so the caller no longer holds it (`ClusterError::LockExpired`).
async fn renew(
    held: &Arc<DashMap<String, HeldLock>>,
    name: &str,
    table: &str,
    new_ttl: Duration,
    holder_id: &str,
) -> Result<(), ClusterError> {
    // Clone the connection handle out under a brief synchronous `get`, then
    // release the shard lock before awaiting on the async mutex (see `HeldLock`).
    // Fence on `holder_id` (PGR-L1): if the entry now belongs to a newer holder
    // that re-acquired `name` after our TTL lapsed, our lock is gone — report it
    // expired rather than resetting the successor's TTL on its own connection.
    let conn = {
        let Some(entry) = held.get(name) else {
            return Err(ClusterError::LockExpired {
                name: name.to_owned(),
            });
        };
        if entry.holder_id != holder_id {
            return Err(ClusterError::LockExpired {
                name: name.to_owned(),
            });
        }
        Arc::clone(&entry.conn)
    };
    let ttl_ms = duration_to_ttl_ms(new_ttl)?;
    let mut conn = conn.lock().await;
    let updated: Option<i32> = sqlx::query_scalar(&format!(
        "UPDATE {table} SET ttl_ms = $1, acquired_at = now() WHERE name = $2 AND holder_id = $3 \
         RETURNING 1"
    ))
    .bind(ttl_ms)
    .bind(name)
    .bind(holder_id)
    .fetch_optional(&mut **conn)
    .await
    .map_err(map_sqlx_error)?;

    if updated.is_some() {
        Ok(())
    } else {
        Err(ClusterError::LockExpired {
            name: name.to_owned(),
        })
    }
}

/// `Release`: reclaims our pinned connection from `held` (atomically —
/// `held.get_mut` in a concurrent `renew` cannot observe a half-removed
/// entry), unlocks, deletes the metadata row, and wakes any blocked `lock()`
/// waiters. If `held` no longer has our entry, the TTL reaper already
/// reclaimed it before this release arrived — best-effort `Ok(())` per the
/// trait's TTL-lapsed narrowing (DESIGN.md §3.7): there is nothing left for us
/// to release.
async fn release(
    held: &Arc<DashMap<String, HeldLock>>,
    name: &str,
    table: &str,
    holder_id: &str,
) -> Result<(), ClusterError> {
    // Fence on `holder_id` (PGR-L1): `remove_if` atomically removes the entry
    // only when it is still *our* acquisition, so a stale guard whose lock
    // already lapsed and was re-acquired by a newer holder cannot unlock the
    // successor's live connection. A non-matching (or absent) entry means the
    // TTL reaper reclaimed us or a successor owns `name` now — best-effort
    // `Ok(())` per the trait's TTL-lapsed narrowing (DESIGN.md §3.7).
    let Some((_, held_lock)) = held.remove_if(name, |_, entry| entry.holder_id == holder_id) else {
        return Ok(());
    };
    let mut conn = held_lock.conn.lock().await;

    let _unlocked: bool = sqlx::query_scalar("SELECT pg_advisory_unlock($1, $2)")
        .bind(held_lock.key1)
        .bind(held_lock.key2)
        .fetch_one(&mut **conn)
        .await
        .map_err(map_sqlx_error)?;

    sqlx::query(&format!(
        "DELETE FROM {table} WHERE name = $1 AND holder_id = $2"
    ))
    .bind(name)
    .bind(holder_id)
    .execute(&mut **conn)
    .await
    .map_err(map_sqlx_error)?;

    notify::notify_released(&mut **conn, name).await?;
    // The guard drops here; `conn` returns to the pool once the last `Arc`
    // (any in-flight renew/reassert clone) is also dropped.
    Ok(())
}

#[async_trait]
impl DistributedLockBackend for PostgresLock {
    fn features(&self) -> LockFeatures {
        // `pg_advisory_lock` + a metadata table under the same
        // `synchronous_commit = on` enforcement (DESIGN.md §3.4) as the cache —
        // ACID-correct mutual exclusion, not merely advisory coordination.
        LockFeatures::new(true)
    }

    async fn try_lock(&self, name: &str, ttl: Duration) -> Result<LockGuard, ClusterError> {
        let span =
            tracing::info_span!(spans::LOCK_TRY_LOCK, provider = %self.provider, lock = %name);
        let started = std::time::Instant::now();
        let ctx = self.guard_context();
        let out = async {
            match try_acquire(&self.pool, &self.table, &self.held, name, ttl, &ctx).await? {
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
        // `LISTEN`ing on the same pinned connection the eventual lock will
        // hold (sqlx's `PgListener` owns its own single-connection pool, with
        // no public way to hand it an already-checked-out `PoolConnection`),
        // wait on the in-process `release_waiters` registry the dedicated
        // LISTEN task (`notify::spawn_release_listen_task`) feeds. A short
        // heartbeat retry runs alongside the wake-up as a safety net against
        // a missed notification (task not started yet, a dropped wake), so a
        // lost wake only costs latency up to the heartbeat interval, never
        // correctness — the loop always re-attempts `pg_try_advisory_lock`
        // itself as the source of truth.
        const HEARTBEAT: Duration = Duration::from_millis(250);

        let span = tracing::info_span!(spans::LOCK_LOCK, provider = %self.provider, lock = %name);
        let op_started = std::time::Instant::now();
        let ctx = self.guard_context();
        let out = async {
            let started = tokio::time::Instant::now();
            let deadline = started + timeout;

            let mut first_attempt = true;
            loop {
                // Bound each *subsequent* acquire attempt by the remaining budget,
                // not just the gap between attempts (PGR-L3): `pool.acquire()`
                // alone can block up to `pool_acquire_timeout` (default 5s), so
                // checking `deadline` only after each full `try_acquire` let a
                // single attempt overshoot the caller's `timeout`. If the wrapped
                // attempt is cancelled mid-acquisition, the pool's `after_release`
                // hook releases any advisory lock the dropped future had taken
                // (see `pg_setup::base_pool_options`), so no pooled connection
                // retains a live lock.
                //
                // The *first* attempt always runs (bounded only by the pool's own
                // acquire timeout), even when `timeout` is 0/expired — matching the
                // CAS-based default's attempt-before-budget-check ordering, so
                // `lock(free_lock, ttl, Duration::ZERO)` still acquires instead of
                // returning `LockTimeout` without ever trying.
                let now = tokio::time::Instant::now();
                let remaining = deadline.saturating_duration_since(now);
                if remaining.is_zero() && !first_attempt {
                    return Err(ClusterError::LockTimeout {
                        name: name.to_owned(),
                        waited: started.elapsed(),
                    });
                }

                let acquire = try_acquire(&self.pool, &self.table, &self.held, name, ttl, &ctx);
                let attempted = if remaining.is_zero() {
                    // First attempt with no remaining budget: run it unbounded
                    // (pool-acquire-timeout bounds it in practice) rather than skip.
                    acquire.await?
                } else {
                    match tokio::time::timeout(remaining, acquire).await {
                        Ok(result) => result?,
                        Err(_elapsed) => {
                            return Err(ClusterError::LockTimeout {
                                name: name.to_owned(),
                                waited: started.elapsed(),
                            });
                        }
                    }
                };
                first_attempt = false;
                if let Some(guard) = attempted {
                    return Ok(guard);
                }

                let now = tokio::time::Instant::now();
                if now >= deadline {
                    return Err(ClusterError::LockTimeout {
                        name: name.to_owned(),
                        waited: started.elapsed(),
                    });
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
        let lock = PostgresLock::new(
            pool,
            &config.schema,
            config.lock_reaper_interval(),
            Arc::clone(&metrics),
            crate::provider::PROVIDER_NAME,
            shutdown.clone(),
        );

        let meter = reaper_meter.unwrap_or_else(reaper::reaper_meter);
        let reaper = lock.spawn_reaper(
            config.lock_reaper_interval(),
            reaper::LockReaperMetrics::new(
                &meter,
                crate::provider::PROVIDER_NAME,
                Arc::clone(&metrics),
            ),
            i64::from(config.lock_name_cardinality_warn_threshold),
            shutdown.clone(),
        );
        let release_listener = notify::spawn_release_listen_task(
            config.connection_string.clone(),
            lock.release_waiters(),
            shutdown.clone(),
        );

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

    /// Test-only access to the concrete [`PostgresLock`], for `pg_spec_005`'s
    /// pinned-connection GUC probing (the `dyn` [`lock`](Self::lock) has no such
    /// seam). Gated behind `--features integration` (PGR-M8).
    #[cfg(feature = "integration")]
    #[doc(hidden)]
    #[must_use]
    pub fn __test_lock(&self) -> Arc<PostgresLock> {
        Arc::clone(&self.lock)
    }

    /// Cancels the lock TTL reaper and release-wake listener, then closes the
    /// pool. Advisory locks held by pinned connections auto-release when the
    /// pool closes (DESIGN.md §10).
    pub async fn stop(mut self) {
        self.shutdown.cancel();
        for task in [self.reaper.take(), self.release_listener.take()]
            .into_iter()
            .flatten()
        {
            let _exited = task.await;
        }
        // Drain any lock still held at shutdown before closing the pool — see
        // `PostgresLock::drain_held` for why `pool.close()` would otherwise
        // block forever on a still-pinned lock connection (DESIGN.md §10).
        self.lock.drain_held().await;
        self.lock.pool().close().await;
        self.stopped = true;
    }
}

impl Drop for PostgresLockHandle {
    fn drop(&mut self) {
        if self.stopped {
            return;
        }
        if std::thread::panicking() {
            warn!(
                "PostgresLockHandle dropped during panic unwind without stop(); \
                 skipping debug panic to avoid double-panic abort"
            );
            return;
        }
        #[cfg(debug_assertions)]
        panic!("PostgresLockHandle dropped without stop() - programming error");
        #[cfg(not(debug_assertions))]
        warn!(
            "PostgresLockHandle dropped without stop() - programming error; \
             background tasks may leak"
        );
    }
}

#[cfg(test)]
#[path = "lock_tests.rs"]
mod tests;
