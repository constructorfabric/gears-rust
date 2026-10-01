//! The ingest outbox: its queue, its payload, and which of its partitions an
//! event drains through.
//!
//! Ingest never assigns a topic partition. It resolves the event's partition
//! key and places the event in the ingest outbox partition derived from the
//! topic and that key, so every event of one key drains in publish order; the
//! storage backend maps the key to a topic partition when it stores the event
//! (feature 0007 §3.1).

use std::fmt;

use event_broker_sdk::backend::FenceKey;
use serde::{Deserialize, Serialize};
use toolkit::domain_model;
use toolkit_gts::GtsInstanceId;

use crate::domain::model::Event;

/// `toolkit_db::outbox` queue name for the ingest publish path.
pub const INGEST_QUEUE_NAME: &str = "evbk-ingest";

/// `payload_type` every ingest outbox row is stamped with.
pub const INGEST_PAYLOAD_TYPE: &str = "application/vnd.event-broker.ingest-event+json;version=2";

/// Partition count `Outbox::builder(..).queue(INGEST_QUEUE_NAME, ..)` is
/// registered with - `toolkit_db::outbox::Partitions::of` requires a power
/// of 2 in `1..=64`; `4` is a reasonable default for a single-process
/// deployment (eb-single-process-implementation D7), not yet
/// operator-configurable.
pub const INGEST_OUTBOX_PARTITIONS: u16 = 4;

/// One ingest outbox message: one event, the topic it was published to, and
/// the partition-key value the backend partitions it by.
#[domain_model]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IngestPayload {
    /// The topic, resolved at publish.
    pub topic: GtsInstanceId,
    /// The value at the event type's partition-key pointer (ADR-0002).
    pub key: String,
    /// No partition, no sequence: both are the backend's to assign.
    pub event: Event,
}

/// The ingest outbox partition an event of `topic` with partition-key value
/// `key` drains through.
///
/// The outbox sequences within a partition, so every event of one key of one
/// topic has to land in the same partition or their order is lost on the way
/// to the backend. Hashing the key spreads topics and keys over every
/// partition.
#[must_use]
pub fn outbox_partition_for(topic: &GtsInstanceId, key: &str) -> u32 {
    let placement = format!("{topic}:{key}");
    (toolkit_stable_hash::murmur3_x86_32(placement.as_bytes(), 0) & 0x7FFF_FFFF)
        % u32::from(INGEST_OUTBOX_PARTITIONS)
}

/// The 64-bit identity of one ingest outbox queue (feature 0005 §2.5).
///
/// Stored in SQL as `BIGINT` with the same 64 bits ([`signed`](Self::signed) /
/// [`from_signed`](Self::from_signed)): a random identity exceeds the signed
/// range half the time, and a cast keeps it exact where a range check would
/// refuse it.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct OutboxId(u64);

impl OutboxId {
    #[must_use]
    pub const fn new(id: u64) -> Self {
        Self(id)
    }

    #[must_use]
    pub const fn value(self) -> u64 {
        self.0
    }

    /// The same 64 bits as a signed integer, for a `BIGINT` column.
    #[must_use]
    #[allow(clippy::cast_possible_wrap)]
    pub const fn signed(self) -> i64 {
        self.0 as i64
    }

    /// Reads back what [`signed`](Self::signed) stored.
    #[must_use]
    #[allow(clippy::cast_sign_loss)]
    pub const fn from_signed(bits: i64) -> Self {
        Self(bits as u64)
    }
}

/// Sixteen lowercase hex digits: the form logs use.
impl fmt::Display for OutboxId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:016x}", self.0)
    }
}

/// The fence key an ingest outbox message appends under: the outbox identity
/// in the high 64 bits, the message's partition (`OutboxMessage.partition_id`,
/// a row id, not an index) in the low 64. Each message's `seq` is strictly
/// increasing within that pair, which is what a fence key promises a backend.
#[must_use]
#[allow(clippy::cast_sign_loss)]
pub const fn fence_key(outbox: OutboxId, partition_id: i64) -> FenceKey {
    FenceKey::new(((outbox.value() as u128) << 64) | (partition_id as u64 as u128))
}
