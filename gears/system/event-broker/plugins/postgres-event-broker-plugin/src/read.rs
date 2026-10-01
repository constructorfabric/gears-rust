//! `read`, `resolve`, `query` (`docs/DESIGN.md` §4.5, §4.6, §4.8) and the
//! current heads a watch starts from (§4.7).

use std::time::Duration;

use chrono::{DateTime, Utc};
use event_broker_sdk::backend::{BackendError, Head, TopicPartition, Wait};
use event_broker_sdk::models::{Event, PartitionRange, TopicSegment};
use event_broker_sdk::{Position, Sequence};
use sqlx::AssertSqlSafe;
use uuid::Uuid;

use crate::shared::Shared;
use crate::{body, error, names, segment};

/// `(sequence, sequence_time, body)` of one stored event.
type StoredRow = (i64, DateTime<Utc>, Vec<u8>);

/// First and last sequence and storage time of a matched range.
type RangeFigures = (
    Option<i64>,
    Option<i64>,
    Option<DateTime<Utc>>,
    Option<DateTime<Utc>>,
);

/// How often a waiting read re-reads besides being woken: a head from another
/// instance whose notification was lost still ends the wait.
const WAIT_POLL: Duration = Duration::from_secs(1);

/// The topic's id and the partition's table, after checking both exist.
async fn partition_of(
    shared: &Shared,
    at: &TopicPartition,
    operation: &str,
) -> Result<(Uuid, String), BackendError> {
    let (topic, known) = shared.topic(&at.topic, operation).await?;
    if at.partition.value() >= known.partitions {
        return Err(BackendError::PartitionNotFound {
            detail: format!("{operation}: the topic has no such partition on this backend"),
        });
    }
    Ok((topic, names::parent(topic, at.partition)))
}

pub async fn read(
    shared: &Shared,
    at: &TopicPartition,
    offset: Sequence,
    max_events: usize,
    wait: Wait,
) -> Result<Vec<Event>, BackendError> {
    match wait {
        Wait::No => read_now(shared, at, offset, max_events).await,
        Wait::For(wait) => {
            shared
                .heads
                .read_waiting(at, offset, wait, WAIT_POLL.min(wait), || {
                    read_now(shared, at, offset, max_events)
                })
                .await
        }
    }
}

async fn read_now(
    shared: &Shared,
    at: &TopicPartition,
    offset: Sequence,
    max_events: usize,
) -> Result<Vec<Event>, BackendError> {
    let (_, table) = partition_of(shared, at, "read").await?;
    let rows: Vec<StoredRow> = sqlx::query_as(AssertSqlSafe(format!(
        "SELECT sequence, sequence_time, body FROM {table} WHERE sequence > $1 ORDER BY sequence LIMIT $2"
    )))
    .bind(offset.as_i64())
    .bind(i64::try_from(max_events).unwrap_or(i64::MAX))
    .fetch_all(shared.pool()?)
    .await
    .map_err(|e| error::map("read", &e))?;
    rows.into_iter()
        .map(|(sequence, stored_at, body)| body::decode(&body, at.partition, sequence, stored_at))
        .collect()
}

pub async fn resolve(
    shared: &Shared,
    at: &TopicPartition,
    position: &Position,
) -> Result<Sequence, BackendError> {
    const OP: &str = "resolve";
    let (topic, table) = partition_of(shared, at, OP).await?;
    let pool = shared.pool()?;
    let map = |e| error::map(OP, &e);
    // One snapshot of both rows: the head, and the highest removed sequence,
    // which is one below the oldest stored event or the head when nothing is
    // stored.
    let (head, floor): (i64, i64) = sqlx::query_as(
        "SELECT p.last_sequence, r.floor FROM evbk__partition p \
         JOIN evbk__retention r USING (topic_uuid, partition) \
         WHERE p.topic_uuid = $1 AND p.partition = $2",
    )
    .bind(topic)
    .bind(names::partition_column(at.partition))
    .fetch_one(pool)
    .await
    .map_err(map)?;
    let ceiling = Sequence::assigned(head);
    let floor = Sequence::assigned(floor);
    match position {
        Position::Latest => Ok(ceiling),
        Position::Earliest => Ok(floor),
        Position::At(at_time) => {
            let found: Option<i64> = sqlx::query_scalar(AssertSqlSafe(format!(
                "SELECT sequence FROM {table} WHERE occurred_at >= $1 AND sequence > $2 \
                 ORDER BY occurred_at, sequence LIMIT 1"
            )))
            .bind(at_time)
            .bind(floor.as_i64())
            .fetch_optional(pool)
            .await
            .map_err(map)?;
            Ok(found.map_or(ceiling, |sequence| Sequence::assigned(sequence - 1)))
        }
        Position::Exact(requested) if (floor..=ceiling).contains(requested) => Ok(*requested),
        Position::Exact(requested) => Err(BackendError::out_of_range(*requested, floor, ceiling)),
    }
}

/// The current head of each of `partitions`, for a watch's first items.
pub async fn current_heads(
    shared: &Shared,
    partitions: &[TopicPartition],
) -> Result<Vec<Head>, BackendError> {
    let mut heads = Vec::with_capacity(partitions.len());
    for at in partitions {
        heads.push(Head {
            at: at.clone(),
            sequence: resolve(shared, at, &Position::Latest).await?,
        });
    }
    Ok(heads)
}

/// Segment introspection (feature 0003): the matched range's figures from the
/// table, and the segments from the catalog.
pub async fn query(
    shared: &Shared,
    at: &TopicPartition,
    range: &PartitionRange,
) -> Result<Vec<TopicSegment>, BackendError> {
    const OP: &str = "query";
    let (_, table) = partition_of(shared, at, OP).await?;
    let pool = shared.pool()?;
    let map = |e| error::map(OP, &e);
    let (first, last, start_time, end_time): RangeFigures = sqlx::query_as(AssertSqlSafe(format!(
        "SELECT MIN(sequence), MAX(sequence), MIN(sequence_time), MAX(sequence_time) FROM \
         (SELECT sequence, sequence_time FROM {table} WHERE sequence >= $1 AND sequence <= $2 \
          ORDER BY sequence LIMIT $3) AS matched"
    )))
    .bind(range.start_offset.map_or(i64::MIN, Sequence::as_i64))
    .bind(range.end_offset.map_or(i64::MAX, Sequence::as_i64))
    .bind(i64::from(range.limit))
    .fetch_one(pool)
    .await
    .map_err(map)?;
    let (Some(first), Some(last)) = (first, last) else {
        return Ok(Vec::new());
    };
    let segments = segments_of(pool, &table)
        .await
        .map_err(map)?
        .into_iter()
        .map(|(name, from, to)| serde_json::json!({ "name": name, "from_sequence": from, "to_sequence": to - 1 }))
        .collect();
    Ok(vec![TopicSegment {
        topic: at.topic.to_string(),
        partition: at.partition.value(),
        start_sequence: Sequence::assigned(first),
        end_sequence: Sequence::assigned(last),
        start_time,
        end_time,
        segments,
    }])
}

/// `(name, from, to)` of every segment attached to `parent`, oldest first;
/// `to` is exclusive.
pub async fn segments_of(
    pool: &sqlx::PgPool,
    parent: &str,
) -> Result<Vec<(String, i64, i64)>, sqlx::Error> {
    let rows: Vec<(String, String)> = sqlx::query_as(
        "SELECT c.relname::text, pg_get_expr(c.relpartbound, c.oid) FROM pg_inherits i \
         JOIN pg_class c ON c.oid = i.inhrelid WHERE i.inhparent = to_regclass($1)",
    )
    .bind(parent)
    .fetch_all(pool)
    .await?;
    let mut segments: Vec<(String, i64, i64)> = rows
        .into_iter()
        .filter_map(|(name, bound)| segment::parse_bound(&bound).map(|(from, to)| (name, from, to)))
        .collect();
    segments.sort_by_key(|(_, from, _)| *from);
    Ok(segments)
}
