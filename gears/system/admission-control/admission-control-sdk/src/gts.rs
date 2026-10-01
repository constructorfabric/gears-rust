//! GTS identifiers and schemas owned by admission-control.
//!
//! Compile-time entries reach `types-registry` through the `toolkit-gts`
//! link-time inventory: the engine plugin specification, the admission
//! resource type and the shared audit topic instance. The refusal event type
//! is a derived schema under the broker's event base and is registered by the
//! gear at init from [`refusal_event_type_schema`].

use gts::GtsInstanceId;
use serde_json::{Value, json};
use toolkit_gts::{PluginV1, gts_id, gts_instance, gts_type_schema};

use event_broker_sdk::gts::TopicV1;

use crate::models::FailureCondition;

/// Error-family / resource-type identifier of an admission decision: the
/// resource type of the gear's canonical errors and the subject type of
/// refusal events.
pub const ADMISSION_CONTROL_RESOURCE: &str = gts_id!("cf.core.admission_control.admission.v1~");

/// The audit topic (an instance of the broker's topic base type).
pub const AUDIT_TOPIC_ID: &str =
    gts_id!("cf.core.events.topic.v1~cf.core.admission_control.audit.v1");

/// Event type of a [`RefusalEvent`](crate::models::RefusalEvent).
pub const REFUSAL_EVENT_TYPE: &str =
    gts_id!("cf.core.events.event.v1~cf.core.admission_control.refusal.v1~");

/// GTS plugin specification for policy engines selectable by the gate.
///
/// # Instance ID format
///
/// ```text
/// gts.cf.toolkit.plugins.plugin.v1~cf.core.admission_control.engine.v1~<vendor>.<package>.<name>.v1
/// ```
#[derive(Default)]
#[gts_type_schema(
    dir_path = "schemas",
    base = PluginV1,
    type_id = gts_id!("cf.toolkit.plugins.plugin.v1~cf.core.admission_control.engine.v1~"),
    description = "Admission Control policy engine plugin specification",
    properties = "",
)]
pub struct AdmissionEnginePluginSpecV1;

/// Type schema of [`ADMISSION_CONTROL_RESOURCE`]. The body is `id`-only: the
/// registry needs the type identifier known (canonical errors and refusal
/// event subjects name it).
#[gts_type_schema(
    dir_path = "schemas",
    base = true,
    type_id = gts_id!("cf.core.admission_control.admission.v1~"),
    description = "Admission-control admission decision: resource type of the gate's canonical errors and the subject type of refusal events",
    properties = "id",
)]
pub struct AdmissionResourceV1 {
    /// Required by the `gts-macros` base-struct contract; inert.
    pub id: GtsInstanceId,
}

gts_instance! {
    TopicV1 {
        id: gts_id!("cf.core.events.topic.v1~cf.core.admission_control.audit.v1"),
        description: "Admission audit stream: admission-control refusal and shadow-finding events".to_owned(),
        retention: None,
    }
}

fn uuid() -> Value {
    json!({ "type": "string", "format": "uuid" })
}

fn string() -> Value {
    json!({ "type": "string" })
}

/// `data` schema of a refusal event. Property **names** only; unknown
/// properties are allowed so fields can be added within a major version.
#[must_use]
pub fn refusal_event_data_schema() -> Value {
    let conditions: Vec<&str> = FailureCondition::ALL.iter().map(|c| c.as_str()).collect();
    json!({
        "type": "object",
        "required": [
            "correlation_id", "occurred_at", "enforcing_gear", "action", "resource_type",
            "resource_tenant_id", "subject_id", "subject_tenant_id", "enforced", "cause"
        ],
        "properties": {
            "correlation_id": uuid(),
            "occurred_at": { "type": "string", "format": "date-time" },
            "enforcing_gear": string(),
            "action": string(),
            "resource_type": string(),
            "resource_id": uuid(),
            "resource_tenant_id": uuid(),
            "subject_id": uuid(),
            "subject_tenant_id": uuid(),
            "enforced": { "type": "boolean" },
            "cause": {
                "type": "string",
                "enum": ["builtin_policy", "policy", "request_too_large", "could_not_run"]
            },
            "builtin_policy_id": string(),
            "condition": { "type": "string", "enum": conditions },
            "policy": {
                "type": "object",
                "required": ["bundle_id", "version_id", "document_id", "document_name"],
                "properties": {
                    "bundle_id": uuid(),
                    "version_id": uuid(),
                    "document_id": uuid(),
                    "document_name": string()
                }
            },
            "property_names": { "type": "array", "items": string() }
        }
    })
}

/// Derived event-type schema of [`REFUSAL_EVENT_TYPE`] on [`AUDIT_TOPIC_ID`],
/// subject type [`ADMISSION_CONTROL_RESOURCE`].
#[must_use]
pub fn refusal_event_type_schema() -> Value {
    event_broker_sdk::gts::derived_event_type_schema(
        REFUSAL_EVENT_TYPE,
        AUDIT_TOPIC_ID,
        refusal_event_data_schema(),
        &[ADMISSION_CONTROL_RESOURCE],
    )
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "gts_tests.rs"]
mod gts_tests;
