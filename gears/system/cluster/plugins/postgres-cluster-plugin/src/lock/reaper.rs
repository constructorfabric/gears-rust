//! The `cluster_lock` TTL reaper background task (DESIGN.md §5.2).
//!
//! Scans `cluster_lock` for rows past `expires_at <= now()`, deletes them in
//! bounded batches, and announces each reclaimed name on `cluster_lock_released`
//! so blocked `lock()` waiters wake instead of sitting out their heartbeat.
//!
//! ## It is garbage collection, not exclusion
//!
//! Nothing here is correctness-critical for mutual exclusion, and that is the
//! single biggest change from the advisory-lock design. Acquire already treats a
//! lapsed row as free and takes it in the same statement (`lock`'s module doc), so a
//! sweep that never runs costs table growth and slower waiter wake-ups — never a
//! double-hold, and never a name wedged fleet-wide.
//!
//! Which also means it no longer matters *who* runs it. Every instance sweeps the
//! same shared table, and whoever wins the `DELETE` frees the name for everyone,
//! because there is no session-scoped unlock that has to route back to the row's
//! owner. The three mechanisms that used to reconcile that race — the owner's own
//! reclaim, the `cluster_lock_released` NOTIFY back to the owner, and the
//! `audit_held` backstop — have nothing left to reconcile.
//!
//! **Nothing depends on this task for liveness either, now.** An earlier revision
//! ran an incarnation-keyed *orphan* sweep on this wake loop, which was the one
//! liveness responsibility here: it reclaimed a row this instance wrote that no live
//! local guard accounted for — what an acquisition cancelled between its committed
//! INSERT and its registration leaves behind — because such a row was unexpired *and*
//! vouched for by a live beacon and so could be stolen by nobody, including its own
//! writer. Both halves of that went with the beacon (`lock`'s module doc): there is
//! no incarnation key to filter on, and no local registry to exempt. An abandoned
//! row is now simply a lapsed lease at its deadline, reclaimed by the TTL sweep
//! below like any other, so its name is taken until its TTL rather than until the
//! next interval wake. That is the residual cost of uniform expiry, recorded in
//! ADR-012 rather than hidden here.
//!
//! ## Wake schedule
//!
//! Each loop iteration sweeps, then sleeps until the *earlier* of the next
//! metrics tick (`interval`) and the next row's deadline
//! (`min(expires_at)`, an index-only read on `cluster_lock_expires_idx`):
//!
//! * The **`interval` cap** is what keeps `cluster_postgres_lock_active_names`
//!   and the cardinality WARN on their configured cadence. A lock's row count
//!   moves with every acquire/release, not only with expiry, so the gauge cannot
//!   be allowed to go quiet just because nothing is near its TTL — that is why
//!   the reaper does not simply sleep until `min(expires_at)` however far out it
//!   is. Only these interval-boundary wakes do the gauge work; expiry-driven
//!   wakes skip it and stay two indexed queries wide.
//! * The **`min(expires_at)` shortening** makes reclamation prompt: an expired
//!   lock is reaped near its actual deadline instead of up to a full `interval`
//!   late, which shortens how long a "holder is alive but forgot to release"
//!   lock (DESIGN.md §5.2 point 4) blocks every waiter behind it.
//! * The **`deadline_hint` signal** is what makes that hold for a lock acquired
//!   *after* a wake, too. The sleep is computed from the table as it looked at
//!   wake time, so without a signal a lock whose whole lifetime fits inside one
//!   sleep (TTL ≲ `interval`) would go unreclaimed until that sleep ended.
//!   `try_acquire` and `renew` therefore notify this task once their write is
//!   committed — but **only when the TTL they wrote is shorter than `interval`**,
//!   which is exactly the condition above (`lock::should_hint`). A longer deadline
//!   cannot be missed: this task's sleep is capped at `interval`, so it re-reads
//!   the table before that deadline falls due regardless.
//!
//!   Local-only by design, and sufficient rather than partial: the sweep is
//!   promptness only, so a hint no other instance hears costs at most a waiter's
//!   heartbeat. Any acquirer takes an expired row on its own predicate whether or
//!   not anyone swept it.
//! * [`wake_floor`] floors both the expiry-driven and the signalled wake. Without
//!   it, N locks with staggered deadlines that keep getting renewed would wake
//!   this task once per deadline that passes, and a burst of acquisitions would
//!   wake it once per acquisition; the floor coalesces either into at most one
//!   wake per floor. It is [`MIN_WAKE`] capped by `interval`, so a deliberately
//!   sub-100ms cadence is honoured rather than stretched.
//!
//! The signal is only ever a *hint*: every wake re-derives its own schedule from
//! the table, so a lost notification costs at most one late sweep and never a
//! missed reclamation.
//!
//! A *spurious* one is not symmetric with that, and the asymmetry is worth
//! stating plainly because getting it wrong is not a one-off cost. `Notify` holds
//! a single permit, so a signal arriving while none is pending leaves the
//! `notified()` branch below permanently ready — and a permanently-ready branch
//! wins the wake `select!` against a sleep that is not, collapsing *every*
//! subsequent sleep to [`MIN_WAKE`]. One over-eager caller therefore does not cost
//! one extra sweep; it costs one full iteration — sweep plus next-expiry probe —
//! per wake floor, for as long as it keeps signalling. That is why the signalling
//! sites are gated (`lock::should_hint`) rather than fired on every write.
//!
//! Deliberately *not* bucketed into coarse expiry slices: for a lock the TTL is
//! the crash/wedge safety net, so rounding a deadline up to a shared boundary
//! would let a stale lock block waiters for up to a full bucket past its TTL,
//! and would make that extra grace depend on *when in the bucket* the lock
//! happened to be acquired. The exact deadline plus this schedule gets the
//! cheap-idle and sleep-until-due properties without buying them with TTL
//! precision.

use std::sync::Arc;
use std::time::Duration;

use cluster_sdk::observability::fields::label;
use cluster_sdk::observability::{self, ResourceId};
use cluster_sdk::{ClusterError, ClusterMetrics};
use opentelemetry::metrics::{Gauge, Histogram, Meter};
use opentelemetry::{InstrumentationScope, KeyValue, global};
use sqlx::AssertSqlSafe;
use sqlx::PgPool;
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use crate::lock::notify;
use crate::pg_error::map_sqlx_error;

/// Everything the TTL reaper needs: the table to sweep and the sink it reports
/// failures through.
///
/// Cheap to clone: a pool handle, one `Arc`, a `String`, and a `&'static str`.
#[derive(Clone)]
pub(super) struct ReaperContext {
    /// The write pool — every `cluster_lock` statement and every `pg_notify`.
    pub pool: PgPool,
    /// The schema-qualified `cluster_lock` table name.
    pub table: String,
    /// The ADR-004 error sink.
    pub metrics: Arc<dyn ClusterMetrics>,
    /// The bounded `provider` label.
    pub provider: &'static str,
    /// How long past `expires_at` a row is left in place so its `fence` survives
    /// the lapse (`config::PostgresLockConfig::fence_retention_ms`, §5.8.1).
    ///
    /// Zero reproduces the pre-retention behaviour exactly — sweep at the
    /// deadline — which is what makes every test written before this window
    /// existed a regression net for the arithmetic below.
    pub fence_retention: Duration,
}

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
    /// `cluster_postgres_notify_queue_usage{provider}` — the fraction (0.0..=1.0)
    /// of Postgres's notify queue currently in use (DESIGN.md §8, §11).
    notify_queue_usage: Gauge<f64>,
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
            notify_queue_usage: meter
                .f64_gauge("cluster_postgres_notify_queue_usage")
                .with_description(
                    "Fraction of the Postgres notify queue in use (cluster-wide, shared by every \
                     database on the server)",
                )
                .build(),
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

    fn record_notify_queue_usage(&self, fraction: f64) {
        self.notify_queue_usage
            .record(fraction, &[KeyValue::new(label::PROVIDER, self.provider)]);
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

/// The signals driving the reaper's wake schedule, bundled so
/// [`spawn_lock_reaper`] stays within a sane argument count (same reason
/// `lock/mod.rs` bundles `GuardContext`).
pub(super) struct ReaperWakeup {
    /// See [`PostgresLock::deadline_hint`](super::PostgresLock): signalled by a
    /// local `try_acquire`/`renew` so a sleep computed before that write is
    /// recomputed with the new deadline in view.
    pub deadline_hint: Arc<Notify>,
    /// Cancelled on `stop()`; ends the task from any point in its sleep.
    pub cancel: CancellationToken,
}

/// Expired rows one sweep statement claims. The sweep loops until a batch comes
/// back short, so a large backlog is still reclaimed in full — just across
/// several bounded statements instead of one unbounded `DELETE` holding row
/// locks on every matching row for however long that takes.
const SWEEP_BATCH: usize = 512;

/// Default floor on an expiry-driven or signalled wake — see the module doc's
/// wake schedule. Always applied via [`wake_floor`], which caps it by the
/// configured reaper interval so a deliberately sub-100ms cadence is still
/// honoured rather than silently stretched to this value.
const MIN_WAKE: Duration = Duration::from_millis(100);

/// The effective wake floor for a reaper configured with `interval`.
///
/// [`MIN_WAKE`] exists to stop deadline churn (many staggered TTLs, or a burst of
/// acquisitions) from waking the reaper faster than it can usefully sweep. It is
/// deliberately *not* allowed to override an explicitly configured cadence: an
/// operator who sets `lock_reaper_interval_ms: 50` asked for 50ms sweeps, and
/// flooring those at 100ms would silently halve their configured rate (the
/// plugin's own tests configure intervals in that range).
fn wake_floor(interval: Duration) -> Duration {
    MIN_WAKE.min(interval)
}

/// Deletes up to [`SWEEP_BATCH`] expired rows, returning the `name` of each.
///
/// The `LIMIT` lives in a subquery because `DELETE` takes no `LIMIT` of its own.
/// `ORDER BY expires_at` reaps longest-expired-first and rides
/// `cluster_lock_expires_idx` (the same index the `expires_at <= now()` filter
/// uses), so ordering costs nothing.
///
/// `FOR UPDATE SKIP LOCKED` does double duty. It keeps concurrent reapers — one
/// per fleet instance, all sweeping the same table — from serializing behind each
/// other on the same batch: each skips rows another has already claimed and takes
/// the next ones instead. It is also what makes the *outer* delete safe despite
/// matching on `name` alone rather than re-checking `expires_at`: a row whose
/// `renew` is in flight is already row-locked by that `UPDATE`, so this statement
/// skips it instead of deleting a lock that is in the act of being extended. (A
/// renew that commits *before* the subquery runs is excluded by the
/// `expires_at <= now()` filter; one that arrives *after* blocks on this
/// statement's own row lock and then finds the row gone, reporting `LockExpired`
/// — the same reaper-wins outcome the unbounded single-statement delete had.)
///
/// `RETURNING name` alone. It used to return `holder_id` too, so [`sweep`] could
/// prune the reaped names from the local holder registry in the same pass; that
/// registry went with the beacon (module doc), so the name is all a sweep needs.
///
/// # The retention window
///
/// The predicate is `expires_at <= now() - retention`, not `expires_at <= now()`:
/// a lapsed row is left in place so its `fence` outlives the lease it fenced, and
/// only becomes garbage a whole window later (§5.8.1). Two things make that
/// cheap rather than clever:
///
/// * **It stays indexed.** `now() - make_interval(...)` is a runtime constant, so
///   this is the same `cluster_lock_expires_idx` range scan it always was.
/// * **A retained row is not a held one.** Acquire's predicate is unchanged and
///   still reads `expires_at <= now()`, so a retained row is stolen in place at
///   `fence + 1` by the next acquirer exactly as before. The window changes when
///   the row is *deleted*, never when the lease is *free*.
async fn sweep_batch(
    pool: &PgPool,
    table: &str,
    retention: Duration,
) -> Result<Vec<String>, ClusterError> {
    sqlx::query_scalar(AssertSqlSafe(format!(
        "DELETE FROM {table} WHERE name IN (\
             SELECT name FROM {table} WHERE expires_at <= now() - make_interval(secs => $1) \
             ORDER BY expires_at LIMIT {SWEEP_BATCH} FOR UPDATE SKIP LOCKED\
         ) RETURNING name"
    )))
    .bind(retention.as_secs_f64())
    .fetch_all(pool)
    .await
    .map_err(map_sqlx_error)
}

/// Runs one sweep to completion, batch by batch. Returns the number of expired
/// rows reclaimed.
///
/// Re-checks `cancel` between batches so a shutdown arriving mid-backlog is not
/// held up for as many statements as the backlog needs — the rows left behind
/// are still expired, so the next reaper to run (here or on any other instance)
/// picks them up.
///
/// Every reclaimed name is announced, with no local/foreign split: whoever wins
/// the `DELETE` frees the name for the whole fleet, so there is no longer a
/// subset that needs routing back to its owner (module doc), and no local registry
/// left to prune alongside it.
pub(super) async fn sweep(
    ctx: &ReaperContext,
    cancel: &CancellationToken,
) -> Result<usize, ClusterError> {
    let mut reclaimed = 0;
    loop {
        let names = sweep_batch(&ctx.pool, &ctx.table, ctx.fence_retention).await?;
        if names.is_empty() {
            return Ok(reclaimed);
        }

        // Kept, but no longer the thing that wakes a waiter promptly: with a
        // retention window these names were free a whole window ago, and whoever
        // was blocked on one took it on its own heartbeat long before this delete.
        // The notification is now a cheap, harmless re-check rather than the fast
        // path — the fast path is `lock()`'s 250ms heartbeat, which is what
        // `lock`'s wait loop already documents as the bound a lapse is observed
        // within. Batched into one round-trip either way.
        if let Err(err) = notify::notify_released_many(&ctx.pool, &names).await {
            observability::emit_provider_error(
                &*ctx.metrics,
                ctx.provider,
                "reaper_sweep_notify",
                ResourceId::Name(&ctx.table),
                &err,
            );
        }

        reclaimed += names.len();
        // A short batch means the table had nothing more to give.
        if names.len() < SWEEP_BATCH || cancel.is_cancelled() {
            return Ok(reclaimed);
        }
    }
}

/// Seconds until the earliest deadline in `cluster_lock`, or `None` when the
/// table is empty. Negative when a row is already due (an aggregate over rows
/// this sweep's `SKIP LOCKED` left to another reaper, say).
///
/// The subtraction happens **in Postgres**, so what comes back is a delay
/// measured entirely on the database clock — this task can then sleep on its own
/// monotonic clock without ever comparing a Postgres timestamp against a
/// possibly-skewed local wall clock (PGR-C2). `min(expires_at)` is an index-only
/// read of `cluster_lock_expires_idx`'s leftmost entry, not a scan.
/// Fraction of Postgres's notify queue currently occupied (`0.0..=1.0`), read via
/// `pg_notification_queue_usage()`.
async fn notify_queue_usage(pool: &PgPool) -> Result<f64, ClusterError> {
    sqlx::query_scalar("SELECT pg_notification_queue_usage()")
        .fetch_one(pool)
        .await
        .map_err(map_sqlx_error)
}

/// Occupancy past which the reaper logs `cluster.provider.notify_queue_high`
/// (DESIGN.md §8, §11).
///
/// Deliberately well below Postgres's own server-side warning at 50%, because by
/// the time this matters it is already too late to react gracefully: at 100% the
/// queue does not shed load, it **fails the committing transaction** of every
/// `NOTIFY` on the server — so every deployment sharing that server starts
/// erroring at once, not just whichever one filled it.
///
/// This is the only signal that catches the *cause*. The queue is cluster-wide and
/// its tail advances only as fast as the slowest listening backend anywhere on the
/// server, so the deployment that fills it is frequently not the one that first
/// fails — and §11's guidance to watch write/provider errors only tells you, after
/// the fact, that somebody did.
const NOTIFY_QUEUE_WARN_FRACTION: f64 = 0.25;

/// The `ResourceId` a notify-queue sampling failure is attributed to.
///
/// `pg_notification_queue_usage()` reads a **server-wide** structure shared by
/// every database on the Postgres instance; it has nothing to do with
/// `cluster_lock`. Naming the table here (as this used to) put a misleading value
/// in the `name` field of `cluster.provider.error` — pointing an operator at a
/// table that is neither the cause nor the victim. A fixed stand-in keeps the
/// field honest and its cardinality at one (ADR-004).
const NOTIFY_QUEUE_RESOURCE: &str = "pg_notify_queue";

/// Samples the notify-queue occupancy, records the gauge, and warns past
/// [`NOTIFY_QUEUE_WARN_FRACTION`].
///
/// On the lock reaper's interval cadence rather than the cache reaper's for one
/// reason: the value is a property of the whole Postgres cluster, not of either
/// primitive, so it wants exactly one sampler per instance — and the lock reaper
/// is the one that runs in *both* plugin shapes (the standalone lock plugin has no
/// cache half). One round-trip per `lock_reaper_interval_ms` per instance.
///
/// `failing` carries whether the *previous* sample failed, and is what keeps a
/// persistently unreadable queue from emitting one `cluster.provider.error` ERROR
/// per interval forever. Only the first failure of a run logs; the rest still
/// increment `cluster_provider_errors_total` directly, so the counter — which is
/// what an alert should be built on — stays exact while the log records the
/// transition rather than the duration. Returns the new `failing` state.
async fn sample_notify_queue(
    ctx: &ReaperContext,
    metrics: &LockReaperMetrics,
    failing: bool,
) -> bool {
    match notify_queue_usage(&ctx.pool).await {
        Ok(fraction) => {
            record_notify_queue_sample(ctx, metrics, fraction, failing);
            false
        }
        Err(err) => {
            report_notify_queue_failure(metrics, &err, failing);
            true
        }
    }
}

/// Records a successful notify-queue sample, warning past
/// [`NOTIFY_QUEUE_WARN_FRACTION`] and noting a recovery from a previous failure.
fn record_notify_queue_sample(
    ctx: &ReaperContext,
    metrics: &LockReaperMetrics,
    fraction: f64,
    was_failing: bool,
) {
    // INFO, not WARN: a recovery transition is not actionable, and alerting rules
    // built on WARN volume would read it as a second fault rather than the end of
    // one. `cluster_provider_errors_total` already carries the exact fault count.
    if was_failing {
        info!(
            name: "cluster.provider.notify_queue_readable",
            provider = ctx.provider,
            "notify-queue sampling recovered"
        );
    }
    metrics.record_notify_queue_usage(fraction);
    if fraction > NOTIFY_QUEUE_WARN_FRACTION {
        warn!(
            name: "cluster.provider.notify_queue_high",
            provider = ctx.provider,
            usage = fraction,
            threshold = NOTIFY_QUEUE_WARN_FRACTION,
            "Postgres notify queue is filling; it is shared by every database on this server, and \
             at 100% every NOTIFY-issuing transaction on the server fails to commit. Look for a \
             listening session that has stopped draining, or a writer producing notifications \
             faster than listeners consume them"
        );
    }
}

/// Reports a failed notify-queue sample: the full ADR-004 signal pair on the
/// first failure of a run, the counter alone thereafter (see
/// [`sample_notify_queue`]).
fn report_notify_queue_failure(metrics: &LockReaperMetrics, err: &ClusterError, was_failing: bool) {
    let (errors, provider) = metrics.errors();
    if !was_failing {
        observability::emit_provider_error(
            errors,
            provider,
            "reaper_notify_queue_usage",
            ResourceId::Name(NOTIFY_QUEUE_RESOURCE),
            err,
        );
        return;
    }
    // Already reported. Keep `cluster_provider_errors_total` exact — it is what an
    // alert should be built on — without repeating the ERROR log every interval
    // for as long as the condition lasts.
    if let ClusterError::Provider { kind, .. } = err {
        errors.provider_error(observability::kind::label(*kind));
    }
}

/// What one wake's scheduling probe reads back.
pub(super) struct SchedulingProbe {
    /// Seconds until the earliest row in `cluster_lock` becomes *reapable* —
    /// `min(expires_at) + fence_retention` — or `None` when the table is empty.
    /// Negative when a row is already due.
    ///
    /// The retention window is folded in here rather than at the sleep site
    /// because this is the only place that knows what the table holds: a row that
    /// has lapsed but is still inside its window is not work, and waking for it
    /// would sweep nothing and then sleep again on the same value.
    pub seconds_until_expiry: Option<f64>,
}

/// Reads the next deadline.
///
/// The subtraction happens **in Postgres**, so what comes back is a delay
/// measured entirely on the database clock — this task can then sleep on its own
/// monotonic clock without ever comparing a Postgres timestamp against a
/// possibly-skewed local wall clock (PGR-C2). `min(expires_at)` is an index-only
/// read of `cluster_lock_expires_idx`'s leftmost entry, not a scan.
///
/// It used to carry `now()` back on the same select list, as the `acquired_at` fence
/// the orphan sweep compared against one wake later. That sweep is gone (module
/// doc), and with it the only reader of the database clock here.
pub(super) async fn probe_schedule(
    pool: &PgPool,
    table: &str,
    retention: Duration,
) -> Result<SchedulingProbe, ClusterError> {
    let seconds_until_expiry: Option<f64> = sqlx::query_scalar(AssertSqlSafe(format!(
        "SELECT extract(epoch FROM \
             (min(expires_at) + make_interval(secs => $1) - now()))::float8 FROM {table}"
    )))
    .bind(retention.as_secs_f64())
    .fetch_one(pool)
    .await
    .map_err(map_sqlx_error)?;
    Ok(SchedulingProbe {
        seconds_until_expiry,
    })
}

/// How long to sleep before the next sweep: the earlier of the next metrics tick
/// (`until_metrics`) and the next row's deadline, with the latter floored at
/// `floor` (see [`wake_floor`]). See the module doc's wake schedule.
fn next_delay(
    until_metrics: Duration,
    seconds_until_expiry: Option<f64>,
    floor: Duration,
) -> Duration {
    match seconds_until_expiry {
        Some(secs) if secs.is_finite() => {
            // `max(0.0)` covers an already-due row (negative seconds); the
            // `try_from` fallback covers a deadline so distant it overflows a
            // `Duration` — either way the metrics cap below still applies.
            let due_in = Duration::try_from_secs_f64(secs.max(0.0)).unwrap_or(until_metrics);
            until_metrics.min(due_in.max(floor))
        }
        // No rows (`None`), or a value there is nothing sensible to derive a
        // deadline from (a non-finite epoch, which `extract` does not produce):
        // nothing to wake early for.
        _ => until_metrics,
    }
}

/// Counts the **live** rows in `cluster_lock` — the cluster-wide number of
/// distinct lock names currently held (DESIGN.md §8), the value behind the
/// `cluster_postgres_lock_active_names` gauge. Deliberately **not**
/// `local_holders.len()`, which is only this instance's slice of the fleet's
/// locks.
///
/// The `expires_at > now()` filter arrived with the retention window and is not
/// cosmetic: a lapsed row now lingers for a whole window, so a plain `count(*)`
/// would report every lock name used in the last hour as *held*. That inflates a
/// gauge operators alert on and fires `cluster.lock.name_cardinality_high` on a
/// deployment holding nothing at all. Held means live, which is the same
/// predicate acquire uses.
async fn active_name_count(pool: &PgPool, table: &str) -> Result<i64, ClusterError> {
    sqlx::query_scalar(AssertSqlSafe(format!(
        "SELECT count(*) FROM {table} WHERE expires_at > now()"
    )))
    .fetch_one(pool)
    .await
    .map_err(map_sqlx_error)
}

/// Spawns the lock TTL reaper.
///
/// `interval` is the metrics cadence and the upper bound on how long this task
/// sleeps; an imminent `expires_at` shortens an individual sleep (module doc,
/// "Wake schedule").
///
/// Besides the TTL sweep, every wake records the reaper-sweep-duration
/// histogram, and each `interval`-boundary wake records the
/// `cluster_postgres_lock_active_names` gauge and logs
/// `cluster.lock.name_cardinality_high` (WARN) while the distinct-name count is
/// over `warn_threshold` (DESIGN.md §8). Tying the WARN to the interval-boundary
/// wake rather than to every sweep is what keeps it rate-limited to once per
/// interval now that expiry-driven wakes can land in between.
///
/// `synchronous_commit = on` needs no re-assertion here or anywhere else: every
/// statement this task issues goes through the write pool, whose
/// `after_connect`/`before_acquire` hooks enforce it on each checkout
/// (DESIGN.md §3.4). This plugin opens no long-lived connection for locks at all
/// now that the beacon is gone.
pub(super) fn spawn_lock_reaper(
    ctx: ReaperContext,
    interval: Duration,
    metrics: LockReaperMetrics,
    warn_threshold: i64,
    wakeup: ReaperWakeup,
) -> tokio::task::JoinHandle<()> {
    let ReaperWakeup {
        deadline_hint,
        cancel,
    } = wakeup;
    let floor = wake_floor(interval);
    tokio::spawn(async move {
        // Due immediately, so the first iteration sweeps and records the gauge
        // right away rather than after one full `interval`.
        let mut metrics_due = tokio::time::Instant::now();
        // Whether the last notify-queue sample failed — see `sample_notify_queue`
        // for why a persistent failure must not log once per interval forever.
        let mut notify_queue_failing = false;
        loop {
            let (errors, provider) = metrics.errors();
            let started = tokio::time::Instant::now();
            let swept = sweep(&ctx, &cancel).await;
            metrics.record_sweep_duration(started.elapsed().as_secs_f64());

            // Shutdown observed (possibly *during* the sweep, which bails between
            // batches): leave now rather than running the gauge and next-deadline
            // queries against a pool `stop()` is about to close — those would fail
            // and emit `cluster.provider.error` ERRORs on every clean shutdown.
            if cancel.is_cancelled() {
                break;
            }

            // On a failed sweep, skip both the gauge and the next-expiry probe
            // (the same backend is almost certainly still unhealthy) and wait out
            // the metrics cadence before trying again.
            let delay = if let Err(err) = swept {
                observability::emit_provider_error(
                    errors,
                    provider,
                    "reaper_sweep",
                    ResourceId::Name(&ctx.table),
                    &err,
                );
                metrics_due.saturating_duration_since(tokio::time::Instant::now())
            } else {
                let interval_wake = tokio::time::Instant::now() >= metrics_due;
                if interval_wake {
                    metrics_due = tokio::time::Instant::now() + interval;
                    // Distinct held names = rows whose lease is still live. Not
                    // `count(*)`: a lapsed row is retained for the fence window,
                    // so counting rows would report an hour of history as held.
                    match active_name_count(&ctx.pool, &ctx.table).await {
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
                            ResourceId::Name(&ctx.table),
                            &err,
                        ),
                    }
                    // Interval-cadence only — see this function's doc.
                    // Interval-cadence, like the gauge above: the notify queue is a
                    // cluster-wide resource whose exhaustion breaks every database
                    // on the server, and this is the only signal that names the
                    // cause rather than a downstream victim (§11).
                    notify_queue_failing =
                        sample_notify_queue(&ctx, &metrics, notify_queue_failing).await;
                }

                let until_metrics =
                    metrics_due.saturating_duration_since(tokio::time::Instant::now());
                match probe_schedule(&ctx.pool, &ctx.table, ctx.fence_retention).await {
                    Ok(probe) => next_delay(until_metrics, probe.seconds_until_expiry, floor),
                    // Can't tell when the next lock is due; fall back to the
                    // fixed cadence, which is the pre-existing behaviour.
                    Err(err) => {
                        observability::emit_provider_error(
                            errors,
                            provider,
                            "reaper_next_expiry",
                            ResourceId::Name(&ctx.table),
                            &err,
                        );
                        until_metrics
                    }
                }
            };

            // Anti-spin, applied only where spinning is actually possible: a
            // *zero* delay. A sweep that overran `interval`, or the unhealthy-
            // backend path above, leaves `metrics_due` already elapsed, and
            // sleeping zero there would turn this loop into a hot retry against
            // the very database that is already struggling.
            //
            // Deliberately not `delay.max(floor)`, which is what this used to be.
            // `next_delay` has already capped `delay` at `until_metrics`, so
            // re-flooring it afterwards could push the metrics/gauge tick out by
            // up to a floor — contradicting both `next_delay`'s own cap and the
            // unit test asserting it
            // (`next_delay_never_outlasts_the_metrics_cadence_even_at_the_floor`).
            // A non-zero sub-floor delay is a tick that is genuinely due that
            // soon, and it self-corrects: the next iteration pushes `metrics_due`
            // out by a full `interval`.
            let delay = if delay.is_zero() { floor } else { delay };
            tokio::select! {
                () = cancel.cancelled() => break,
                () = tokio::time::sleep(delay) => {}
                // A local acquire/renew just wrote a deadline this sleep was
                // computed without. Re-sweep, but only after the wake floor:
                // `Notify` coalesces concurrent signals into a single permit, so
                // the floor is what stops a *stream* of acquisitions from
                // turning every one of them into its own sweep.
                () = deadline_hint.notified() => {
                    tokio::select! {
                        () = cancel.cancelled() => break,
                        () = tokio::time::sleep(floor) => {}
                    }
                }
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn next_delay_falls_back_to_the_metrics_cadence_with_no_locks() {
        // Empty table: nothing can expire, so the only thing left to wake for is
        // the gauge/WARN tick.
        assert_eq!(
            next_delay(Duration::from_secs(5), None, MIN_WAKE),
            Duration::from_secs(5)
        );
    }

    #[test]
    fn next_delay_shortens_to_an_imminent_deadline() {
        // A lock due in 800ms must not wait out the full 5s interval — this is
        // the whole point of reading `min(expires_at)`.
        assert_eq!(
            next_delay(Duration::from_secs(5), Some(0.8), MIN_WAKE),
            Duration::from_millis(800)
        );
    }

    #[test]
    fn next_delay_caps_a_distant_deadline_at_the_metrics_cadence() {
        // A deadline an hour out must not silence the `active_names` gauge for an
        // hour: the row count moves with every acquire/release, not just expiry.
        assert_eq!(
            next_delay(Duration::from_secs(5), Some(3600.0), MIN_WAKE),
            Duration::from_secs(5)
        );
    }

    #[test]
    fn next_delay_floors_an_already_due_deadline() {
        // Negative seconds mean a row is already past its deadline but survived
        // this sweep (another reaper's `SKIP LOCKED` claim, or a batch boundary).
        // Re-sweeping must be floored, not immediate, or the task spins.
        assert_eq!(
            next_delay(Duration::from_secs(5), Some(-2.0), MIN_WAKE),
            MIN_WAKE
        );
        assert_eq!(
            next_delay(Duration::from_secs(5), Some(0.0), MIN_WAKE),
            MIN_WAKE
        );
    }

    #[test]
    fn next_delay_floors_a_deadline_inside_the_wake_floor() {
        // Many locks with staggered sub-floor deadlines would otherwise wake this
        // task once per deadline; the floor coalesces them.
        assert_eq!(
            next_delay(Duration::from_secs(5), Some(0.001), MIN_WAKE),
            MIN_WAKE
        );
    }

    #[test]
    fn next_delay_never_outlasts_the_metrics_cadence_even_at_the_floor() {
        // The metrics cap wins over the floor: with the tick due sooner than
        // `MIN_WAKE`, the gauge is not pushed out by up to a floor's worth.
        let until_metrics = Duration::from_millis(10);
        assert_eq!(
            next_delay(until_metrics, Some(0.0), MIN_WAKE),
            until_metrics
        );
    }

    #[test]
    fn wake_floor_never_overrides_a_shorter_configured_interval() {
        // A configured cadence below MIN_WAKE is an explicit request, not churn:
        // flooring it at 100ms would silently halve a 50ms reaper's rate.
        assert_eq!(
            wake_floor(Duration::from_millis(50)),
            Duration::from_millis(50)
        );
        assert_eq!(wake_floor(Duration::from_secs(5)), MIN_WAKE);
    }

    #[test]
    fn next_delay_honours_a_floor_below_min_wake() {
        // With a 50ms interval the floor is 50ms, so a due-now deadline re-sweeps
        // at that cadence rather than being stretched to MIN_WAKE.
        let floor = wake_floor(Duration::from_millis(50));
        assert_eq!(
            next_delay(Duration::from_millis(50), Some(0.0), floor),
            Duration::from_millis(50)
        );
    }

    #[test]
    fn next_delay_survives_an_unrepresentable_deadline() {
        // A deadline beyond `Duration`'s range (a hand-inserted row with an
        // absurd `expires_at`) must not panic `Duration::from_secs_f64`.
        assert_eq!(
            next_delay(Duration::from_secs(5), Some(f64::MAX), MIN_WAKE),
            Duration::from_secs(5)
        );
        assert_eq!(
            next_delay(Duration::from_secs(5), Some(f64::NAN), MIN_WAKE),
            Duration::from_secs(5)
        );
    }
}
