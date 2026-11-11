//! The `cache_invalidate` broadcast: its body and the documents the Types
//! Registry holds for it. The body and its schemas live in the gear; only the
//! three identifiers (`ACTIVATION_SUBJECT_TYPE`, `CACHE_TOPIC_ID`,
//! `CACHE_INVALIDATE_EVENT_TYPE`) are part of the SDK, in `settings_service_sdk::gts`.
//!
//! The `change_notification` body is not defined here: it is the SDK's
//! (`settings_service_sdk::activation::ChangeNotification`), re-exported below
//! so the publisher and the consuming client read and write one definition;
//! only its event-type schema is built here.
//!
//! Every replica of the gear publishes one [`CacheInvalidation`] per tenant a
//! request touched on the cache topic and consumes the ones its peers publish.
//! The body is a thin event (ADR-003): it names *what* changed, never *to
//! what*, so a replica re-reads through its ordinary authorization.
//!
//! The event type is a derived schema of the broker's event base and the topic
//! an instance of its topic base; the broker registers the bases, so the gear
//! registers these two once the broker resolves. The activation subject type is
//! this gear's own control-plane type and reaches the registry through the
//! link-time inventory, ahead of the gear's init, like the `setting_type` base.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;

use settings_service_sdk::gts::{
    ACTIVATION_SUBJECT_TYPE, ACTIVATION_TOPIC_ID, CACHE_INVALIDATE_EVENT_TYPE, CACHE_TOPIC_ID,
    CHANGE_NOTIFICATION_EVENT_TYPE,
};

/// The `data` of a `cache_invalidate` event.
///
/// **Identifiers only.** There is no field for a setting value, a secret, a
/// secret reference or an actor, and none may be added: the broadcast reaches
/// every replica and the broker's retention, so what it carries must be safe
/// to read there. Unknown attributes are ignored on receipt, so an additive
/// attribute of the same event type is harmless to an older replica.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CacheInvalidation {
    /// The correlation id of the request that changed the values — or the id a
    /// reconcile pass of contributed declarations mints once per pass.
    pub change_set_id: Uuid,
    /// The tenant this broadcast covers, a bare lowercase UUID string; the root
    /// tenant's id for a platform-wide change.
    pub tenant: String,
    /// The full set of the request's changed setting keys at that tenant.
    pub changed_keys: Vec<String>,
    /// The subject's type, for a subject-scoped change. Absent in v1.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject_type: Option<String>,
    /// The subject's runtime id, for a subject-scoped change. Absent in v1.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject_id: Option<String>,
}

/// The `data` schema of [`CacheInvalidation`]. Unknown properties are allowed
/// so attributes can be added within a major version.
fn cache_invalidation_data_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "change_set_id": { "type": "string", "format": "uuid" },
            "tenant": { "type": "string" },
            "changed_keys": { "type": "array", "items": { "type": "string" } },
            "subject_type": { "type": "string" },
            "subject_id": { "type": "string" }
        },
        "required": ["change_set_id", "tenant", "changed_keys"]
    })
}

/// The derived event-type schema of [`CACHE_INVALIDATE_EVENT_TYPE`] on
/// [`CACHE_TOPIC_ID`]. It keeps the broker's default partition key
/// (`/tenant_id`) and names [`ACTIVATION_SUBJECT_TYPE`] as its only allowed
/// subject type.
#[must_use]
pub fn cache_invalidate_event_type_schema() -> Value {
    derived_event_type_schema(
        CACHE_INVALIDATE_EVENT_TYPE,
        CACHE_TOPIC_ID,
        cache_invalidation_data_schema(),
        &[ACTIVATION_SUBJECT_TYPE],
    )
}

/// The event base's default partition key: the event's tenant.
const TENANT_POINTER: &str = "/tenant_id";

/// Assembles the derived event-type schema document for `type_id`: the payload
/// contract as a narrowing of the base's `data` member, and the governing
/// metadata as `x-gts-traits`. The broker's SDK used to provide this; it now
/// declares event types through `#[gts_type_schema(base = EventV1, ..)]`, and
/// this gear keeps its payload schema hand-written, so it assembles the same
/// document itself.
///
/// A `data_schema` with no constraints of its own is rendered as the base's
/// `["object", "null"]` rather than as an empty schema: an empty schema admits
/// any JSON value, which is wider than the base's member, and a derived type
/// may only narrow.
fn derived_event_type_schema(
    type_id: &str,
    topic: &str,
    data_schema: Value,
    allowed_subject_types: &[&str],
) -> Value {
    json!({
        "$id": format!("{}{type_id}", toolkit_gts::GTS_ID_URI_PREFIX),
        "$schema": "http://json-schema.org/draft-07/schema#",
        "x-gts-traits": {
            "topic": topic,
            "allowed_subject_types": allowed_subject_types,
            "partition_key": TENANT_POINTER,
        },
        "type": "object",
        "allOf": [
            { "$ref": toolkit_gts::gts_uri!("cf.core.events.event.v1~") },
            {
                "type": "object",
                "properties": {
                    "data": if data_schema.as_object().is_some_and(serde_json::Map::is_empty) {
                        json!({ "type": ["object", "null"] })
                    } else {
                        data_schema
                    },
                },
            },
        ],
    })
}

/// The cache topic as an instance of the broker's topic base, ready to
/// register. Retention is left to the broker's configured default.
#[must_use]
pub fn cache_topic_instance() -> Value {
    json!({
        "id": CACHE_TOPIC_ID,
        "description": "Settings Service replica cache coherence: cache_invalidate broadcasts between the service's own replicas",
    })
}

// The body is the SDK's: the publishing service and the consuming SDK read and
// write one definition (`settings_service_sdk::activation`), identifiers only.
pub use settings_service_sdk::activation::{ChangeNotification, NotifiedKey};

/// The `data` schema of [`ChangeNotification`]. Unknown properties are allowed
/// so attributes can be added within a major version.
fn change_notification_data_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "tenant": { "type": "string" },
            "subscriber": { "type": "string" },
            "changes": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "key": { "type": "string" },
                        "change_ref": { "type": "string", "format": "uuid" }
                    },
                    "required": ["key", "change_ref"]
                }
            },
            "subject_type": { "type": "string" },
            "subject_id": { "type": "string" }
        },
        "required": ["tenant", "subscriber", "changes"]
    })
}

/// The derived event-type schema of [`CHANGE_NOTIFICATION_EVENT_TYPE`] on
/// [`ACTIVATION_TOPIC_ID`]. It keeps the broker's default partition key
/// (`/tenant_id`) and names [`ACTIVATION_SUBJECT_TYPE`] as its only allowed
/// subject type.
#[must_use]
pub fn change_notification_event_type_schema() -> Value {
    derived_event_type_schema(
        CHANGE_NOTIFICATION_EVENT_TYPE,
        ACTIVATION_TOPIC_ID,
        change_notification_data_schema(),
        &[ACTIVATION_SUBJECT_TYPE],
    )
}

/// The activation topic as an instance of the broker's topic base, ready to
/// register. Retention is left to the broker's configured default.
#[must_use]
pub fn activation_topic_instance() -> Value {
    json!({
        "id": ACTIVATION_TOPIC_ID,
        "description": "Settings Service consumer activation: filtered change_notification events for the gears that subscribed to a setting",
    })
}

// @cpt-begin:cpt-cf-settings-service-flow-replica-coherence-lifecycle:p1:inst-rc-lifecycle-1
/// The schema of the activation signal subject: a control-plane type naming
/// the entity a signal is about. The body is `id`-only; the registry needs the
/// type identifier known, because a signal's `subject_type` names it.
#[must_use]
pub fn activation_subject_type_schema() -> Value {
    json!({
        "$id": format!("gts://{ACTIVATION_SUBJECT_TYPE}"),
        "$schema": "http://json-schema.org/draft-07/schema#",
        "description": "Subject type of the Settings Service activation signals: a signal describes a tenant's settings, not one key.",
        "type": "object",
        "properties": { "id": { "type": "string" } },
        "required": ["id"]
    })
}

// Registered through the link-time inventory, as the `setting_type` base is:
// the Types Registry drains it when it initializes, before this gear, so the
// subject type exists before the broker resolves and the event type that names
// it registers. Gear init calls nothing and never waits.
toolkit_gts::inventory::submit! {
    toolkit_gts::InventoryTypeSchema {
        type_id: ACTIVATION_SUBJECT_TYPE,
        schema_fn: || activation_subject_type_schema().to_string(),
    }
}
// @cpt-end:cpt-cf-settings-service-flow-replica-coherence-lifecycle:p1:inst-rc-lifecycle-1

#[cfg(test)]
#[path = "event_schemas_tests.rs"]
mod event_schemas_tests;
