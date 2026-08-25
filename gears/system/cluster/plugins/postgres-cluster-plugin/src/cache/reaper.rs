//! The `cluster_cache` TTL sweeper background task (DESIGN.md §4.2).
//!
//! Wakes on a configurable interval and deletes every expired entry,
//! `NOTIFY`-ing `E:<key>` for each so watchers receive
//! `CacheWatchEvent::Event(CacheEvent::Expired { key })`. Driven by a
//! [`CancellationToken`]; self-terminates when cancelled.
//!
//! Takes no [`WatchRegistry`](crate::cache::watch::WatchRegistry) reference:
//! the `NOTIFY` this sweep issues reaches this same instance's local watchers
//! (and every other instance's) via the dedicated LISTEN connection's normal
//! round-trip, exactly like a `put`/`delete` call — see `cache/watch.rs`'s
//! module doc for why that round-trip is the correct fan-out path, not a
//! shortcut worth bypassing here.
//!
//! ## Chunking
//!
//! A sweep is a *loop* of bounded transactions ([`SWEEP_BATCH`] rows each), not
//! one unbounded `DELETE` — the same shape `lock::reaper` uses, and for two
//! reasons that both scale with backlog size rather than with the configured
//! interval:
//!
//! * **Lock-hold time.** `DELETE ... RETURNING` holds a row lock on every
//!   matching row until commit, and each deleted key costs a sequential
//!   `pg_notify` round-trip before that commit. Unbounded, a concurrent
//!   `put`/`put_if_absent` on a key caught mid-batch does not wait for a quick
//!   row lock — it waits out the *rest of the whole backlog's* `NOTIFY` loop,
//!   on a write-pool connection pinned for that entire span. Chunking caps both
//!   the pinned-connection span and the `NOTIFY` burst at one batch's worth.
//! * **Partial progress.** All-or-nothing means one transient failure anywhere
//!   in the loop rolls back the entire batch, so the tick reclaims *nothing*.
//!   Should the expiry rate ever outpace one sweep, every later tick would
//!   re-attempt the same, larger, still-all-or-nothing batch and never eat into
//!   the backlog. Per-chunk transactions commit as they go, so a failing chunk
//!   costs only its own rows and the ones already committed stay reclaimed.
//!
//! Each chunk keeps its delete and that chunk's `NOTIFY`s in **one**
//! transaction: the atomicity that matters is per-event (never a row deleted
//! with its `Expired` event lost, nor the reverse — DESIGN.md §4.1), and that
//! is preserved by a bounded transaction just as well as by an unbounded one.

use std::sync::Arc;
use std::time::Duration;

use cluster_sdk::observability::fields::label;
use cluster_sdk::observability::{self, ResourceId};
use cluster_sdk::{ClusterError, ClusterMetrics};
use opentelemetry::KeyValue;
use opentelemetry::metrics::Histogram;
use sqlx::AssertSqlSafe;
use sqlx::PgPool;
use tokio_util::sync::CancellationToken;

use crate::cache::watch::{self, NotifyEvent};
use crate::pg_error::map_sqlx_error;

/// Expired rows one sweep transaction claims. The sweep loops until a chunk
/// comes back short, so a backlog of any size is still cleared in full — just
/// across several bounded transactions instead of one that row-locks every
/// expired key for as long as that many `NOTIFY` round-trips take (module doc,
/// "Chunking"). Matches `lock::reaper`'s constant of the same name; kept
/// separate so either primitive can be tuned without moving the other.
const SWEEP_BATCH: usize = 512;

/// The bounded delete one chunk runs.
///
/// The `LIMIT` lives in a subquery because `DELETE` takes no `LIMIT` of its own.
/// `ORDER BY expires_at` reaps longest-expired-first and rides
/// `cluster_cache_expires_idx` (the same partial index the
/// `expires_at IS NOT NULL AND expires_at <= now()` filter uses), so the
/// ordering costs nothing.
///
/// `FOR UPDATE SKIP LOCKED` does double duty, exactly as in `lock::reaper`. It
/// keeps concurrent reapers — one per fleet instance, all sweeping this same
/// table — from serializing behind each other on the same chunk: each skips the
/// rows another has already claimed and takes the next ones. It is also what
/// makes the *outer* delete safe while matching on `key` alone rather than
/// re-checking `expires_at`: a row whose `put` is in flight is already row-locked
/// by that `INSERT ... ON CONFLICT (key) DO UPDATE`, so this statement skips it
/// instead of deleting an entry in the act of being rewritten. (A `put` that
/// commits *before* the subquery runs is excluded by the `expires_at` filter; one
/// that arrives *after* blocks on this statement's row lock, then finds the row
/// gone and inserts fresh — the same reaper-wins-then-put-recreates outcome the
/// unbounded single-statement delete had, `Expired` event included.)
fn chunk_sql(table: &str) -> String {
    format!(
        "DELETE FROM {table} WHERE key IN (\
             SELECT key FROM {table} \
             WHERE expires_at IS NOT NULL AND expires_at <= now() \
             ORDER BY expires_at LIMIT {SWEEP_BATCH} FOR UPDATE SKIP LOCKED\
         ) RETURNING key"
    )
}

/// Runs one chunk: deletes up to [`SWEEP_BATCH`] rows past their `expires_at`
/// and issues one `NOTIFY` per deleted key in the same transaction (DESIGN.md
/// §4.2). Returns how many rows it reaped — a count below [`SWEEP_BATCH`] means
/// the table had nothing more to give.
async fn sweep_chunk(pool: &PgPool, table: &str) -> Result<usize, ClusterError> {
    let mut tx = pool.begin().await.map_err(map_sqlx_error)?;
    let expired_keys: Vec<String> = sqlx::query_scalar(AssertSqlSafe(chunk_sql(table)))
        .fetch_all(&mut *tx)
        .await
        .map_err(map_sqlx_error)?;

    // One statement for the whole chunk, not one per key. Every one of those
    // round-trips ran *inside* this transaction, so it added directly to the span
    // over which the chunk's `DELETE ... RETURNING` holds a row lock on each key
    // it claimed — the very cost the chunking exists to bound (module doc,
    // "Chunking"). Mirrors `lock::notify::notify_released_many`, which the lock
    // sweep has always used for exactly this reason.
    if !expired_keys.is_empty() {
        watch::notify_many(&mut *tx, NotifyEvent::Expired, &expired_keys).await?;
    }

    tx.commit().await.map_err(map_sqlx_error)?;
    Ok(expired_keys.len())
}

/// Runs one sweep to completion, chunk by chunk, returning the number of rows
/// reaped.
///
/// A failing chunk ends the sweep rather than skipping ahead: its rows are not
/// row-locked by anyone, so the very next chunk's subquery would re-select the
/// same ones and retry them in a tight loop. Ending here instead leaves every
/// already-committed chunk reclaimed and lets the next tick resume from there —
/// which is the incremental progress the whole chunking exists for.
///
/// Re-checks `cancel` between chunks so a shutdown arriving mid-backlog is not
/// held up for as many transactions as the backlog needs — the rows left behind
/// are still expired, so the next reaper to run (here or on another instance)
/// picks them up.
pub(super) async fn sweep(
    pool: &PgPool,
    table: &str,
    cancel: &CancellationToken,
) -> Result<usize, ClusterError> {
    let mut reaped = 0;
    loop {
        let chunk = sweep_chunk(pool, table).await?;
        reaped += chunk;
        if chunk < SWEEP_BATCH || cancel.is_cancelled() {
            return Ok(reaped);
        }
    }
}

/// Spawns the cache TTL reaper.
///
/// Each tick records `cluster_postgres_reaper_sweep_duration_seconds` with
/// `primitive = "cache"` into the shared `sweep_duration` histogram (DESIGN.md
/// §8, built by `lock::reaper::sweep_duration_histogram`). The cache reaper has
/// no gauge counterpart — only the lock reaper emits
/// `cluster_postgres_lock_active_names`.
pub fn spawn_cache_reaper(
    pool: PgPool,
    table: String,
    interval: Duration,
    sweep_duration: Histogram<f64>,
    metrics: Arc<dyn ClusterMetrics>,
    provider: &'static str,
    cancel: CancellationToken,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(interval);
        // A chunked sweep of a real backlog can outrun `interval`, and the
        // default `Burst` would then fire every missed tick back-to-back with no
        // delay — hammering a database that is, by definition, already behind.
        // `Delay` restarts the cadence from when the long sweep actually
        // finished. Nothing is lost by not catching up: the sweep already loops
        // until the backlog is drained, so a missed tick has no leftover work to
        // rush back for — and if the sweep ended on an *error*, waiting out an
        // interval is exactly right rather than hot-retrying a struggling
        // backend (the same reasoning as `lock::reaper`'s wake floor).
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                () = cancel.cancelled() => break,
                _ = tick.tick() => {
                    let started = tokio::time::Instant::now();
                    let swept = sweep(&pool, &table, &cancel).await;
                    sweep_duration.record(
                        started.elapsed().as_secs_f64(),
                        &[
                            KeyValue::new(label::PROVIDER, provider),
                            KeyValue::new(label::PRIMITIVE, "cache"),
                        ],
                    );
                    if let Err(err) = swept {
                        // Route through the shared signal path (ERROR log +
                        // `cluster_provider_errors_total`), not a free-text WARN
                        // (PGR-M1 / OBSERVABILITY.md §6). A whole-table sweep has
                        // no single resource key, so the `cluster_cache` table is
                        // the `name` resource field.
                        observability::emit_provider_error(
                            &*metrics,
                            provider,
                            "reaper_sweep",
                            ResourceId::Name(&table),
                            &err,
                        );
                    }
                }
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A canary, not a formatting assertion: the unbounded `DELETE` this
    /// statement replaced is precisely the regression being guarded against, and
    /// both of these clauses are load-bearing (module doc, "Chunking") while
    /// being invisible in any unit-testable behaviour.
    #[test]
    fn the_chunk_delete_is_bounded_and_skips_claimed_rows() {
        let sql = chunk_sql("public.cluster_cache");
        assert!(
            sql.contains(&format!("LIMIT {SWEEP_BATCH}")),
            "the chunk delete must stay bounded, got: {sql}"
        );
        assert!(
            sql.contains("FOR UPDATE SKIP LOCKED"),
            "concurrent reapers must not serialize on the same chunk, got: {sql}"
        );
        // Without this the sweep would also delete indefinite (`NULL`) entries.
        assert!(
            sql.contains("expires_at IS NOT NULL AND expires_at <= now()"),
            "the chunk delete must only claim expired-with-a-TTL rows, got: {sql}"
        );
    }
}
