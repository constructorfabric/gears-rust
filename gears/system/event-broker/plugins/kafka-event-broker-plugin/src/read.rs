//! `read`, `resolve` and `query` (`docs/DESIGN.md` §5.3-§5.5): pooled
//! `read_committed` consumers, assigned one partition per call.

use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use event_broker_sdk::backend::{BackendError, TopicPartition, Wait};
use event_broker_sdk::models::{Event, PartitionRange, TopicSegment};
use event_broker_sdk::{Position, Sequence};
use rdkafka::consumer::{Consumer, StreamConsumer};
use rdkafka::error::{KafkaError, KafkaResult, RDKafkaErrorCode};
use rdkafka::topic_partition_list::{Offset, TopicPartitionList};

use crate::clients::{self, Consumed, Lease, Record, Span};
use crate::error::{self, Operation};
use crate::shared::{Latest, Shared};
use crate::{body, names};

/// Up to `max_events` events after `offset`, waiting for one under
/// [`Wait::For`] when none is stored yet.
pub async fn read(
    shared: &Shared,
    at: &TopicPartition,
    offset: Sequence,
    max_events: usize,
    wait: Wait,
) -> Result<Vec<Event>, BackendError> {
    const OP: Operation = Operation::Read;
    let (known, partition) = shared.partition(at, OP).await?;
    let probe = || async {
        let consumed = with_lease(shared, OP, |consumer| {
            let topic = Arc::clone(&known.name);
            async move {
                let (low, high) = clients::watermarks(
                    Arc::clone(&consumer),
                    topic.to_string(),
                    partition,
                    shared.settings.request_timeout,
                )
                .await?;
                let start = names::first_offset_after(offset).max(low);
                clients::consume(
                    &consumer,
                    Span {
                        topic: &topic,
                        partition,
                        start,
                        end: high,
                        max: max_events,
                        timeout: shared.settings.request_timeout,
                        eof_ends: true,
                    },
                )
                .await
            }
        })
        .await?;
        consumed
            .records
            .into_iter()
            .map(|record| event_of(at, &record))
            .collect::<Result<Vec<_>, _>>()
    };
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

fn event_of(at: &TopicPartition, record: &Record) -> Result<Event, BackendError> {
    let value = record
        .value
        .as_deref()
        .ok_or_else(|| BackendError::Internal("a stored event record has no value".to_owned()))?;
    body::decode(
        value,
        at.partition,
        names::sequence_of(record.offset),
        stored_at(record)?,
    )
}

fn stored_at(record: &Record) -> Result<DateTime<Utc>, BackendError> {
    record
        .timestamp
        .and_then(DateTime::from_timestamp_millis)
        .ok_or_else(|| BackendError::Internal("a stored event record has no timestamp".to_owned()))
}

/// Runs `call` with a pooled consumer; a consumer that failed is not pooled
/// again.
async fn with_lease<F, Fut, T>(
    shared: &Shared,
    operation: Operation,
    call: F,
) -> Result<T, BackendError>
where
    F: FnOnce(Arc<StreamConsumer>) -> Fut,
    Fut: Future<Output = Result<T, KafkaError>>,
{
    let lease: Lease = shared.lease(operation).await?;
    match call(Arc::clone(lease.consumer())).await {
        Ok(value) => Ok(value),
        Err(err) => {
            lease.discard();
            Err(error::map(operation, &err))
        }
    }
}

/// Where the probes read records: the pooled consumer, or a scripted
/// partition in unit tests.
pub trait RecordSource {
    /// The committed records of `span`, as [`clients::consume`] reads them.
    fn consume(&self, span: Span<'_>) -> impl Future<Output = KafkaResult<Consumed>> + Send;
}

impl RecordSource for StreamConsumer {
    fn consume(&self, span: Span<'_>) -> impl Future<Output = KafkaResult<Consumed>> + Send {
        clients::consume(self, span)
    }
}

/// The partition's bounds: the floor (the cursor before its oldest stored
/// event) and the ceiling (its last event's sequence).
struct Bounds {
    low: i64,
    high: i64,
    floor: Sequence,
    ceiling: Sequence,
}

/// How many offsets below the high watermark the first look for the last
/// event spans: an event and its transaction's commit marker.
const FIRST_WINDOW: i64 = 4;

/// The partition's bounds. The ceiling is the sequence of the last committed
/// record below the last stable offset - never the watermark itself, which a
/// commit marker or an aborted record may hold - found by reading back from
/// the watermark in growing windows, and cached against it.
async fn bounds(
    shared: &Shared,
    at: &TopicPartition,
    topic: &str,
    partition: i32,
    operation: Operation,
) -> Result<Bounds, BackendError> {
    let timeout = shared.settings.request_timeout;
    with_lease(shared, operation, |consumer| async move {
        let (low, high) =
            clients::watermarks(Arc::clone(&consumer), topic.to_owned(), partition, timeout)
                .await?;
        let cached = shared.latest(at);
        let ceiling = match cached {
            Some(latest) if latest.high == high => latest.sequence,
            _ => {
                let found =
                    last_event(consumer.as_ref(), topic, partition, low, high, timeout).await?;
                let sequence = match found {
                    Some(record) => names::sequence_of(record.offset),
                    // Nothing ever stored, or every event removed. With every
                    // write transactional, the record below the watermark is
                    // a marker, so its offset is the last event's sequence
                    // when the last transaction committed.
                    None if low == 0 => Sequence::NONE,
                    None => Sequence::assigned(high - 1),
                };
                let mut sequence = shared.record_latest(at, Latest { high, sequence });
                if let Some(local) = shared.heads.known(at) {
                    sequence = sequence.max(local);
                }
                sequence
            }
        };
        let floor = Sequence::assigned(low).min(ceiling);
        Ok(Bounds {
            low,
            high,
            floor,
            ceiling,
        })
    })
    .await
}

/// The last committed record in `[low, high)`, read back from `high` in
/// growing windows.
async fn last_event<S: RecordSource>(
    source: &S,
    topic: &str,
    partition: i32,
    low: i64,
    high: i64,
    timeout: Duration,
) -> KafkaResult<Option<Record>> {
    let mut window = FIRST_WINDOW;
    loop {
        let start = high.saturating_sub(window).max(low);
        let consumed = source
            .consume(Span {
                topic,
                partition,
                start,
                end: high,
                max: usize::try_from(high - start).unwrap_or(usize::MAX),
                timeout,
                eof_ends: true,
            })
            .await?;
        if let Some(last) = complete(consumed)?.pop() {
            return Ok(Some(last));
        }
        if start <= low {
            return Ok(None);
        }
        window = window.saturating_mul(16);
    }
}

/// The sequence `position` names, checked against the partition's floor and
/// ceiling.
pub async fn resolve(
    shared: &Shared,
    at: &TopicPartition,
    position: &Position,
) -> Result<Sequence, BackendError> {
    const OP: Operation = Operation::Resolve;
    let (known, partition) = shared.partition(at, OP).await?;
    let bounds = bounds(shared, at, &known.name, partition, OP).await?;
    let Bounds { floor, ceiling, .. } = bounds;
    match position {
        Position::Latest => Ok(ceiling),
        Position::Earliest => Ok(floor),
        Position::Exact(requested) if (floor..=ceiling).contains(requested) => Ok(*requested),
        Position::Exact(requested) => Err(BackendError::out_of_range(*requested, floor, ceiling)),
        Position::At(instant) => {
            let found = offset_for_time(shared, &known.name, partition, *instant, &bounds).await?;
            // The cursor before the first record stored at or after the
            // instant, kept within the bounds: the record found may be a
            // marker past the last event.
            Ok(found.map_or(ceiling, |offset| {
                Sequence::assigned(offset).clamp(floor, ceiling)
            }))
        }
    }
}

/// The first offset stored at or after `instant`, by `LogAppendTime`; `None`
/// when nothing was.
async fn offset_for_time(
    shared: &Shared,
    topic: &str,
    partition: i32,
    instant: DateTime<Utc>,
    bounds: &Bounds,
) -> Result<Option<i64>, BackendError> {
    const OP: Operation = Operation::Resolve;
    // Record timestamps are milliseconds: the first one at or after a finer
    // instant is at or after the instant rounded up.
    let micros = instant.timestamp_micros();
    let millis = micros.div_euclid(1000) + i64::from(micros.rem_euclid(1000) != 0);
    let timeout = shared.settings.request_timeout;
    let (low, high) = (bounds.low, bounds.high);
    let topic = topic.to_owned();
    with_lease(shared, OP, |consumer| async move {
        let mut wanted = TopicPartitionList::new();
        wanted.add_partition_offset(&topic, partition, Offset::Offset(millis))?;
        let answer = clients::detached(move || consumer.offsets_for_times(wanted, timeout))
            .await
            .map_err(|_| KafkaError::Canceled)??;
        Ok(answer
            .find_partition(&topic, partition)
            .and_then(|element| match element.offset() {
                Offset::Offset(offset) if offset < high => Some(offset.max(low)),
                _ => None,
            }))
    })
    .await
}

/// Segment introspection (feature 0003): the matched range's figures, read
/// from the partition, and the Kafka partition the log lives in. Kafka keeps
/// its segment files to itself, so the one entry describes the partition.
pub async fn query(
    shared: &Shared,
    at: &TopicPartition,
    range: &PartitionRange,
) -> Result<Vec<TopicSegment>, BackendError> {
    const OP: Operation = Operation::Query;
    let (known, partition) = shared.partition(at, OP).await?;
    let bounds = bounds(shared, at, &known.name, partition, OP).await?;
    let scope = Scope {
        topic: &known.name,
        partition,
        low: bounds.low,
        high: bounds.high,
        timeout: shared.settings.request_timeout,
    };
    let lease = shared.lease(OP).await?;
    let found = query_on(lease.consumer().as_ref(), at, &scope, range).await;
    if found.is_err() {
        lease.discard();
    }
    found
}

/// The Kafka partition a query reads, and its watermarks.
pub struct Scope<'a> {
    /// The Kafka topic.
    pub topic: &'a str,
    /// The Kafka partition.
    pub partition: i32,
    /// The log start offset.
    pub low: i64,
    /// The last stable offset.
    pub high: i64,
    /// How long one read may take.
    pub timeout: Duration,
}

/// [`query`] over `source`: the first event by a single-record read
/// forward, the last by the backward windows of [`last_event`] when the
/// limit covers the range, else by reading forward at most `limit` records.
/// A read the timeout cuts short is an error, never a partial envelope.
pub async fn query_on<S: RecordSource>(
    source: &S,
    at: &TopicPartition,
    scope: &Scope<'_>,
    range: &PartitionRange,
) -> Result<Vec<TopicSegment>, BackendError> {
    const OP: Operation = Operation::Query;
    let first = range.start_offset.map_or(scope.low, |start| {
        start.as_i64().saturating_sub(1).max(scope.low)
    });
    let last = range
        .end_offset
        .map_or(i64::MAX, |end| end.as_i64().saturating_sub(1));
    let end = scope.high.min(last.saturating_add(1));
    let limit = usize::try_from(range.limit).unwrap_or(usize::MAX);
    let found = ends(source, scope, first, end, limit)
        .await
        .map_err(|err| error::map(OP, &err))?;
    let Some((oldest, newest)) = found else {
        return Ok(Vec::new());
    };
    Ok(vec![TopicSegment {
        topic: at.topic.to_string(),
        partition: at.partition.value(),
        start_sequence: names::sequence_of(oldest.offset),
        end_sequence: names::sequence_of(newest.offset),
        start_time: Some(stored_at(&oldest)?),
        end_time: Some(stored_at(&newest)?),
        segments: vec![serde_json::json!({
            "kafka_topic": scope.topic,
            "kafka_partition": scope.partition,
            "log_start_offset": scope.low,
            "last_stable_offset": scope.high,
        })],
    }])
}

/// The first and the last of the first `limit` events in `[first, end)`;
/// `None` when there is none or `limit` is 0, which reads nothing.
async fn ends<S: RecordSource>(
    source: &S,
    scope: &Scope<'_>,
    first: i64,
    end: i64,
    limit: usize,
) -> KafkaResult<Option<(Record, Record)>> {
    if limit == 0 {
        return Ok(None);
    }
    let span = |start: i64, max: usize| Span {
        topic: scope.topic,
        partition: scope.partition,
        start,
        end,
        max,
        timeout: scope.timeout,
        eof_ends: true,
    };
    let Some(oldest) = complete(source.consume(span(first, 1)).await?)?
        .into_iter()
        .next()
    else {
        return Ok(None);
    };
    let covered = i64::try_from(limit).map_or(true, |limit| limit >= end - oldest.offset);
    let newest = if covered {
        last_event(
            source,
            scope.topic,
            scope.partition,
            oldest.offset,
            end,
            scope.timeout,
        )
        .await?
    } else {
        complete(source.consume(span(oldest.offset, limit)).await?)?.pop()
    };
    // Both reads start at `oldest`: finding nothing means retention removed it
    // meanwhile, and the envelope falls back to the one event already read.
    let newest = newest.unwrap_or_else(|| oldest.clone());
    Ok(Some((oldest, newest)))
}

/// The records of a read that reached its end or its `max`; a read the
/// timeout cut short is no evidence of what lies past it.
fn complete(consumed: Consumed) -> KafkaResult<Vec<Record>> {
    if consumed.complete {
        Ok(consumed.records)
    } else {
        Err(KafkaError::MessageConsumption(
            RDKafkaErrorCode::OperationTimedOut,
        ))
    }
}
