//! `read`, `resolve`, `query` (`docs/DESIGN.md` §4.5, §4.6, §4.8) and the
//! current heads a watch starts from (§4.7).

use std::collections::HashMap;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use event_broker_sdk::backend::{BackendError, Head, TopicPartition, Wait};
use event_broker_sdk::models::{Event, PartitionRange, TopicSegment};
use event_broker_sdk::{Position, Sequence};
use sqlx::AssertSqlSafe;
use uuid::Uuid;

use crate::error::Operation;
use crate::heads::{self, Key};
use crate::shared::Shared;
use crate::{body, error, names};

/// One stored event.
#[derive(sqlx::FromRow)]
struct StoredRow {
    seq: i64,
    seq_time: DateTime<Utc>,
    body: Vec<u8>,
}

/// First and last sequence and storage time of a matched range; all `None`
/// when it matched nothing.
#[derive(sqlx::FromRow)]
struct RangeFigures {
    first: Option<i64>,
    last: Option<i64>,
    start_time: Option<DateTime<Utc>>,
    end_time: Option<DateTime<Utc>>,
}

/// The topic's uuid and the partition's table, after checking both exist.
async fn partition_of(
    shared: &Shared,
    at: &TopicPartition,
    operation: Operation,
) -> Result<(Uuid, Arc<str>), BackendError> {
    let known = shared.topic(&at.topic, operation).await?;
    let table = usize::try_from(at.partition.value())
        .ok()
        .and_then(|i| known.tables.get(i))
        .ok_or_else(|| BackendError::PartitionNotFound {
            detail: format!("{operation}: the topic has no such partition on this backend"),
        })?;
    Ok((known.uuid, Arc::clone(table)))
}

/// Up to `max_events` events after `offset`, waiting for one under
/// [`Wait::For`] when none is stored yet.
pub async fn read(
    shared: &Shared,
    at: &TopicPartition,
    offset: Sequence,
    max_events: usize,
    wait: Wait,
) -> Result<Vec<Event>, BackendError> {
    // Resolved once: a waiting read probes again on every wake.
    let (topic, table) = partition_of(shared, at, Operation::Read).await?;
    let probe = || read_now(shared, at, topic, &table, offset, max_events);
    match wait {
        Wait::No => probe().await,
        Wait::For(wait) => {
            let poll = shared.settings.poll_interval.min(wait);
            shared
                .heads
                .read_waiting(at, offset, wait, poll, probe)
                .await
        }
    }
}

async fn read_now(
    shared: &Shared,
    at: &TopicPartition,
    topic: Uuid,
    table: &str,
    offset: Sequence,
    max_events: usize,
) -> Result<Vec<Event>, BackendError> {
    // Expired segments stay until the maintenance drops them; the floor
    // hides them from the commit that expired them. One statement, so the
    // floor and the rows are read in one snapshot, and no DROP PARTITION
    // runs between them.
    let pool = shared.pool()?;
    let rows: Vec<StoredRow> = error::retried(|| {
        sqlx::query_as(AssertSqlSafe(format!(
        "SELECT seq, seq_time, body FROM {table} \
         WHERE seq > GREATEST(?, (SELECT floor_seq FROM evbk__retention WHERE topic_uuid = ? AND partition_no = ?)) \
         ORDER BY seq LIMIT ?"
    )))
    .bind(offset.as_i64())
    .bind(topic)
    .bind(names::partition_column(at.partition))
    .bind(i64::try_from(max_events).unwrap_or(i64::MAX))
    .fetch_all(pool)
    })
    .await
    .map_err(|e| error::map(Operation::Read, &e))?;
    rows.into_iter()
        .map(|row| body::decode(&row.body, at.partition, row.seq, row.seq_time))
        .collect()
}

/// The sequence `position` names, checked against the partition's floor and
/// head.
pub async fn resolve(
    shared: &Shared,
    at: &TopicPartition,
    position: &Position,
) -> Result<Sequence, BackendError> {
    const OP: Operation = Operation::Resolve;
    let (topic, table) = partition_of(shared, at, OP).await?;
    let pool = shared.pool()?;
    let map = |e| error::map(OP, &e);
    // One snapshot of both rows: the head, and the highest removed sequence,
    // which is one below the oldest stored event or the head when nothing is
    // stored.
    let (head, floor): (i64, i64) = error::retried(|| {
        sqlx::query_as(
            "SELECT p.hwm, r.floor_seq FROM evbk__partition p \
         JOIN evbk__retention r ON r.topic_uuid = p.topic_uuid AND r.partition_no = p.partition_no \
         WHERE p.topic_uuid = ? AND p.partition_no = ?",
        )
        .bind(topic)
        .bind(names::partition_column(at.partition))
        .fetch_one(pool)
    })
    .await
    .map_err(map)?;
    let ceiling = Sequence::assigned(head);
    let floor = Sequence::assigned(floor);
    match position {
        Position::Latest => Ok(ceiling),
        Position::Earliest => Ok(floor),
        Position::At(at_time) => {
            // `seq_time` rises with `seq` (`append`), so every event of a
            // sealed segment is no later than its `newest_at`: the scan
            // starts past the last segment sealed before `at_time`, and
            // walks the primary key from there.
            let found: Option<i64> = error::retried(|| {
                sqlx::query_scalar(AssertSqlSafe(format!(
                "SELECT seq FROM {table} WHERE seq_time >= ? AND seq > \
                 GREATEST(?, (SELECT COALESCE(MAX(end_seq - 1), 0) FROM evbk__segment \
                  WHERE topic_uuid = ? AND partition_no = ? AND state = 'sealed' AND newest_at < ?)) \
                 ORDER BY seq LIMIT 1"
            )))
            .bind(at_time)
            .bind(floor.as_i64())
            .bind(topic)
            .bind(names::partition_column(at.partition))
            .bind(at_time)
            .fetch_optional(pool)
            })
            .await
            .map_err(map)?;
            Ok(found.map_or(ceiling, |sequence| Sequence::assigned(sequence - 1)))
        }
        Position::Exact(requested) if (floor..=ceiling).contains(requested) => Ok(*requested),
        Position::Exact(requested) => Err(BackendError::out_of_range(*requested, floor, ceiling)),
    }
}

/// The current head of each of `partitions`, in order, for a watch's first
/// items: one query per 500 partitions.
pub async fn current_heads(
    shared: &Shared,
    partitions: &[TopicPartition],
) -> Result<Vec<Head>, BackendError> {
    const OP: Operation = Operation::Watch;
    let keys = keys_of(shared, partitions).await?;
    let pool = shared.pool()?;
    let found: HashMap<Key, i64> = error::retried(|| heads::hwms(pool, &keys))
        .await
        .map_err(|e| error::map(OP, &e))?
        .into_iter()
        .collect();
    partitions
        .iter()
        .zip(&keys)
        .map(|(at, key)| {
            let hwm = found.get(key).copied().ok_or_else(|| {
                BackendError::Internal(format!("{OP}: a named partition has no row"))
            })?;
            Ok(Head {
                at: at.clone(),
                sequence: Sequence::assigned(hwm),
            })
        })
        .collect()
}

/// The row keys of `partitions`, in order, after checking each exists.
pub async fn keys_of(
    shared: &Shared,
    partitions: &[TopicPartition],
) -> Result<Vec<Key>, BackendError> {
    let mut keys = Vec::with_capacity(partitions.len());
    for at in partitions {
        keys.push((
            partition_of(shared, at, Operation::Watch).await?.0,
            at.partition,
        ));
    }
    Ok(keys)
}

/// Segment introspection (feature 0003): the matched range's figures from the
/// table, and the live segments from the catalog.
pub async fn query(
    shared: &Shared,
    at: &TopicPartition,
    range: &PartitionRange,
) -> Result<Vec<TopicSegment>, BackendError> {
    const OP: Operation = Operation::Query;
    let (topic, table) = partition_of(shared, at, OP).await?;
    let pool = shared.pool()?;
    let map = |e| error::map(OP, &e);
    let figures: RangeFigures = error::retried(|| {
        sqlx::query_as(AssertSqlSafe(format!(
        "SELECT MIN(seq) AS first, MAX(seq) AS last, MIN(seq_time) AS start_time, \
         MAX(seq_time) AS end_time FROM \
         (SELECT seq, seq_time FROM {table} WHERE seq <= ? AND seq >= \
          GREATEST(?, (SELECT floor_seq + 1 FROM evbk__retention WHERE topic_uuid = ? AND partition_no = ?)) \
          ORDER BY seq LIMIT ?) AS matched"
    )))
    .bind(range.end_offset.map_or(i64::MAX, Sequence::as_i64))
    .bind(range.start_offset.map_or(i64::MIN, Sequence::as_i64))
    .bind(topic)
    .bind(names::partition_column(at.partition))
    .bind(i64::from(range.limit))
    .fetch_one(pool)
    })
    .await
    .map_err(map)?;
    let (Some(first), Some(last)) = (figures.first, figures.last) else {
        return Ok(Vec::new());
    };
    let segments: Vec<(String, i64, i64)> = error::retried(|| {
        sqlx::query_as(
        "SELECT segment_name, first_seq, end_seq FROM evbk__segment \
         WHERE topic_uuid = ? AND partition_no = ? AND state IN ('open', 'sealed') ORDER BY first_seq",
    )
    .bind(topic)
    .bind(names::partition_column(at.partition))
    .fetch_all(pool)
    })
    .await
    .map_err(map)?;
    Ok(vec![TopicSegment {
        topic: at.topic.to_string(),
        partition: at.partition.value(),
        start_sequence: Sequence::assigned(first),
        end_sequence: Sequence::assigned(last),
        start_time: figures.start_time,
        end_time: figures.end_time,
        segments: segments
            .into_iter()
            .map(|(name, from, to)| {
                serde_json::json!({ "name": name, "from_sequence": from, "to_sequence": to - 1 })
            })
            .collect(),
    }])
}
