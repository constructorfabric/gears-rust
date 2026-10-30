//! The three topics, the keys writers draw from, and the stamp each event
//! carries (design D5).

use std::time::Duration;

use event_broker_sdk::backend::support::{partition_for, topic_uuid};
use event_broker_sdk::backend::{
    Backend, KeyedEvent, LogClaim, LogId, Partition, Replacing, Retention, TopicPartition,
    TopicSpec,
};
use event_broker_sdk::models::Event;
use event_broker_sdk::{GtsInstanceId, GtsTypeId};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// One topic per kind of retention bound.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Age,
    Bytes,
    Keep,
}

impl Kind {
    pub const ALL: [Self; 3] = [Self::Age, Self::Bytes, Self::Keep];

    pub const fn name(self) -> &'static str {
        match self {
            Self::Age => "age",
            Self::Bytes => "bytes",
            Self::Keep => "keep",
        }
    }

    pub const fn index(self) -> usize {
        match self {
            Self::Age => 0,
            Self::Bytes => 1,
            Self::Keep => 2,
        }
    }
}

/// A provisioned topic.
#[derive(Debug, Clone)]
pub struct Topic {
    pub kind: Kind,
    pub id: GtsInstanceId,
    pub uuid: Uuid,
    pub partitions: u32,
}

impl Topic {
    pub fn at(&self, partition: u32) -> TopicPartition {
        TopicPartition::new(self.id.clone(), Partition::new(partition))
    }

    pub fn all(&self) -> Vec<TopicPartition> {
        (0..self.partitions).map(|p| self.at(p)).collect()
    }
}

/// Provisions the three topics.
pub async fn provision(
    backend: &dyn Backend,
    partitions: u32,
    max_age: Duration,
    max_bytes: u64,
) -> Result<Vec<Topic>, String> {
    let mut topics = Vec::new();
    for kind in Kind::ALL {
        let id = GtsInstanceId::try_new(&format!(
            "gts.cf.core.events.topic.v1~cf.longhaul.x.{}.v1",
            kind.name()
        ))
        .map_err(|err| format!("topic id: {err}"))?;
        let retention = match kind {
            Kind::Age => Retention {
                max_age: Some(max_age),
                max_bytes: None,
            },
            Kind::Bytes => Retention {
                max_age: None,
                max_bytes: Some(max_bytes),
            },
            Kind::Keep => Retention {
                max_age: None,
                max_bytes: None,
            },
        };
        backend
            .provision(
                &TopicSpec {
                    topic: id.clone(),
                    partitions,
                    retention,
                },
                &LogClaim {
                    replacing: Replacing::Nothing,
                    new: LogId::new_v4(),
                },
            )
            .await
            .map_err(|err| format!("provision {}: {err}", kind.name()))?;
        let uuid = topic_uuid(&id);
        topics.push(Topic {
            kind,
            id,
            uuid,
            partitions,
        });
    }
    Ok(topics)
}

/// Key indexes grouped by the partition their key hashes to; a partition no
/// key reaches stays empty and is skipped.
pub fn keys_by_partition(keys: u32, partitions: u32) -> Vec<Vec<u32>> {
    let mut grouped = vec![Vec::new(); partitions as usize];
    for k in 0..keys {
        grouped[partition_for(&key(k), partitions).value() as usize].push(k);
    }
    grouped
}

pub fn key(k: u32) -> String {
    format!("lh-{k}")
}

/// What a reader checks an event against: who wrote it, for which key, and
/// that writer's count of events for that key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Stamp {
    pub w: u32,
    pub k: u32,
    pub n: u64,
}

/// One event of writer `stamp.w`, padded to `pad` bytes.
pub fn event(stamp: Stamp, pad: &str) -> KeyedEvent {
    KeyedEvent {
        key: key(stamp.k),
        event: Event {
            id: Uuid::new_v4(),
            type_id: GtsTypeId::new("gts.cf.core.events.event.v1~cf.longhaul.x.written.v1~"),
            tenant_id: Uuid::from_u128(1),
            source: "longhaul".to_owned(),
            subject: key(stamp.k),
            subject_type: GtsTypeId::new("gts.cf.longhaul.x.key.v1~"),
            occurred_at: chrono::Utc::now(),
            trace_parent: None,
            data: Some(serde_json::json!({ "w": stamp.w, "k": stamp.k, "n": stamp.n, "pad": pad })),
            partition: None,
            sequence: None,
            sequence_time: None,
            meta: None,
        },
    }
}

/// The stamp an event carries, if it is one of ours.
pub fn stamp_of(event: &Event) -> Option<Stamp> {
    serde_json::from_value(event.data.clone()?).ok()
}

/// splitmix64: cheap and good enough to pick keys and positions.
pub struct Rng(u64);

impl Rng {
    pub fn seeded() -> Self {
        Self(u64::try_from(Uuid::new_v4().as_u128() & u128::from(u64::MAX)).unwrap_or(1))
    }

    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// A value in `0..bound`; a zero bound acts as 1.
    pub fn below(&mut self, bound: u64) -> u64 {
        self.next_u64() % bound.max(1)
    }
}
