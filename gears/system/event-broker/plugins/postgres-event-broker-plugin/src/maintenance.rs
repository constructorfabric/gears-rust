//! The maintenance worker (`docs/DESIGN.md` §4.9): keeps a segment ahead of
//! every partition's head and removes expired events, ageing them by
//! `sequence_time` - never by the producer's `occurred_at`.
//!
//! Every instance that builds the registry entry runs one; a pass over a
//! partition holds a session advisory lock on a pooled connection and skips
//! the partition when another instance holds it. The lock is a session one
//! because `DETACH PARTITION ... CONCURRENTLY` runs outside a transaction.

use std::sync::Arc;

use chrono::{DateTime, Utc};
use event_broker_sdk::backend::Partition;
use sqlx::pool::PoolConnection;
use sqlx::{AssertSqlSafe, Postgres};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::names;
use crate::read::segments_of;
use crate::segment::{self, Observed};
use crate::shared::Shared;

/// The partition's floor, `$1`/`$2` being its topic and partition. Rows at or
/// below it are already counted as removed, even while a segment holding them
/// waits to be dropped.
const FLOOR: &str = "SELECT floor FROM evbk__retention WHERE topic_uuid = $1 AND partition = $2";

/// Records a `removed (sequence, stored_bytes)` prefix: the floor moves to
/// its last sequence and its bytes count as removed.
const ADVANCE: &str = "UPDATE evbk__retention SET \
     floor = GREATEST(floor, (SELECT MAX(sequence) FROM removed)), \
     removed_bytes = removed_bytes + (SELECT COALESCE(SUM(stored_bytes), 0) FROM removed) \
     WHERE topic_uuid = $1 AND partition = $2";

pub fn spawn(shared: Arc<Shared>, stop: CancellationToken) {
    tokio::spawn(async move {
        loop {
            pass(&shared).await;
            tokio::select! {
                () = stop.cancelled() => return,
                () = tokio::time::sleep(shared.settings.maintenance_interval) => {}
                () = shared.maintenance.notified() => {}
            }
        }
    });
}

/// `(topic, partition, last_sequence, stored_bytes, max_age seconds, max_bytes)`.
type DueRow = (Uuid, i32, i64, i64, Option<f64>, Option<i64>);

/// `(oldest sequence_time, newest sequence_time, events)` of one segment.
type Observation = (Option<DateTime<Utc>>, Option<DateTime<Utc>>, i64);

/// One partition, as `evbk__partition`, `evbk__retention` and `evbk__log`
/// describe it.
struct Due {
    topic: Uuid,
    partition: Partition,
    last_sequence: i64,
    stored_bytes: i64,
    max_age_secs: Option<f64>,
    max_bytes: Option<i64>,
}

async fn pass(shared: &Shared) {
    let Ok(pool) = shared.pool() else {
        return;
    };
    let rows: Result<Vec<DueRow>, _> = sqlx::query_as(
        "SELECT p.topic_uuid, p.partition, p.last_sequence, p.appended_bytes - r.removed_bytes, \
         EXTRACT(EPOCH FROM l.max_age)::float8, l.max_bytes \
         FROM evbk__partition p JOIN evbk__retention r USING (topic_uuid, partition) \
         JOIN evbk__log l USING (topic_uuid) ORDER BY p.topic_uuid, p.partition",
    )
    .fetch_all(pool)
    .await;
    let rows = match rows {
        Ok(rows) => rows,
        Err(err) => {
            tracing::warn!(%err, "postgres event-broker maintenance: partitions could not be listed");
            return;
        }
    };
    for (topic, partition, last_sequence, stored_bytes, max_age_secs, max_bytes) in rows {
        let Some(partition) = names::partition_from_column(partition) else {
            continue;
        };
        let target = Due {
            topic,
            partition,
            last_sequence,
            stored_bytes,
            max_age_secs,
            max_bytes,
        };
        if let Err(err) = maintain(shared, &target).await {
            tracing::warn!(%err, topic = %topic, partition = %target.partition, "postgres event-broker maintenance pass failed");
        }
    }
}

async fn maintain(shared: &Shared, target: &Due) -> Result<(), sqlx::Error> {
    let pool = shared.pool().map_err(|_| sqlx::Error::PoolClosed)?;
    let mut conn = pool.acquire().await?;
    let key = names::maintenance_key(target.topic, target.partition);
    let held: bool = sqlx::query_scalar("SELECT pg_try_advisory_lock(hashtextextended($1, 0))")
        .bind(&key)
        .fetch_one(&mut *conn)
        .await?;
    if !held {
        return Ok(());
    }
    let outcome = upkeep(shared, &mut conn, target).await;
    sqlx::query("SELECT pg_advisory_unlock(hashtextextended($1, 0))")
        .bind(&key)
        .execute(&mut *conn)
        .await?;
    outcome
}

async fn upkeep(
    shared: &Shared,
    conn: &mut PoolConnection<Postgres>,
    target: &Due,
) -> Result<(), sqlx::Error> {
    let parent = names::parent(target.topic, target.partition);
    let pool = shared.pool().map_err(|_| sqlx::Error::PoolClosed)?;
    let segments = segments_of(pool, &parent).await?;
    ahead(shared, conn, target, &parent, &segments).await?;
    if let Some(max_age_secs) = target.max_age_secs {
        expire(conn, target, &parent, &segments, max_age_secs).await?;
    }
    if let Some(max_bytes) = target.max_bytes
        && target.stored_bytes > max_bytes
    {
        trim_bytes(conn, target, &parent, target.stored_bytes - max_bytes).await?;
    }
    Ok(())
}

/// Creates a segment ahead of the head, once half of the last one is used.
async fn ahead(
    shared: &Shared,
    conn: &mut PoolConnection<Postgres>,
    target: &Due,
    parent: &str,
    segments: &[(String, i64, i64)],
) -> Result<(), sqlx::Error> {
    let Some((last_name, first, end)) = segments.last() else {
        return Ok(());
    };
    if !segment::next_is_due(*first, *end, target.last_sequence) {
        return Ok(());
    }
    let observed: Option<Observation> = sqlx::query_as(AssertSqlSafe(format!(
        "SELECT MIN(sequence_time), MAX(sequence_time), COUNT(*) FROM {last_name}"
    )))
    .fetch_optional(&mut **conn)
    .await?;
    let observed = observed.and_then(|(from, to, events)| {
        let seconds = (to? - from?).to_std().ok()?.as_secs_f64();
        Some(Observed { events, seconds })
    });
    let span = segment::span(shared.settings.segment, observed);
    let next = names::segment(target.topic, target.partition, *end);
    sqlx::raw_sql(AssertSqlSafe(format!(
        "CREATE TABLE IF NOT EXISTS {next} (LIKE {parent} INCLUDING ALL); \
         ALTER TABLE {parent} ATTACH PARTITION {next} FOR VALUES FROM ({end}) TO ({to})",
        to = end + span,
    )))
    .execute(&mut **conn)
    .await?;
    Ok(())
}

/// Drops every expired segment but the last, then deletes the expired prefix
/// of what remains. `evbk__retention` follows in the same transaction as each
/// removal; `evbk__partition` is append's alone, so retention never waits on
/// an append's row lock.
async fn expire(
    conn: &mut PoolConnection<Postgres>,
    target: &Due,
    parent: &str,
    segments: &[(String, i64, i64)],
    max_age_secs: f64,
) -> Result<(), sqlx::Error> {
    let cutoff: DateTime<Utc> = sqlx::query_scalar("SELECT now() - make_interval(secs => $1)")
        .bind(max_age_secs)
        .fetch_one(&mut **conn)
        .await?;
    for (name, _, end) in segments.iter().take(segments.len().saturating_sub(1)) {
        // A segment whose whole range is assigned and whose newest event is
        // expired, or which holds nothing any more.
        if end - 1 > target.last_sequence {
            break;
        }
        let newest: Option<DateTime<Utc>> = sqlx::query_scalar(AssertSqlSafe(format!(
            "SELECT sequence_time FROM {name} ORDER BY sequence DESC LIMIT 1"
        )))
        .fetch_optional(&mut **conn)
        .await?;
        if newest.is_some_and(|newest| newest >= cutoff) {
            break;
        }
        drop_segment(conn, target, parent, name, *end).await?;
    }
    sqlx::query(AssertSqlSafe(format!(
        "WITH removed AS (DELETE FROM {parent} WHERE sequence > ({FLOOR}) AND sequence_time < $3 \
                          RETURNING sequence, stored_bytes) \
         {ADVANCE}"
    )))
    .bind(target.topic)
    .bind(names::partition_column(target.partition))
    .bind(cutoff)
    .execute(&mut **conn)
    .await?;
    Ok(())
}

/// Moves the floor past `segment` (its `end` is exclusive), then detaches it
/// without blocking the parent and drops it. The floor moves first, so a
/// reader never resolves a position into a segment on its way out; the
/// `floor < end - 1` guard keeps a retry after a failed detach from counting
/// the segment's bytes twice.
async fn drop_segment(
    conn: &mut PoolConnection<Postgres>,
    target: &Due,
    parent: &str,
    segment: &str,
    end: i64,
) -> Result<(), sqlx::Error> {
    sqlx::query(AssertSqlSafe(format!(
        "UPDATE evbk__retention SET \
         removed_bytes = removed_bytes + \
             (SELECT COALESCE(SUM(stored_bytes), 0) FROM {segment} WHERE sequence > evbk__retention.floor), \
         floor = $3 - 1 \
         WHERE topic_uuid = $1 AND partition = $2 AND floor < $3 - 1"
    )))
    .bind(target.topic)
    .bind(names::partition_column(target.partition))
    .bind(end)
    .execute(&mut **conn)
    .await?;
    sqlx::raw_sql(AssertSqlSafe(format!(
        "ALTER TABLE {parent} DETACH PARTITION {segment} CONCURRENTLY"
    )))
    .execute(&mut **conn)
    .await?;
    sqlx::raw_sql(AssertSqlSafe(format!("DROP TABLE {segment}")))
        .execute(&mut **conn)
        .await?;
    Ok(())
}

/// Deletes the oldest events until at least `excess` bytes are gone.
async fn trim_bytes(
    conn: &mut PoolConnection<Postgres>,
    target: &Due,
    parent: &str,
    excess: i64,
) -> Result<(), sqlx::Error> {
    sqlx::query(AssertSqlSafe(format!(
        "WITH cut AS (SELECT sequence FROM (SELECT sequence, SUM(stored_bytes) OVER (ORDER BY sequence) AS total \
                      FROM {parent} WHERE sequence > ({FLOOR})) AS running \
                      WHERE total >= $3 ORDER BY sequence LIMIT 1), \
         removed AS (DELETE FROM {parent} WHERE sequence > ({FLOOR}) AND sequence <= (SELECT sequence FROM cut) \
                     RETURNING sequence, stored_bytes) \
         {ADVANCE}"
    )))
    .bind(target.topic)
    .bind(names::partition_column(target.partition))
    .bind(excess)
    .execute(&mut **conn)
    .await?;
    Ok(())
}
