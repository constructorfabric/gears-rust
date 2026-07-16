//! The `cluster_lock` TTL reaper background task (DESIGN.md §5.2).
//!
//! Scans `cluster_lock` every `interval` for rows past
//! `acquired_at + ttl_ms * interval '1ms' < now()`, deletes them, and — for
//! each one this process itself still has pinned (`held.remove(name)` returns
//! `Some`) — calls `pg_advisory_unlock` **on that exact connection**, since
//! `pg_advisory_unlock` called from any other session is a silent no-op
//! (session-scoped advisory locks can only be released by the session that
//! acquired them). That's why this reaper needs `held` at all, rather than
//! just running the unlock on a connection of its own: DESIGN.md §5.2 says it
//! plainly — "The advisory unlock must run on the same connection that holds
//! the lock... The reaper therefore tracks the connection ID per lock." This
//! plugin tracks the connection object itself, one step more directly than an
//! ID, via the same `held` registry `lock/mod.rs`'s guard task uses.
//!
//! A row whose name isn't in this process's own `held` map belongs to a
//! different fleet instance (or was already reclaimed by a racing `release()`
//! in this same process — `DashMap::remove` makes exactly one of the two
//! win). Either way, deleting the metadata row is still correct: the actual
//! mutual-exclusion guarantee lives in Postgres's advisory-lock table, not in
//! this bookkeeping row, so a stale row for a lock still legitimately held by
//! a live remote session costs that session nothing — a subsequent
//! `pg_try_advisory_lock` from elsewhere still correctly fails while that
//! session is alive. This is the accepted, documented nature of a TTL layered
//! on top of a primitive with no native TTL (DESIGN.md §5.2's opening line).

use std::sync::Arc;
use std::time::Duration;

use cluster_sdk::observability::fields::label;
use cluster_sdk::observability::{self, ResourceId};
use cluster_sdk::{ClusterError, ClusterMetrics};
use dashmap::DashMap;
use opentelemetry::metrics::{Gauge, Histogram, Meter};
use opentelemetry::{InstrumentationScope, KeyValue, global};
use sqlx::PgPool;
use tokio_util::sync::CancellationToken;
use tracing::warn;

use super::HeldLock;
use crate::lock::notify;
use crate::pg_error::map_sqlx_error;

/// Instrumentation scope for this plugin's own, non-contract metrics (DESIGN.md
/// §8): the lock-name cardinality gauge and the reaper-sweep-duration histogram.
/// Distinct from the shared `cf-gears-cluster` scope `OtelClusterMetrics` uses
/// for the ADR-004 contract signals — these two are plugin-local additions
/// emitted via a meter this plugin owns directly, not through the
/// `ClusterMetrics` port (which has no gauge method).
const REAPER_SCOPE: &str = "cf-postgres-cluster-plugin";

/// The process-global meter under [`REAPER_SCOPE`], used when no meter is
/// injected (production). Tests inject their own meter over an in-memory reader
/// to read the gauge back (see `PostgresLockBuilder::__with_reaper_meter`).
pub fn reaper_meter() -> Meter {
    global::meter_with_scope(InstrumentationScope::builder(REAPER_SCOPE).build())
}

/// Builds the shared `cluster_postgres_reaper_sweep_duration_seconds` histogram
/// (DESIGN.md §8). Both TTL reapers (`primitive={cache,lock}`) record into it;
/// creating it by the same name on the same meter yields the same instrument.
pub fn sweep_duration_histogram(meter: &Meter) -> Histogram<f64> {
    meter
        .f64_histogram("cluster_postgres_reaper_sweep_duration_seconds")
        .with_description("Postgres cluster TTL reaper sweep duration")
        .with_unit("s")
        .build()
}

/// Plugin-local (non-ADR-004) lock-reaper metrics, emitted via a directly-owned
/// OpenTelemetry meter rather than the `ClusterMetrics` contract sink — DESIGN.md
/// §8 classifies both of these as plugin-local, and `ClusterMetrics` exposes no
/// gauge method anyway (see `GAP-SOLUTIONS.md` §6).
pub struct LockReaperMetrics {
    provider: &'static str,
    /// `cluster_postgres_lock_active_names{provider}` — the cluster-wide count
    /// of distinct held lock names (`= count(*)` of `cluster_lock`, not
    /// `held.len()`, which is only this instance's slice — DESIGN.md §8).
    active_names: Gauge<i64>,
    /// `cluster_postgres_reaper_sweep_duration_seconds{provider,primitive=lock}`.
    sweep_duration: Histogram<f64>,
    /// The ADR-004 sink the reaper routes backend failures through
    /// (`emit_provider_error` → `cluster_provider_errors_total` + a
    /// `cluster.provider.error` ERROR log). Distinct from the plugin-local
    /// `OTel` instruments above (DESIGN.md §8 / PGR-M1).
    errors: Arc<dyn ClusterMetrics>,
}

impl LockReaperMetrics {
    pub fn new(meter: &Meter, provider: &'static str, errors: Arc<dyn ClusterMetrics>) -> Self {
        Self {
            provider,
            active_names: meter
                .i64_gauge("cluster_postgres_lock_active_names")
                .with_description("Distinct lock names currently held cluster-wide")
                .build(),
            sweep_duration: sweep_duration_histogram(meter),
            errors,
        }
    }

    /// The ADR-004 error sink and the bounded `provider` label, for the sweep's
    /// `emit_provider_error` calls.
    fn errors(&self) -> (&dyn ClusterMetrics, &'static str) {
        (&*self.errors, self.provider)
    }

    fn record_active_names(&self, count: i64) {
        self.active_names
            .record(count, &[KeyValue::new(label::PROVIDER, self.provider)]);
    }

    fn record_sweep_duration(&self, seconds: f64) {
        self.sweep_duration.record(
            seconds,
            &[
                KeyValue::new(label::PROVIDER, self.provider),
                KeyValue::new(label::PRIMITIVE, "lock"),
            ],
        );
    }
}

/// Reclaims one lock this process still has pinned: releases the advisory lock
/// on its exact connection and wakes any blocked `lock()` waiters. Best-effort
/// throughout — logs and continues on either failure, since the connection is
/// being dropped either way (returning to the pool), and a failed unlock just
/// means the session-disconnect safety net (DESIGN.md §5.2 point 4) is what
/// eventually frees it instead.
///
/// `pub(super)` so the shutdown drain (`PostgresLock::drain_held`, DESIGN.md
/// §10 step 4) can reuse this exact per-lock release path rather than
/// duplicating it — the drain is just this, run unconditionally for every
/// still-held lock instead of only the TTL-expired ones the sweep targets.
pub(super) async fn reclaim(
    name: &str,
    held_lock: HeldLock,
    metrics: &dyn ClusterMetrics,
    provider: &'static str,
) {
    // `held_lock` was just `remove`d from the map, so we own an `Arc` to the
    // connection; lock the async mutex (waiting out any in-flight renew/reassert
    // clone, without blocking a thread). The connection returns to the pool once
    // this `Arc` and any concurrent clone are dropped.
    let mut conn = held_lock.conn.lock().await;
    if let Err(err) = sqlx::query("SELECT pg_advisory_unlock($1, $2)")
        .bind(held_lock.key1)
        .bind(held_lock.key2)
        .execute(&mut **conn)
        .await
    {
        observability::emit_provider_error(
            metrics,
            provider,
            "reaper_reclaim",
            ResourceId::Lock(name),
            &map_sqlx_error(err),
        );
    }
    if let Err(err) = notify::notify_released(&mut **conn, name).await {
        observability::emit_provider_error(
            metrics,
            provider,
            "reaper_reclaim_notify",
            ResourceId::Lock(name),
            &err,
        );
    }
    // `conn` drops here, returning to the pool.
}

/// Runs one sweep. Returns the number of expired rows reclaimed (regardless
/// of whether this process could act on the advisory lock for each one).
async fn sweep(
    pool: &PgPool,
    table: &str,
    held: &Arc<DashMap<String, HeldLock>>,
    metrics: &dyn ClusterMetrics,
    provider: &'static str,
) -> Result<usize, ClusterError> {
    let expired: Vec<(String, String)> = sqlx::query_as(&format!(
        "DELETE FROM {table} WHERE acquired_at + ttl_ms * interval '1ms' < now() \
         RETURNING name, holder_id"
    ))
    .fetch_all(pool)
    .await
    .map_err(map_sqlx_error)?;

    for (name, holder_id) in &expired {
        // Fence reclamation on `holder_id` (PGR-L1): reclaim the pinned
        // connection only when *this* process still holds the exact acquisition
        // whose row just expired. A non-matching entry means a newer holder
        // re-acquired `name` (its fresh row is not expired, so it was not among
        // the deleted rows) — reclaiming it would unlock a live lock. `None`
        // means the row belongs to a different fleet instance, or a racing
        // `release()` in this process already reclaimed it.
        if let Some((_, held_lock)) = held.remove_if(name, |_, entry| &entry.holder_id == holder_id)
        {
            reclaim(name, held_lock, metrics, provider).await;
        }
    }

    Ok(expired.len())
}

/// Counts every row in `cluster_lock` — the cluster-wide number of distinct
/// lock names currently held (DESIGN.md §8), the value behind the
/// `cluster_postgres_lock_active_names` gauge. Deliberately **not** `held.len()`,
/// which is only this instance's slice of the fleet's locks.
async fn active_name_count(pool: &PgPool, table: &str) -> Result<i64, ClusterError> {
    sqlx::query_scalar(&format!("SELECT count(*) FROM {table}"))
        .fetch_one(pool)
        .await
        .map_err(map_sqlx_error)
}

/// Spawns the lock TTL reaper.
///
/// Besides the TTL sweep, each tick records the reaper-sweep-duration histogram
/// and the `cluster_postgres_lock_active_names` gauge, and logs
/// `cluster.lock.name_cardinality_high` (WARN) while the distinct-name count is
/// over `warn_threshold` (DESIGN.md §8). The WARN is naturally rate-limited to
/// once per interval — it only fires from the sweep, which runs once per tick.
///
/// `synchronous_commit = on` re-assertion on pinned connections is **not** done
/// here: it lives on each guard task's own interval (`run_guard_task`,
/// `lock/mod.rs`), so no second accessor takes `held.get_mut` on a live key
/// across an `.await` (which would deadlock the guard's own renew/release under
/// the current-thread runtime — see `GAP-SOLUTIONS.md` §5).
pub(super) fn spawn_lock_reaper(
    pool: PgPool,
    table: String,
    held: Arc<DashMap<String, HeldLock>>,
    interval: Duration,
    metrics: LockReaperMetrics,
    warn_threshold: i64,
    cancel: CancellationToken,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(interval);
        loop {
            tokio::select! {
                () = cancel.cancelled() => break,
                _ = tick.tick() => {
                    let (errors, provider) = metrics.errors();
                    let started = tokio::time::Instant::now();
                    let swept = sweep(&pool, &table, &held, errors, provider).await;
                    metrics.record_sweep_duration(started.elapsed().as_secs_f64());
                    if let Err(err) = swept {
                        observability::emit_provider_error(
                            errors,
                            provider,
                            "reaper_sweep",
                            ResourceId::Name(&table),
                            &err,
                        );
                        continue;
                    }
                    // Distinct held names = live row count *after* the TTL
                    // delete, so expired-but-not-yet-swept rows never inflate it.
                    match active_name_count(&pool, &table).await {
                        Ok(count) => {
                            metrics.record_active_names(count);
                            if count > warn_threshold {
                                warn!(
                                    active_names = count,
                                    threshold = warn_threshold,
                                    "cluster.lock.name_cardinality_high"
                                );
                            }
                        }
                        Err(err) => observability::emit_provider_error(
                            errors,
                            provider,
                            "reaper_active_name_count",
                            ResourceId::Name(&table),
                            &err,
                        ),
                    }
                }
            }
        }
    })
}
