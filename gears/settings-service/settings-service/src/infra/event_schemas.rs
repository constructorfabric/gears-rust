//! The `cache_invalidate` broadcast: its body and the documents the Types
//! Registry holds for it. The body and its schemas live in the gear; only the
//! three identifiers (`ACTIVATION_SUBJECT_TYPE`, `CACHE_TOPIC_ID`,
//! `CACHE_INVALIDATE_EVENT_TYPE`) are part of the SDK, in `settings_service_sdk::gts`.
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
    event_broker_sdk::gts::derived_event_type_schema(
        CACHE_INVALIDATE_EVENT_TYPE,
        CACHE_TOPIC_ID,
        cache_invalidation_data_schema(),
        &[ACTIVATION_SUBJECT_TYPE],
    )
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

/// One key of a [`ChangeNotification`], with the change that owes a report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotifiedKey {
    /// The setting key.
    pub key: String,
    /// The change the consumer reports on: the id of the change's audit record.
    pub change_ref: Uuid,
}

/// The `data` of a `change_notification` event.
///
/// **Identifiers only.** The tenant, the subscriber and the keys with their
/// `change_ref`; there is no field for a value, an expected value, a hash, a
/// secret or an actor, and none may be added: the consumer re-reads under its
/// own identity and reports what it applied. Unknown attributes are ignored on
/// receipt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChangeNotification {
    /// The subscription tenant, a bare lowercase UUID string; the root tenant's
    /// id for a platform-wide subscription.
    pub tenant: String,
    /// The subscriber the notification is for: the broker delivers by topic,
    /// type and tenant only, so a consumer drops what names another.
    pub subscriber: String,
    /// The subscriber's `awaiting` keys at that tenant, in key order.
    pub changes: Vec<NotifiedKey>,
    /// The subject's type, for a subject-scoped change. Absent in v1.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject_type: Option<String>,
    /// The subject's runtime id, for a subject-scoped change. Absent in v1.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject_id: Option<String>,
}

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
    event_broker_sdk::gts::derived_event_type_schema(
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
