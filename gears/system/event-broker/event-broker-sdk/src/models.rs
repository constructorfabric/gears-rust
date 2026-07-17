use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::api::StorageBackendConfig;
use crate::ids::ConsumerGroupId;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Topic {
    pub id: String,
    pub description: Option<String>,
    pub partitions: u32,
    pub retention: Option<String>,
    pub streaming: Option<StorageBackendConfig>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventType {
    pub id: String,
    pub topic: String,
    pub description: Option<String>,
    pub data_schema: serde_json::Value,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConsumerGroup {
    pub id: ConsumerGroupId,
    pub tenant_id: Uuid,
    pub owner_principal_id: String,
    pub kind: ConsumerGroupKind,
    pub description: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ConsumerGroupKind {
    Named,
    Anonymous,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Subscription {
    pub id: crate::ids::SubscriptionId,
    pub consumer_group: ConsumerGroupId,
    pub assigned: Vec<PartitionAssignment>,
    pub topology_version: i64,
    pub expires_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct PartitionAssignment {
    pub topic_ix: u16,
    pub partition: u32,
}

#[derive(Debug, Clone)]
pub struct CreateConsumerGroupRequest {
    /// RFC 9110 User-Agent grammar; ASCII 1-256 bytes. Diagnostic only - no broker semantic.
    pub client_agent: String,
    pub description: Option<String>,
}

#[derive(Debug, Clone, Copy)]
pub struct PartitionRange {
    pub start_offset: Option<i64>,
    pub end_offset: Option<i64>,
    pub limit: u32,
}

#[derive(Debug, Clone)]
pub struct TopicSegment {
    pub topic: String,
    pub partition: u32,
    pub start_sequence: i64,
    pub end_sequence: i64,
    pub start_time: DateTime<Utc>,
    pub end_time: DateTime<Utc>,
    /// Backend-specific per-segment opaque entries. Required in the wire response envelope.
    pub segments: Vec<serde_json::Value>,
}

#[derive(Debug, Clone)]
pub struct PartitionLeader {
    pub partition: u32,
    pub endpoint: String,
}

/// Paginated result wrapper used by list endpoints (e.g. GET /v1/consumer-groups).
#[derive(Debug, Clone)]
pub struct Page<T> {
    pub items: Vec<T>,
    pub next_cursor: Option<String>,
    pub prev_cursor: Option<String>,
    pub limit: u32,
}

/// Query parameters for [`EventBroker::list_consumer_groups`](crate::api::EventBroker::list_consumer_groups).
/// Built fluently; `ConsumerGroupQuery::default()` requests the first page with the
/// broker's default limit and no filter/order.
///
/// ```ignore
/// let q = ConsumerGroupQuery::new().limit(50).filter("name eq 'orders'");
/// ```
#[derive(Debug, Clone, Default)]
pub struct ConsumerGroupQuery {
    /// Max items per page (broker default when unset).
    pub limit: Option<u32>,
    /// Opaque pagination cursor from a previous page's `next_cursor`.
    pub cursor: Option<String>,
    /// Filter expression (backend-defined grammar).
    pub filter: Option<String>,
    /// Ordering expression (backend-defined grammar).
    pub orderby: Option<String>,
}

impl ConsumerGroupQuery {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
    #[must_use]
    pub fn limit(mut self, limit: u32) -> Self {
        self.limit = Some(limit);
        self
    }
    #[must_use]
    pub fn cursor(mut self, cursor: impl Into<String>) -> Self {
        self.cursor = Some(cursor.into());
        self
    }
    #[must_use]
    pub fn filter(mut self, filter: impl Into<String>) -> Self {
        self.filter = Some(filter.into());
        self
    }
    #[must_use]
    pub fn orderby(mut self, orderby: impl Into<String>) -> Self {
        self.orderby = Some(orderby.into());
        self
    }
}

/// Scope of a producer chain reset
/// ([`EventBroker::reset_producer_chain`](crate::api::EventBroker::reset_producer_chain)).
/// Models the valid combinations directly - a partition reset always names its topic,
/// so "partition without topic" is unrepresentable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResetScope<'a> {
    /// Reset every (topic, partition) chain for the producer.
    AllTopics,
    /// Reset every partition chain under one topic.
    Topic(&'a str),
    /// Reset a single (topic, partition) chain.
    Partition { topic: &'a str, partition: u32 },
}

/// The event envelope. Matches `event.v1.schema.json` in the design and is the
/// parameter/return type on the public [`EventBroker`](crate::api::EventBroker)
/// (publish/storage side). Broker-stamped fields (`partition`, `sequence`,
/// `sequence_time`, `offset`, `offset_time`) are `None` on publish payloads; the
/// broker populates them on receipt.
///
/// This is a plain domain type with no serde derives - construct it via field
/// init. Wire (de)serialization is the transport's concern: the `outbox` async
/// producer round-trips it through its own `OutboxEvent` DTO, and an HTTP backend
/// owns its own wire mapping.
#[derive(Debug, Clone)]
pub struct Event {
    pub id: Uuid,
    pub type_id: String,
    pub topic: String,
    pub tenant_id: Uuid,
    pub source: String,
    pub subject: String,
    pub subject_type: String,
    pub partition_key: Option<String>,
    pub occurred_at: DateTime<Utc>,
    pub trace_parent: Option<String>,
    pub data: Option<serde_json::Value>,

    // Broker-stamped (readOnly on the wire; absent on publish)
    pub partition: Option<u32>,
    pub sequence: Option<i64>,
    pub sequence_time: Option<DateTime<Utc>>,
    pub offset: Option<i64>,
    pub offset_time: Option<DateTime<Utc>>,

    // Publisher-only (writeOnly; stripped on read)
    pub meta: Option<ProducerMeta>,
}

/// Publisher-only chain/idempotency metadata stamped onto an [`Event`] before
/// publish (`writeOnly`; the broker strips it on read).
#[derive(Debug, Clone)]
pub struct ProducerMeta {
    pub version: u8,
    pub producer_id: Option<uuid::Uuid>,
    pub previous: Option<i64>,
    pub sequence: Option<i64>,
    pub partition_hint: Option<u32>,
}
