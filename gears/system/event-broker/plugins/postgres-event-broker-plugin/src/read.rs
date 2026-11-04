//! `read`, `resolve`, `query` (`docs/DESIGN.md` §4.5, §4.6, §4.8) and the
//! current heads a watch starts from (§4.7).

use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use event_broker_sdk::backend::{BackendError, Head, TopicPartition, Wait};
use event_broker_sdk::models::{Event, PartitionRange, TopicSegment};
use event_broker_sdk::{Position, Sequence};
use sqlx::AssertSqlSafe;
use uuid::Uuid;

use crate::error::Operation;
use crate::shared::Shared;
use crate::{body, error, names, segment};

/// One stored event.
#[derive(sqlx::FromRow)]
struct StoredRow {
    sequence: i64,
    sequence_time: DateTime<Utc>,
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

/// How often a waiting read re-reads besides being woken: a head from another
/// instance whose notification was lost still ends the wait.
const WAIT_POLL: Duration = Duration::from_secs(1);

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
            shared
                .heads
                .read_waiting(at, offset, wait, WAIT_POLL.min(wait), probe)
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
    // Expired segments stay attached until the detacher gets to them; the
    // floor hides them from the commit that expired them.
    //
    // No upper bound, though sequences are gapless (`append` raises the head
    // and inserts every sequence below it in one transaction) and
    // `(after, after + max_events]` would name the same rows: a bound on the
    // floor's subquery prunes only during execution, after every segment is
    // set up, and the `LIMIT` already stops in the first one, so the bound
    // would save nothing and look the floor up again.
    let rows: Vec<StoredRow> = sqlx::query_as(AssertSqlSafe(format!(
        "SELECT sequence, sequence_time, body FROM {table} \
         WHERE sequence > GREATEST($1, (SELECT floor FROM evbk__retention WHERE topic_uuid = $3 AND partition = $4)) \
         ORDER BY sequence LIMIT $2"
    )))
    .bind(offset.as_i64())
    .bind(i64::try_from(max_events).unwrap_or(i64::MAX))
    .bind(topic)
    .bind(names::partition_column(at.partition))
    .fetch_all(shared.pool()?)
    .await
    .map_err(|e| error::map(Operation::Read, &e))?;
    rows.into_iter()
        .map(|row| body::decode(&row.body, at.partition, row.sequence, row.sequence_time))
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
    let (head, floor): (i64, i64) = sqlx::query_as(
        "SELECT p.hwm, r.floor FROM evbk__partition p \
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
            // `sequence_time` rises with `sequence` (`append`), so every
            // event of a sealed segment is no later than its `newest_at`:
            // the search starts past the last segment sealed before
            // `at_time`, and the block-range index on `sequence_time` skips
            // what is left before it.
            let found: Option<i64> = sqlx::query_scalar(AssertSqlSafe(format!(
                "SELECT sequence FROM {table} WHERE sequence_time >= $1 AND sequence > \
                 GREATEST($2, (SELECT COALESCE(MAX(end_seq - 1), 0) FROM evbk__segment \
                  WHERE topic_uuid = $3 AND partition = $4 AND state = 'sealed' AND newest_at < $1)) \
                 ORDER BY sequence LIMIT 1"
            )))
            .bind(at_time)
            .bind(floor.as_i64())
            .bind(topic)
            .bind(names::partition_column(at.partition))
            .fetch_optional(pool)
            .await
            .map_err(map)?;
            Ok(found.map_or(ceiling, |sequence| Sequence::assigned(sequence - 1)))
        }
        Position::Exact(requested) if (floor..=ceiling).contains(requested) => Ok(*requested),
        Position::Exact(requested) => Err(BackendError::out_of_range(*requested, floor, ceiling)),
    }
}

/// The current head of each of `partitions`, in order, for a watch's first
/// items: one query, however many partitions.
pub async fn current_heads(
    shared: &Shared,
    partitions: &[TopicPartition],
) -> Result<Vec<Head>, BackendError> {
    const OP: Operation = Operation::Watch;
    let mut topics = Vec::with_capacity(partitions.len());
    let mut columns = Vec::with_capacity(partitions.len());
    for at in partitions {
        topics.push(partition_of(shared, at, OP).await?.0);
        columns.push(names::partition_column(at.partition));
    }
    let heads: Vec<i64> = sqlx::query_scalar(
        "SELECT p.hwm FROM UNNEST($1::uuid[], $2::int[]) WITH ORDINALITY AS w(topic_uuid, partition, n) \
         JOIN evbk__partition p USING (topic_uuid, partition) ORDER BY w.n",
    )
    .bind(topics)
    .bind(columns)
    .fetch_all(shared.pool()?)
    .await
    .map_err(|e| error::map(OP, &e))?;
    if heads.len() != partitions.len() {
        return Err(BackendError::Internal(format!(
            "{OP}: evbk__partition holds {} of {} named partitions",
            heads.len(),
            partitions.len()
        )));
    }
    Ok(partitions
        .iter()
        .zip(heads)
        .map(|(at, head)| Head {
            at: at.clone(),
            sequence: Sequence::assigned(head),
        })
        .collect())
}

/// Segment introspection (feature 0003): the matched range's figures from the
/// table, and the segments from the catalog.
pub async fn query(
    shared: &Shared,
    at: &TopicPartition,
    range: &PartitionRange,
) -> Result<Vec<TopicSegment>, BackendError> {
    const OP: Operation = Operation::Query;
    let (topic, table) = partition_of(shared, at, OP).await?;
    let pool = shared.pool()?;
    let map = |e| error::map(OP, &e);
    let figures: RangeFigures = sqlx::query_as(AssertSqlSafe(format!(
        "SELECT MIN(sequence) AS first, MAX(sequence) AS last, MIN(sequence_time) AS start_time, \
         MAX(sequence_time) AS end_time FROM \
         (SELECT sequence, sequence_time FROM {table} WHERE sequence <= $2 AND sequence >= \
          GREATEST($1, (SELECT floor + 1 FROM evbk__retention WHERE topic_uuid = $4 AND partition = $5)) \
          ORDER BY sequence LIMIT $3) AS matched"
    )))
    .bind(range.start_offset.map_or(i64::MIN, Sequence::as_i64))
    .bind(range.end_offset.map_or(i64::MAX, Sequence::as_i64))
    .bind(i64::from(range.limit))
    .bind(topic)
    .bind(names::partition_column(at.partition))
    .fetch_one(pool)
    .await
    .map_err(map)?;
    let (Some(first), Some(last)) = (figures.first, figures.last) else {
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
        start_time: figures.start_time,
        end_time: figures.end_time,
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
