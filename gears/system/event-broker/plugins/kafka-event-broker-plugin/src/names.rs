//! Kafka names the plugin builds (`docs/DESIGN.md` §2): topics, transactional
//! ids, and the sequence/offset mapping (§4).

use event_broker_sdk::Sequence;
use event_broker_sdk::backend::FenceKey;
use uuid::Uuid;

/// The Kafka topic holding one event-broker topic's log.
pub fn event_topic(prefix: &str, topic: Uuid) -> String {
    format!("{prefix}.{}", topic.simple())
}

/// The compacted topic holding each event-broker topic's log record.
pub fn logs_topic(prefix: &str) -> String {
    format!("{prefix}.logs")
}

/// The compacted topic holding each fence key's last applied seq.
pub fn fences_topic(prefix: &str) -> String {
    format!("{prefix}.fences")
}

/// The transactional id that serializes provisioning of one topic.
pub fn log_transaction(prefix: &str, topic: Uuid) -> String {
    format!("{prefix}.log.{}", topic.simple())
}

/// The transactional id of one fence key's producer.
pub fn fence_transaction(prefix: &str, key: FenceKey) -> String {
    format!("{prefix}.fence.{key}")
}

/// The fence-topic partition a fence key's records live in.
pub fn fence_partition(key: FenceKey, partitions: u32) -> i32 {
    let partition = key.value() % u128::from(partitions.max(1));
    i32::try_from(partition).unwrap_or(0)
}

/// The sequence of the record at `offset`: offsets start at 0, sequences at 1.
pub fn sequence_of(offset: i64) -> Sequence {
    Sequence::assigned(offset.saturating_add(1))
}

/// The first Kafka offset a read after `offset` may return: the record whose
/// sequence is `offset + 1`.
pub fn first_offset_after(offset: Sequence) -> i64 {
    offset.as_i64().max(0)
}
