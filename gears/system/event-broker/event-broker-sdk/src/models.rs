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
    /// RFC 9110 User-Agent grammar; ASCII 1–256 bytes. Diagnostic only — no broker semantic.
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
