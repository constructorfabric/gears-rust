//! The records of the plugin's two compacted topics (`docs/DESIGN.md` §2.3,
//! §3): a topic's log record and a fence key's last applied seq.

use event_broker_sdk::GtsInstanceId;
use event_broker_sdk::backend::{FenceKey, LogId};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

const LOG_VERSION: u8 = 1;
const FENCE_VERSION: u8 = 1;

/// What the logs topic holds for one event-broker topic, keyed by its GTS id.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LogRecord {
    /// The record format version.
    pub v: u8,
    /// The GTS topic id, for an operator reading the topic.
    pub topic: String,
    /// The topic's Registry Reference, `TopicSpec.uuid`; it names the topic's Kafka event topic.
    pub uuid: Uuid,
    /// The id of the log the topic's events belong to.
    pub log_id: Uuid,
    /// The partition count the Kafka topic held when the log was claimed.
    pub partitions: u32,
}

impl LogRecord {
    /// A log record in the current format.
    pub fn new(topic: &GtsInstanceId, uuid: Uuid, log_id: LogId, partitions: u32) -> Self {
        Self {
            v: LOG_VERSION,
            topic: topic.to_string(),
            uuid,
            log_id: log_id.uuid(),
            partitions,
        }
    }

    /// The record's log id.
    pub fn log_id(&self) -> LogId {
        LogId::from_uuid(self.log_id)
    }

    /// The record as the logs topic stores it.
    pub fn encode(&self) -> Vec<u8> {
        serde_json::to_vec(self).unwrap_or_default()
    }

    /// `None` for a value this plugin did not write.
    pub fn decode(value: &[u8]) -> Option<Self> {
        serde_json::from_slice::<Self>(value)
            .ok()
            .filter(|record| record.v == LOG_VERSION && record.partitions > 0)
    }
}

/// The logs-topic key of a topic: its GTS id.
pub fn log_key(topic: &GtsInstanceId) -> Vec<u8> {
    topic.as_ref().as_bytes().to_vec()
}

/// The fence-topic key of a fence key: its 16 bytes.
pub fn fence_key(key: FenceKey) -> Vec<u8> {
    key.value().to_be_bytes().to_vec()
}

/// A fence key's last applied seq, as the fence topic holds it.
pub fn encode_fence(seq: i64) -> Vec<u8> {
    let mut value = Vec::with_capacity(9);
    value.push(FENCE_VERSION);
    value.extend_from_slice(&seq.to_be_bytes());
    value
}

/// `None` for a value this plugin did not write.
pub fn decode_fence(value: &[u8]) -> Option<i64> {
    match value {
        [FENCE_VERSION, rest @ ..] => <[u8; 8]>::try_from(rest).ok().map(i64::from_be_bytes),
        _ => None,
    }
}
