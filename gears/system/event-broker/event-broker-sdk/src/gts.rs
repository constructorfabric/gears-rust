//! GTS schema definitions for the Event Broker.

use chrono::{DateTime, Utc};
use toolkit_gts::{GtsInstanceId, gts_id, gts_type_schema};

/// GTS base type for topics. Concrete topics are registered dynamically
/// against `types-registry` as instances chaining directly onto this base
/// (`gts.cf.core.events.topic.v1~<vendor>.<name>.v1`), not as compile-time
/// Rust types - this struct carries the full instance shape
/// (`docs/schemas/topic.v1.schema.json`) so those instances actually
/// validate; it previously declared only `id`, which meant no real topic
/// could ever be registered against a live `types-registry` (every existing
/// test bypasses real validation via `MockTypesRegistryClient`, so this went
/// unnoticed until the standalone binary was actually booted).
#[gts_type_schema(
    dir_path = "schemas",
    type_id = gts_id!("cf.core.events.topic.v1~"),
    description = "Event Broker topic",
    properties = "id,description,partitions,retention,streaming,created_at",
    base = true,
)]
pub struct TopicV1 {
    pub id: GtsInstanceId,
    pub description: Option<String>,
    pub partitions: i32,
    /// ISO 8601 duration string (e.g. `"PT24H"`) - see
    /// `domain::model::Topic::retention`'s own doc comment.
    pub retention: Option<String>,
    /// Opaque backend configuration - validated against the chosen
    /// backend's own `config_schema` at topic registration, not here.
    pub streaming: Option<serde_json::Value>,
    pub created_at: DateTime<Utc>,
}

/// GTS base type for event types. Same rationale as `TopicV1` - carries the
/// full instance shape (`docs/schemas/event_type.v1.schema.json`) instead of
/// `id` alone.
///
/// `topic_id` (not the wire schema's `topic`): matches
/// `domain::model::EventType::topic_id`'s own field name, which this
/// crate's `register_event_type`/`get_event_type` already read/write
/// directly (`serde_json::to_value`/`from_value` on the domain struct, no
/// remapping) - the schema must match what's actually sent, not the
/// separately-documented wire name. That doc-vs-domain naming mismatch is
/// pre-existing and tracked separately; not resolved here.
#[gts_type_schema(
    dir_path = "schemas",
    type_id = gts_id!("cf.core.events.event_type.v1~"),
    description = "Event Broker event type",
    properties = "id,topic_id,description,allowed_subject_types,data_schema,created_at",
    base = true,
)]
pub struct EventTypeV1 {
    pub id: GtsInstanceId,
    pub topic_id: GtsInstanceId,
    pub description: Option<String>,
    pub allowed_subject_types: Vec<String>,
    pub data_schema: serde_json::Value,
    pub created_at: DateTime<Utc>,
}

/// Consumer-group resource type - PEP/error matching only, no schema
/// generation (no `types-registry` inventory entry).
pub const CONSUMER_GROUP_RESOURCE_TYPE: &str = gts_id!("cf.core.events.consumer_group.v1~");
/// Subscription resource type - PEP/error matching only.
pub const SUBSCRIPTION_RESOURCE_TYPE: &str = gts_id!("cf.core.events.subscription.v1~");
/// Producer resource type - PEP/error matching only.
pub const PRODUCER_RESOURCE_TYPE: &str = gts_id!("cf.core.events.producer.v1~");
/// Generic fallback resource type for errors not tied to one entity kind
/// (mirrors `api/rest/error.rs::EventBrokerResourceError`'s own fallback).
pub const REQUEST_RESOURCE_TYPE: &str = gts_id!("cf.core.events.request.v1~");
