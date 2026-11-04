//! The retention sweep (`docs/DESIGN.md` §4.8): every `sweep.interval_ms`, for
//! each partition with a bound and anything stored, the floor is raised past
//! the expired events and they are deleted - one write transaction of at most
//! `sweep.batch` events at a time, oldest first - and the freed pages are
//! handed back to the file system.

use std::sync::Arc;

use chrono::Utc;
use event_broker_sdk::backend::BackendError;
use sqlx::SqlitePool;
use tokio::time::MissedTickBehavior;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;
use uuid::Uuid;

use crate::clock;
use crate::error::{self, Operation};
use crate::hub::Hub;
use crate::shared::Shared;

const OP: Operation = Operation::Sweep;

/// Pages one pass hands back at most, so the write lock is not held for long.
const VACUUM_PAGES: u32 = 2048;

/// One bounded partition, as a pass finds it: its bounds, and what it holds
/// against them.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Bounded {
    /// The topic the partition belongs to.
    pub topic_uuid: Uuid,
    /// The partition number.
    pub partition: i64,
    /// The age bound in milliseconds; `None` with no age bound.
    pub max_age_ms: Option<i64>,
    /// The size bound in body bytes; `None` with no size bound.
    pub max_bytes: Option<i64>,
    /// Body bytes stored.
    pub bytes: i64,
    /// `sequence_time` of the oldest stored event; `None` when none is.
    pub oldest_time: Option<i64>,
}

impl Bounded {
    /// Whether anything is past a bound at `now` (microseconds): only then
    /// does the pass take the write lock for the partition.
    pub fn crossed(&self, now: i64) -> bool {
        let aged = self
            .max_age_ms
            .zip(self.oldest_time)
            .is_some_and(|(age, oldest)| oldest < now.saturating_sub(age.saturating_mul(1000)));
        let full = self.max_bytes.is_some_and(|max| self.bytes > max);
        aged || full
    }
}

/// Starts the sweep on `tasks`; it ends when `stop` fires.
pub fn spawn(
    shared: &Shared,
    pool: SqlitePool,
    hub: Arc<Hub>,
    stop: &CancellationToken,
    tasks: &TaskTracker,
) {
    let interval = shared.settings.sweep.interval;
    let batch = i64::from(shared.settings.sweep.batch);
    let stop = stop.clone();
    tasks.spawn(async move {
        let mut ticker = tokio::time::interval(interval);
        ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                () = stop.cancelled() => return,
                _ = ticker.tick() => {}
            }
            let pass = tokio::select! {
                () = stop.cancelled() => return,
                pass = pass(&pool, &hub, batch) => pass,
            };
            if let Err(problem) = pass {
                tracing::warn!(%problem, "sqlite event-broker backend: sweep pass failed; retried next interval");
            }
        }
    });
}

/// One pass over every bounded partition.
async fn pass(pool: &SqlitePool, hub: &Hub, batch: i64) -> Result<(), BackendError> {
    let map = |e| error::map(OP, &e);
    // Read-only, outside the write lock: the oldest time is one seek on
    // `evbk__event_time` per partition.
    let bounded: Vec<Bounded> = sqlx::query_as(
        "SELECT p.topic_uuid, p.partition, l.max_age_ms, l.max_bytes, p.bytes, \
         (SELECT MIN(e.sequence_time) FROM evbk__event e \
          WHERE e.topic_uuid = p.topic_uuid AND e.partition = p.partition) AS oldest_time \
         FROM evbk__partition p JOIN evbk__log l ON l.topic_uuid = p.topic_uuid \
         WHERE p.floor < p.hwm AND (l.max_age_ms IS NOT NULL OR l.max_bytes IS NOT NULL)",
    )
    .fetch_all(pool)
    .await
    .map_err(map)?;
    let now = clock::micros(Utc::now());
    let mut removed = 0;
    for partition in bounded.iter().filter(|p| p.crossed(now)) {
        loop {
            let chunk = expire(pool, hub, partition, batch).await?;
            removed += chunk;
            if chunk < batch {
                break;
            }
        }
    }
    if removed > 0 {
        let _writer = hub.writer.lock().await;
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "PRAGMA incremental_vacuum({VACUUM_PAGES})"
        )))
        .execute(pool)
        .await
        .map_err(map)?;
    }
    Ok(())
}

/// Removes up to `batch` expired events of `partition` in one write
/// transaction; answers how many.
async fn expire(
    pool: &SqlitePool,
    hub: &Hub,
    partition: &Bounded,
    batch: i64,
) -> Result<i64, BackendError> {
    let map = |e| error::map(OP, &e);
    let _writer = hub.writer.lock().await;
    let mut tx = pool.begin_with("BEGIN IMMEDIATE").await.map_err(map)?;
    let (floor, hwm, bytes): (i64, i64, i64) = sqlx::query_as(
        "SELECT floor, hwm, bytes FROM evbk__partition WHERE topic_uuid = ?1 AND partition = ?2",
    )
    .bind(partition.topic_uuid)
    .bind(partition.partition)
    .fetch_one(&mut *tx)
    .await
    .map_err(map)?;

    let mut cut = floor;
    if let Some(max_age_ms) = partition.max_age_ms {
        let before = clock::micros(Utc::now()).saturating_sub(max_age_ms.saturating_mul(1000));
        // `sequence_time` never falls with `sequence`: the last event stored
        // before the cutoff is the highest expired sequence.
        let expired: Option<i64> = sqlx::query_scalar(
            "SELECT sequence FROM evbk__event WHERE topic_uuid = ?1 AND partition = ?2 \
             AND sequence_time < ?3 ORDER BY sequence_time DESC, sequence DESC LIMIT 1",
        )
        .bind(partition.topic_uuid)
        .bind(partition.partition)
        .bind(before)
        .fetch_optional(&mut *tx)
        .await
        .map_err(map)?;
        cut = cut.max(expired.unwrap_or(floor));
    }
    if let Some(max_bytes) = partition.max_bytes
        && bytes > max_bytes
    {
        let oldest: Vec<(i64, i64)> = sqlx::query_as(
            "SELECT sequence, size FROM evbk__event WHERE topic_uuid = ?1 AND partition = ?2 \
             AND sequence > ?3 ORDER BY sequence LIMIT ?4",
        )
        .bind(partition.topic_uuid)
        .bind(partition.partition)
        .bind(floor)
        .bind(batch)
        .fetch_all(&mut *tx)
        .await
        .map_err(map)?;
        if let Some(over) = past_bytes(&oldest, bytes - max_bytes) {
            cut = cut.max(over);
        }
    }
    let cut = cut.min(floor.saturating_add(batch)).min(hwm);
    if cut <= floor {
        tx.rollback().await.map_err(map)?;
        return Ok(0);
    }
    let freed: i64 = sqlx::query_scalar(
        "SELECT COALESCE(SUM(size), 0) FROM evbk__event WHERE topic_uuid = ?1 AND partition = ?2 \
         AND sequence > ?3 AND sequence <= ?4",
    )
    .bind(partition.topic_uuid)
    .bind(partition.partition)
    .bind(floor)
    .bind(cut)
    .fetch_one(&mut *tx)
    .await
    .map_err(map)?;
    sqlx::query(
        "DELETE FROM evbk__event WHERE topic_uuid = ?1 AND partition = ?2 AND sequence > ?3 AND sequence <= ?4",
    )
    .bind(partition.topic_uuid)
    .bind(partition.partition)
    .bind(floor)
    .bind(cut)
    .execute(&mut *tx)
    .await
    .map_err(map)?;
    sqlx::query(
        "UPDATE evbk__partition SET floor = ?3, bytes = bytes - ?4 WHERE topic_uuid = ?1 AND partition = ?2",
    )
    .bind(partition.topic_uuid)
    .bind(partition.partition)
    .bind(cut)
    .bind(freed)
    .execute(&mut *tx)
    .await
    .map_err(map)?;
    tx.commit().await.map_err(map)?;
    Ok(cut - floor)
}

/// The sequence up to which the oldest events `(sequence, size)` must go to
/// shed `excess` bytes: the first at which their sizes add up to it, or the
/// last given when they do not reach it.
pub fn past_bytes(oldest: &[(i64, i64)], excess: i64) -> Option<i64> {
    let mut shed = 0_i64;
    for &(sequence, size) in oldest {
        shed = shed.saturating_add(size);
        if shed >= excess {
            return Some(sequence);
        }
    }
    oldest.last().map(|&(sequence, _)| sequence)
}
