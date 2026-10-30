//! `read`, `resolve`, `query` (`docs/DESIGN.md` §4.4, §4.5, §4.7) and the
//! current heads a watch starts from (§4.6).

use std::time::Duration;

use event_broker_sdk::backend::{BackendError, Head, TopicPartition, Wait};
use event_broker_sdk::models::{Event, PartitionRange, TopicSegment};
use event_broker_sdk::{Position, Sequence};
use uuid::Uuid;

use crate::error::{self, Operation};
use crate::shared::Shared;
use crate::{body, clock};

/// How often a waiting read re-reads besides being woken: the hub hears every
/// append made in this process, so this only bounds the wait should a head
/// arrive some other way.
const WAIT_POLL: Duration = Duration::from_secs(1);

#[derive(sqlx::FromRow)]
struct StoredRow {
    sequence: i64,
    sequence_time: i64,
    body: Vec<u8>,
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
    let (topic, partition) = shared.partition(at, Operation::Read).await?;
    let probe = || read_now(shared, at, topic, partition, offset, max_events);
    match wait {
        Wait::No => probe().await,
        Wait::For(wait) => {
            shared
                .opened()?
                .hub
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
    partition: i64,
    offset: Sequence,
    max_events: usize,
) -> Result<Vec<Event>, BackendError> {
    // The sweep raises the floor and deletes below it in one transaction, so
    // no row at or below the floor is ever visible: no floor check here.
    let rows: Vec<StoredRow> = sqlx::query_as(
        "SELECT sequence, sequence_time, body FROM evbk__event \
         WHERE topic_uuid = ?1 AND partition = ?2 AND sequence > ?3 ORDER BY sequence LIMIT ?4",
    )
    .bind(topic)
    .bind(partition)
    .bind(offset.as_i64())
    .bind(i64::try_from(max_events).unwrap_or(i64::MAX))
    .fetch_all(&shared.opened()?.pool)
    .await
    .map_err(|e| error::map(Operation::Read, &e))?;
    rows.into_iter()
        .map(|row| {
            body::decode(
                &row.body,
                at.partition,
                row.sequence,
                clock::instant(row.sequence_time)?,
            )
        })
        .collect()
}

/// The partition's `(hwm, floor)`.
async fn bounds(
    shared: &Shared,
    topic: Uuid,
    partition: i64,
    operation: Operation,
) -> Result<(i64, i64), BackendError> {
    sqlx::query_as(
        "SELECT hwm, floor FROM evbk__partition WHERE topic_uuid = ?1 AND partition = ?2",
    )
    .bind(topic)
    .bind(partition)
    .fetch_one(&shared.opened()?.pool)
    .await
    .map_err(|e| error::map(operation, &e))
}

/// The sequence `position` names, checked against the partition's floor and
/// head.
pub async fn resolve(
    shared: &Shared,
    at: &TopicPartition,
    position: &Position,
) -> Result<Sequence, BackendError> {
    const OP: Operation = Operation::Resolve;
    let (topic, partition) = shared.partition(at, OP).await?;
    let (head, floor) = match position {
        Position::At(at_time) => return resolve_at(shared, topic, partition, *at_time).await,
        Position::Latest | Position::Earliest | Position::Exact(_) => {
            bounds(shared, topic, partition, OP).await?
        }
    };
    let ceiling = Sequence::assigned(head);
    let floor = Sequence::assigned(floor);
    match position {
        Position::Latest => Ok(ceiling),
        Position::Exact(requested) if (floor..=ceiling).contains(requested) => Ok(*requested),
        Position::Exact(requested) => Err(BackendError::out_of_range(*requested, floor, ceiling)),
        Position::Earliest | Position::At(_) => Ok(floor),
    }
}

/// One below the first event stored at or after `at_time`; the head when none
/// is that late.
///
/// One statement, one snapshot: the head and the event are read together.
/// `sequence_time` never falls with `sequence` (`append`), so the first event
/// in time order is the first in sequence order too.
async fn resolve_at(
    shared: &Shared,
    topic: Uuid,
    partition: i64,
    at_time: chrono::DateTime<chrono::Utc>,
) -> Result<Sequence, BackendError> {
    let (head, found): (i64, Option<i64>) = sqlx::query_as(
        "SELECT p.hwm, (SELECT e.sequence FROM evbk__event e \
          WHERE e.topic_uuid = p.topic_uuid AND e.partition = p.partition \
          AND e.sequence_time >= ?3 ORDER BY e.sequence_time, e.sequence LIMIT 1) \
         FROM evbk__partition p WHERE p.topic_uuid = ?1 AND p.partition = ?2",
    )
    .bind(topic)
    .bind(partition)
    .bind(clock::micros_ceil(at_time))
    .fetch_one(&shared.opened()?.pool)
    .await
    .map_err(|e| error::map(Operation::Resolve, &e))?;
    Ok(Sequence::assigned(
        found.map_or(head, |sequence| sequence - 1),
    ))
}

/// The current head of each of `partitions`, in order, for a watch's first
/// items.
pub async fn current_heads(
    shared: &Shared,
    partitions: &[TopicPartition],
) -> Result<Vec<Head>, BackendError> {
    const OP: Operation = Operation::Watch;
    let mut heads = Vec::with_capacity(partitions.len());
    for at in partitions {
        let (topic, partition) = shared.partition(at, OP).await?;
        let (head, _) = bounds(shared, topic, partition, OP).await?;
        heads.push(Head {
            at: at.clone(),
            sequence: Sequence::assigned(head),
        });
    }
    Ok(heads)
}

#[derive(sqlx::FromRow)]
struct RangeFigures {
    first: Option<i64>,
    last: Option<i64>,
    start_time: Option<i64>,
    end_time: Option<i64>,
}

/// Segment introspection (feature 0003): the matched range's figures, and the
/// partition's stored span as its one segment.
pub async fn query(
    shared: &Shared,
    at: &TopicPartition,
    range: &PartitionRange,
) -> Result<Vec<TopicSegment>, BackendError> {
    const OP: Operation = Operation::Query;
    let (topic, partition) = shared.partition(at, OP).await?;
    let figures: RangeFigures = sqlx::query_as(
        "SELECT MIN(sequence) AS first, MAX(sequence) AS last, MIN(sequence_time) AS start_time, \
         MAX(sequence_time) AS end_time FROM \
         (SELECT sequence, sequence_time FROM evbk__event WHERE topic_uuid = ?1 AND partition = ?2 \
          AND sequence >= ?3 AND sequence <= ?4 ORDER BY sequence LIMIT ?5)",
    )
    .bind(topic)
    .bind(partition)
    .bind(range.start_offset.map_or(i64::MIN, Sequence::as_i64))
    .bind(range.end_offset.map_or(i64::MAX, Sequence::as_i64))
    .bind(i64::from(range.limit))
    .fetch_one(&shared.opened()?.pool)
    .await
    .map_err(|e| error::map(OP, &e))?;
    let (Some(first), Some(last)) = (figures.first, figures.last) else {
        return Ok(Vec::new());
    };
    let (head, floor) = bounds(shared, topic, partition, OP).await?;
    Ok(vec![TopicSegment {
        topic: at.topic.to_string(),
        partition: at.partition.value(),
        start_sequence: Sequence::assigned(first),
        end_sequence: Sequence::assigned(last),
        start_time: figures.start_time.map(clock::instant).transpose()?,
        end_time: figures.end_time.map(clock::instant).transpose()?,
        segments: vec![serde_json::json!({ "from_sequence": floor + 1, "to_sequence": head })],
    }])
}
