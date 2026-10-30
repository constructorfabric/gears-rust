//! `append` (`docs/DESIGN.md` §5.2): the events and the fence record in one
//! Kafka transaction of the fence key's producer.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use event_broker_sdk::backend::support::partition_for;
use event_broker_sdk::backend::{
    AppendRequest, Appended, BackendError, Head, Partition, TopicPartition,
};
use rdkafka::error::KafkaError;
use rdkafka::producer::{FutureRecord, Producer};
use tokio::time::Instant;

use crate::error::{self, Operation};
use crate::fence::{self, Open};
use crate::shared::{KnownTopic, Shared};
use crate::{body, clients, names, records};

/// The most events one `append` carries: one transaction, well under a
/// broker's default `message.max.bytes` per record batch.
pub const MAX_EVENTS_PER_APPEND: usize = 1000;

const OP: Operation = Operation::Append;

/// Appends a batch in one transaction, or reports it a duplicate when the
/// fence has already passed its seq.
pub async fn append(
    shared: &Shared,
    request: &AppendRequest<'_>,
) -> Result<Appended, BackendError> {
    if request.events.len() > MAX_EVENTS_PER_APPEND {
        return Err(BackendError::append_failed(format!(
            "an append carries at most {MAX_EVENTS_PER_APPEND} events"
        )));
    }
    // Counted from here, so `stop` waits for this call to end.
    let _in_flight = shared.enter(OP)?;
    // Provisioned? Before the fence: a fence that has passed this seq is no
    // answer for a topic the backend does not hold.
    let known = shared.topic(request.topic, OP).await?;
    let mut encoded = Vec::with_capacity(request.events.len());
    for keyed in request.events {
        encoded.push(Encoded {
            partition: partition_for(&keyed.key, known.partitions),
            key: keyed.key.as_bytes(),
            value: body::encode(&keyed.event)?,
        });
    }
    let seq = request.fence.seq.value();
    let lease = shared
        .producers
        .slot(request.fence.key)
        .await
        .ok_or_else(|| error::stopped(OP))?;
    let mut slot = lease.slot().lock().await;
    // A fenced producer is opened again once: the instance that fenced it
    // may have applied this very fence, which only a fresh read shows.
    for attempt in 0..2 {
        shared.check_running(OP)?;
        if shared.producers.is_closed() {
            return Err(error::stopped(OP));
        }
        // Out of the slot while it is used: a call dropped from here on
        // drops the producer, and the next one opens it again.
        let open = match slot.open.take() {
            Some(open) => open,
            None => fence::open_fence(shared, request.fence.key).await?,
        };
        if open.last.is_some_and(|last| seq <= last) {
            slot.open = Some(open);
            return Ok(Appended::Duplicate);
        }
        match write(shared, &open, &known, request, &encoded).await {
            Ok(heads) => {
                slot.open = Some(Open {
                    last: Some(seq),
                    ..open
                });
                for head in &heads {
                    shared.heads.publish(head.clone());
                }
                return Ok(Appended::Heads(heads));
            }
            Err(err) => {
                drop(open);
                if attempt == 0 && error::is_fenced(&err) {
                    continue;
                }
                return Err(error::map(OP, &err));
            }
        }
    }
    Err(BackendError::unavailable(
        "append: the fence key's producer was fenced twice in a row",
    ))
}

/// Waits until `read_committed` readers see each partition's head: the
/// commit returns once the coordinator has decided, and the brokers write the
/// partitions' commit markers - which move the last stable offset past the
/// events - a moment later. Gives up after the request timeout: the events are
/// committed either way, and become readable with the markers.
async fn await_visible(shared: &Shared, topic: &str, heads: &BTreeMap<Partition, i64>) {
    let deadline = Instant::now() + shared.settings.request_timeout;
    let Ok(consumer) = shared.stable_consumer(OP) else {
        return;
    };
    for (partition, offset) in heads {
        let partition = i32::try_from(partition.value()).unwrap_or(i32::MAX);
        let mut pause = FIRST_VISIBILITY_POLL;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            let watermarks = clients::watermarks(
                Arc::clone(&consumer),
                topic.to_owned(),
                partition,
                left.max(Duration::from_millis(1)),
            )
            .await;
            match watermarks {
                Ok((_, stable)) if stable > *offset => break,
                Ok(_) if Instant::now() + pause < deadline => {
                    tokio::time::sleep(pause).await;
                    pause = (pause * 2).min(LAST_VISIBILITY_POLL);
                }
                Ok(_) | Err(_) => {
                    tracing::warn!(
                        "kafka backend: committed events not yet readable within the request timeout"
                    );
                    return;
                }
            }
        }
    }
}

/// Between two looks at a partition's last stable offset after a commit:
/// from the first, doubling to the last. Markers usually land within a few
/// milliseconds.
const FIRST_VISIBILITY_POLL: Duration = Duration::from_millis(1);
const LAST_VISIBILITY_POLL: Duration = Duration::from_millis(50);

/// One event, ready to send.
struct Encoded<'a> {
    partition: Partition,
    key: &'a [u8],
    value: Vec<u8>,
}

/// One transaction: every event, then the fence record. The heads come from
/// the offsets the broker assigned.
async fn write(
    shared: &Shared,
    open: &Open,
    known: &KnownTopic,
    request: &AppendRequest<'_>,
    encoded: &[Encoded<'_>],
) -> Result<Vec<Head>, KafkaError> {
    let producer = open.producer.producer();
    let timeout = shared.settings.request_timeout;
    let fence_key = records::fence_key(request.fence.key);
    let fence_value = records::encode_fence(request.fence.seq.value());
    let fence_partition = open.partition;

    // Aborted on the blocking pool if this future is dropped before it ends.
    let transaction = fence::Transaction::begin(producer, timeout)?;
    let sent = async {
        let mut deliveries = Vec::with_capacity(encoded.len());
        for event in encoded {
            let record = FutureRecord::to(&known.name)
                .partition(i32::try_from(event.partition.value()).unwrap_or(i32::MAX))
                .key(event.key)
                .payload(&event.value);
            let delivery = producer.send_result(record).map_err(|(err, _)| err)?;
            deliveries.push((event.partition, delivery));
        }
        let fence = FutureRecord::to(&shared.fences_topic)
            .partition(fence_partition)
            .key(&fence_key)
            .payload(&fence_value);
        let fence_delivery = producer.send_result(fence).map_err(|(err, _)| err)?;
        let mut heads: BTreeMap<Partition, i64> = BTreeMap::new();
        for (partition, delivery) in deliveries {
            let delivered = delivery.await.map_err(|_| KafkaError::Canceled)?;
            let offset = delivered.map_err(|(err, _)| err)?.offset;
            let head = heads.entry(partition).or_insert(offset);
            *head = (*head).max(offset);
        }
        fence_delivery
            .await
            .map_err(|_| KafkaError::Canceled)?
            .map_err(|(err, _)| err)?;
        Ok::<_, KafkaError>(heads)
    }
    .await;
    let offsets = match sent {
        Ok(heads) => heads,
        Err(err) => {
            fence::abort(producer, timeout).await;
            transaction.end();
            return Err(err);
        }
    };
    if let Err(err) = fence::blocking(producer, move |p| p.commit_transaction(timeout)).await {
        // Committed or not is unknown: the caller retries the same fence,
        // and the producer opened for it settles this transaction first.
        fence::abort(producer, timeout).await;
        transaction.end();
        return Err(err);
    }
    transaction.end();
    await_visible(shared, &known.name, &offsets).await;
    Ok(offsets
        .into_iter()
        .map(|(partition, offset)| Head {
            at: TopicPartition::new(request.topic.clone(), partition),
            sequence: names::sequence_of(offset),
        })
        .collect())
}
