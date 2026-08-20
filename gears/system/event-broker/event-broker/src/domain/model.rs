//! Domain entities (`DESIGN.md` §3.1 Domain Model). Shapes only - no
//! persistence, validation, or transport concerns live here.

use std::collections::HashMap;
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use toolkit::domain_model;
use toolkit_gts::GtsInstanceId;
use uuid::Uuid;

/// `gts.cf.core.events.topic.v1~` - a named, partitioned event log.
///
/// `description`/`streaming`/`retention` are optional per
/// `docs/schemas/topic.v1.schema.json` (only `id`/`partitions` are
/// `required`) - `#[serde(default)]` lets a real minimal stored record
/// (omitting any of the three) deserialize without error
/// (`eb-event-type-enforcement`).
#[domain_model]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Topic {
    pub id: GtsInstanceId,
    #[serde(default)]
    pub description: Option<String>,
    pub partitions: i32,
    #[serde(default)]
    pub streaming: Option<JsonValue>,
    /// ISO 8601 duration (e.g. `"PT24H"`), per
    /// `docs/schemas/topic.v1.schema.json`'s `retention` field - a `String`,
    /// not a `JsonValue` (this field used to be typed `JsonValue`, which
    /// disagreed with the schema; fixed in `eb-event-type-enforcement`).
    #[serde(default)]
    pub retention: Option<String>,
    pub created_at: DateTime<Utc>,
}

/// `gts.cf.core.events.event_type.v1~` - schema and constraints for one
/// category of events within a topic.
///
/// `description` is optional per `docs/schemas/event_type.v1.schema.json`;
/// `allowed_subject_types` is `required` there (`eb-event-type-enforcement`
/// added it to the schema to match this struct/the REST DTO) and
/// deliberately has no `#[serde(default)]` - a stored record missing it
/// should fail to deserialize, not silently resolve to `[]`.
#[domain_model]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventType {
    pub id: GtsInstanceId,
    pub topic_id: GtsInstanceId,
    #[serde(default)]
    pub description: Option<String>,
    /// GTS **Type-id patterns** (e.g. `cf.core.events.subject_type.v1~cf.core.acm.*`)
    /// describing which subject types (themselves GTS Type ids naming a
    /// *kind* of entity, not an instance of one) this event type's events
    /// may declare - stays `Vec<String>` deliberately; `GtsInstanceId::try_new`
    /// rejects both wildcard segments and Type ids (trailing `~`). Validated
    /// as `gts::GtsIdPattern` at registration
    /// (`domain::specification::validate_allowed_subject_types`), matched
    /// against `event.subject_type` via `gts::GtsId::matches_pattern` at
    /// publish time (`domain::ingest::subject_type_allowed`).
    pub allowed_subject_types: Vec<String>,
    pub data_schema: JsonValue,
    pub created_at: DateTime<Utc>,
}

/// `gts.cf.core.events.event.v1~` - an immutable record in a
/// `(topic, partition)` log.
///
/// `Serialize`/`Deserialize` (unlike `event_broker_sdk::models::Event`,
/// which deliberately has none) - this is the exact type
/// `IngestServiceImpl::publish_event` serializes as the ingest outbox
/// payload (design.md D5): reusing this domain type directly means the
/// leased handler that later decodes it can call the existing
/// `domain::backend::to_sdk_event` conversion unchanged, instead of a
/// second, hand-mirrored envelope type duplicating every field.
#[domain_model]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
    pub id: Uuid,
    pub r#type: GtsInstanceId,
    pub topic: GtsInstanceId,
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
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Meta {
    pub version: i32,
    pub producer_id: Uuid,
    pub previous: i64,
    pub sequence: i64,
}

/// Engine-typed filter expression on one interest (`ADR-0005`).
#[domain_model]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FilterSpec {
    pub engine: String,
    pub expression: String,
}

/// One topic-anchored interest within a JOIN request/response
/// (`subscription.v1.schema.json`'s `Interest` definition): event-type
/// patterns (GTS wildcard rules) scoped to `topic`, with an optional
/// compiled filter. Lives here (not `domain/delivery.rs`) because it's part
/// of `Subscription`'s own persisted/echoed shape, not just a
/// `DeliveryService::join` operation parameter.
#[domain_model]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Interest {
    pub topic: GtsInstanceId,
    /// Authz-validated against the calling principal
    /// (`DeliveryServiceImpl::join`'s `tenant_authorized` call,
    /// `gears-rust#4516`) - a dedicated `authz-resolver` PEP check
    /// (`domain::authz::TENANT_SCOPE_RESOURCE`), not a `tenant-resolver-sdk`
    /// call.
    pub tenant_id: Uuid,
    /// GTS **wildcard patterns** (GTS §10 rules, `validate_type_pattern`),
    /// not concrete instance ids - stays `String` deliberately;
    /// `GtsInstanceId::try_new` rejects wildcard segments.
    pub types: Vec<String>,
    pub filter: Option<FilterSpec>,
}

/// `gts.cf.core.events.subscription.v1~` - ephemeral, in-cache consumer
/// instance.
#[domain_model]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Subscription {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub consumer_group: GtsInstanceId,
    pub client_agent: String,
    pub interests: Vec<Interest>,
    pub topics: Vec<GtsInstanceId>,
    pub assigned: Vec<Assignment>,
    /// `std::time::Duration` has no built-in `serde` impl - `Storage`'s
    /// `subscription` namespace (eb-single-process-implementation D2)
    /// stores this as JSON in `ClusterCacheV1`, so it round-trips through
    /// whole seconds via `serde_duration_secs` (a pure serialization helper,
    /// not an infra dependency - stays in `domain/` alongside the type that
    /// needs it).
    #[serde(with = "serde_duration_secs")]
    pub session_timeout: Duration,
    pub last_seen_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
}

#[domain_model]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Assignment {
    pub topic: GtsInstanceId,
    pub partition: i32,
    pub offset: i64,
    pub last_examined: i64,
}

/// Ephemeral, in-cache runtime state of a consumer group.
#[domain_model]
#[derive(Debug, Clone)]
pub struct GroupState {
    pub consumer_group: GtsInstanceId,
    pub topic: GtsInstanceId,
    pub per_member_filters: HashMap<Uuid, JsonValue>,
    pub active_members: HashMap<Uuid, Subscription>,
    pub topology_version: i64,
    pub owning_delivery_shard_id: String,
}

/// Ephemeral, in-cache group progress for one `(topic, partition)`.
#[domain_model]
#[derive(Debug, Clone)]
pub struct Cursor {
    pub topic: GtsInstanceId,
    pub consumer_group: GtsInstanceId,
    pub partition: i32,
    pub offset: i64,
}

/// Whether a `ConsumerGroup` was minted anonymously by `POST
/// /v1/consumer-groups` or pre-registered by name via the types registry
/// (`docs/openapi.yaml`'s `POST /v1/consumer-groups` description).
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConsumerGroupKind {
    Anonymous,
    Named,
}

/// `gts.cf.core.events.consumer_group.v1~` - a registered consumer group.
/// Membership (`GroupState::active_members`) is tracked separately, in
/// cache, not here. `tenant_id`/`owner_principal_id` are non-overridable -
/// captured from `SecurityContext` at create time, never from the request
/// body (`docs/schemas/consumer_group.v1.schema.json`).
#[domain_model]
#[derive(Debug, Clone)]
pub struct ConsumerGroup {
    pub id: GtsInstanceId,
    pub kind: ConsumerGroupKind,
    pub tenant_id: Uuid,
    pub owner_principal_id: Uuid,
    pub description: Option<String>,
    pub created_at: DateTime<Utc>,
}

/// Caller-suppliable fields for `DeliveryService::create_consumer_group`
/// (`docs/openapi.yaml`'s `POST /v1/consumer-groups`'s `requestBody` is
/// itself optional; `client_agent` is required only once a body is sent at
/// all - both fields are `None` when no body is sent).
#[domain_model]
#[derive(Debug, Clone, Default)]
pub struct ConsumerGroupCreateInput {
    /// Diagnostic hint surfaced in operational logs only - no broker-side
    /// semantic, not persisted on `ConsumerGroup`
    /// (`consumer_group.v1.schema.json`'s `CreateRequest.client_agent` doc
    /// comment).
    pub client_agent: Option<String>,
    pub description: Option<String>,
}

/// Backend segment manifest for one `(topic, partition)`
/// (`GET /v1/topics/segments`). `segments` entries are opaque per
/// `docs/openapi.yaml` - this in-memory backing reports the whole stored
/// range as one synthetic segment, which is honest for what it actually is
/// (no real segmented storage backend exists yet - `#4347`/`#4348`).
#[domain_model]
#[derive(Debug, Clone)]
pub struct TopicSegmentManifest {
    pub topic: GtsInstanceId,
    pub partition: i32,
    pub start_sequence: i64,
    pub end_sequence: i64,
    pub start_time: Option<DateTime<Utc>>,
    pub end_time: Option<DateTime<Utc>>,
    pub segments: Vec<JsonValue>,
}

/// `#[serde(with = "...")]` helper for `Subscription::session_timeout` -
/// `std::time::Duration` has no built-in `serde` support. Whole-second
/// precision only (matches every other duration-as-config-field in this
/// crate, e.g. `config::StreamingConfig::heartbeat_interval_secs`).
mod serde_duration_secs {
    use std::time::Duration;

    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(value: &Duration, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_u64(value.as_secs())
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Duration, D::Error> {
        Ok(Duration::from_secs(u64::deserialize(deserializer)?))
    }
}
