//! Domain entities (`DESIGN.md` §3.1 Domain Model). Shapes only - no
//! persistence, validation, or transport concerns live here.

use std::collections::HashMap;
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde_json::Value as JsonValue;
use toolkit::domain_model;
use uuid::Uuid;

/// `gts.cf.core.events.topic.v1~` - a named, partitioned event log.
#[domain_model]
#[derive(Debug, Clone)]
pub struct Topic {
    pub id: String,
    pub description: Option<String>,
    pub partitions: i32,
    pub streaming: JsonValue,
    pub retention: JsonValue,
    pub created_at: DateTime<Utc>,
}

/// `gts.cf.core.events.event_type.v1~` - schema and constraints for one
/// category of events within a topic.
#[domain_model]
#[derive(Debug, Clone)]
pub struct EventType {
    pub id: String,
    pub topic_id: String,
    pub description: Option<String>,
    pub allowed_subject_types: Vec<String>,
    pub data_schema: JsonValue,
    pub created_at: DateTime<Utc>,
}

/// `gts.cf.core.events.event.v1~` - an immutable record in a
/// `(topic, partition)` log.
#[domain_model]
#[derive(Debug, Clone)]
pub struct Event {
    pub id: Uuid,
    pub r#type: String,
    pub topic: String,
    pub partition_key: Option<String>,
    pub tenant_id: Uuid,
    pub source: String,
    pub subject: String,
    pub subject_type: String,
    pub occurred_at: DateTime<Utc>,
    pub trace_parent: Option<String>,
    pub data: JsonValue,
    /// Publish-input only; stripped on the read projection.
    pub meta: Option<Meta>,
    /// Read-projection only; broker-derived.
    pub partition: Option<i32>,
    /// Read-projection only; broker-logical consumer-visible ordering key.
    pub sequence: Option<i64>,
    pub sequence_time: Option<DateTime<Utc>>,
}

/// Producer chain metadata. Publish-input only; stripped on read.
#[domain_model]
#[derive(Debug, Clone)]
pub struct Meta {
    pub version: i32,
    pub producer_id: Uuid,
    pub previous: i64,
    pub sequence: i64,
}

/// `gts.cf.core.events.subscription.v1~` - ephemeral, in-cache consumer
/// instance.
#[domain_model]
#[derive(Debug, Clone)]
pub struct Subscription {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub consumer_group: String,
    pub topics: Vec<String>,
    pub assigned: Vec<Assignment>,
    pub session_timeout: Duration,
    pub last_seen_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
}

#[domain_model]
#[derive(Debug, Clone)]
pub struct Assignment {
    pub topic: String,
    pub partition: i32,
    pub offset: i64,
    pub last_examined: i64,
}

/// Ephemeral, in-cache runtime state of a consumer group.
#[domain_model]
#[derive(Debug, Clone)]
pub struct GroupState {
    pub consumer_group: String,
    pub topic: String,
    pub per_member_filters: HashMap<Uuid, JsonValue>,
    pub active_members: HashMap<Uuid, Subscription>,
    pub topology_version: i64,
    pub owning_delivery_shard_id: String,
}

/// Ephemeral, in-cache group progress for one `(topic, partition)`.
#[domain_model]
#[derive(Debug, Clone)]
pub struct Cursor {
    pub topic: String,
    pub consumer_group: String,
    pub partition: i32,
    pub offset: i64,
}
