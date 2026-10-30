//! The ingest outbox: its queue, its payload, and which of its partitions an
//! event drains through.
//!
//! Ingest never assigns a topic partition. It resolves the event's partition
//! key and places the event in the ingest outbox partition derived from the
//! topic and that key, so every event of one key drains in publish order; the
//! storage backend maps the key to a topic partition when it stores the event
//! (feature 0007 §4.1).

use std::fmt;

use event_broker_sdk::backend::FenceKey;
use serde::{Deserialize, Serialize};
use toolkit::domain_model;
use toolkit_gts::GtsInstanceId;

use crate::domain::model::Event;

/// `toolkit_db::outbox` queue name for the ingest publish path: the outbox
/// group's name. Only the reserved group `default` exists, and it carries every
/// topic (feature 0005 §2.5).
pub const INGEST_QUEUE_NAME: &str = "default";

/// The `default` group's toolkit-db table prefix. A fixed name rather than one
/// minted at runtime, so its tables are created by the migrations the gear
/// declares, before any instance starts, and the gear never runs DDL itself.
/// 24 bytes, within toolkit-db's 36.
pub const OUTBOX_TABLE_PREFIX: &str = "event_broker__ob_default";

/// `payload_type` every ingest outbox row is stamped with.
pub const INGEST_PAYLOAD_TYPE: &str = "application/vnd.event-broker.ingest-event+json;version=2";

/// How many partitions an outbox group drains through: a power of 2 from 1 to
/// 64, the toolkit-db outbox's limit (feature 0005 §2.4, C8). Fixed once the
/// group's queue exists - toolkit-db refuses a different count for it.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OutboxPartitions(u16);

impl OutboxPartitions {
    /// What a group gets when configuration states no count (feature 0005
    /// §2.5).
    pub const DEFAULT: Self = Self(16);

    /// `None` unless `count` is a power of 2 from 1 to 64.
    #[must_use]
    pub fn new(count: u32) -> Option<Self> {
        (count.is_power_of_two() && count <= 64)
            .then(|| u16::try_from(count).ok().map(Self))
            .flatten()
    }

    #[must_use]
    pub const fn get(self) -> u16 {
        self.0
    }
}

impl Default for OutboxPartitions {
    fn default() -> Self {
        Self::DEFAULT
    }
}

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
/// `key` drains through, of the group's `partitions`.
///
/// The outbox sequences within a partition, so every event of one key of one
/// topic has to land in the same partition or their order is lost on the way
/// to the backend. Hashing the key spreads topics and keys over every
/// partition.
#[must_use]
pub fn outbox_partition_for(topic: &GtsInstanceId, key: &str, partitions: OutboxPartitions) -> u32 {
    let placement = format!("{topic}:{key}");
    (toolkit_stable_hash::murmur3_x86_32(placement.as_bytes(), 0) & 0x7FFF_FFFF)
        % u32::from(partitions.get())
}

/// The 64-bit identity of one ingest outbox group (feature 0005 §2.5).
///
/// Stored as its 16 lowercase hex digits ([`Display`](fmt::Display) /
/// [`parse`](Self::parse)), the form the identity table and logs both use.
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

    /// Reads back what [`Display`](fmt::Display) wrote: exactly 16 lowercase
    /// hex digits, nothing else. A stored value of any other shape is not one
    /// this gear wrote.
    #[must_use]
    pub fn parse(hex: &str) -> Option<Self> {
        (hex.len() == 16 && hex.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')))
            .then(|| u64::from_str_radix(hex, 16).ok().map(Self))
            .flatten()
    }
}

/// Sixteen lowercase hex digits: the form the identity table and logs use.
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
