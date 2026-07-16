//! The `cluster_cache` TTL sweeper background task (DESIGN.md §4.2).
//!
//! Wakes on a configurable interval and deletes all expired entries,
//! `NOTIFY`-ing `E:<key>` for each so watchers receive
//! `CacheWatchEvent::Event(CacheEvent::Expired { key })`. Driven by a
//! [`CancellationToken`]; self-terminates when cancelled. Uses one connection
//! from the write pool per sweep, releasing it immediately after.
//!
//! Takes no [`WatchRegistry`](crate::cache::watch::WatchRegistry) reference:
//! the `NOTIFY` this sweep issues reaches this same instance's local watchers
//! (and every other instance's) via the dedicated LISTEN connection's normal
//! round-trip, exactly like a `put`/`delete` call — see `cache/watch.rs`'s
//! module doc for why that round-trip is the correct fan-out path, not a
//! shortcut worth bypassing here.

use std::sync::Arc;
use std::time::Duration;

use cluster_sdk::observability::fields::label;
use cluster_sdk::observability::{self, ResourceId};
use cluster_sdk::{ClusterError, ClusterMetrics};
use opentelemetry::KeyValue;
use opentelemetry::metrics::Histogram;
use sqlx::PgPool;
use tokio_util::sync::CancellationToken;

use crate::cache::watch::{self, NotifyEvent};
use crate::pg_error::map_sqlx_error;

/// Runs one sweep: deletes all rows past their `expires_at` and issues one
/// `NOTIFY` per deleted key in the same transaction (DESIGN.md §4.2).
async fn sweep(pool: &PgPool, table: &str) -> Result<usize, ClusterError> {
    let mut tx = pool.begin().await.map_err(map_sqlx_error)?;
    let expired_keys: Vec<String> = sqlx::query_scalar(&format!(
        "DELETE FROM {table} WHERE expires_at IS NOT NULL AND expires_at <= now() RETURNING key"
    ))
    .fetch_all(&mut *tx)
    .await
    .map_err(map_sqlx_error)?;

    for key in &expired_keys {
        watch::notify(&mut *tx, NotifyEvent::Expired, key).await?;
    }

    tx.commit().await.map_err(map_sqlx_error)?;
    Ok(expired_keys.len())
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
        loop {
            tokio::select! {
                () = cancel.cancelled() => break,
                _ = tick.tick() => {
                    let started = tokio::time::Instant::now();
                    let swept = sweep(&pool, &table).await;
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
