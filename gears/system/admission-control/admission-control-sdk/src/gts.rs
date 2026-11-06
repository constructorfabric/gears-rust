//! GTS identifiers and schemas owned by admission-control.
//!
//! Compile-time entries reach `types-registry` through the `toolkit-gts`
//! link-time inventory: the engine plugin specification, the admission
//! resource type, the shared audit topic instance and the refusal event type,
//! declared in Rust as [`RefusalEventV1`] under the broker's event base.

use gts::{GtsInstanceId, GtsSchema};
use serde_json::Value;
use toolkit_gts::{PluginV1, gts_id, gts_instance, gts_type_schema};
use uuid::Uuid;

use event_broker_sdk::gts::{EventV1, EventV1Traits, TopicV1};

use crate::models::{FailureCondition, PolicyReference, RefusalEventCause};

/// Error-family / resource-type identifier of an admission decision: the
/// resource type of the gear's canonical errors and the subject type of
/// refusal events.
pub const ADMISSION_CONTROL_RESOURCE: &str = gts_id!("cf.core.admission_control.admission.v1~");

/// The audit topic (an instance of the broker's topic base type).
pub const AUDIT_TOPIC_ID: &str =
    gts_id!("cf.core.events.topic.v1~cf.core.admission_control.audit.v1");

/// Event type of a [`RefusalEvent`](crate::models::RefusalEvent).
pub const REFUSAL_EVENT_TYPE: &str = <RefusalEventV1 as GtsSchema>::TYPE_ID;

/// GTS plugin specification for admission engines selectable by the gate.
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
    description = "Admission-control admission engine plugin specification",
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

crate::models::refusal_event_payload! {
    /// Declaration of the refusal event type under the broker's event base, on
    /// [`AUDIT_TOPIC_ID`] with subject type [`ADMISSION_CONTROL_RESOURCE`]; its
    /// fields are the event's `data` contract.
    ///
    /// This struct only declares the schema: a derived GTS struct has no `serde`
    /// impls of its own, and its generated serializer would write an absent optional
    /// as `null`, which the contract (absent, never `null`) refuses. The payload that
    /// is sent is [`RefusalEvent`](crate::models::RefusalEvent). Both types share
    /// the same field declaration and serde annotations. Unknown properties are
    /// not allowed at any level.
    #[gts_type_schema(
        dir_path = "schemas",
        base = EventV1,
        type_id = gts_id!("cf.core.events.event.v1~cf.core.admission_control.refusal.v1~"),
        description = "Admission-control refusal or shadow finding: one event per (operation, policy) pair. Property values are never carried, only names.",
        properties = "enforcing_gear,action,resource_type,resource_id,subject_id,subject_tenant_id,enforced,cause,condition,policy,property_names",
        traits = EventV1Traits::builder()
            .with_topic(AUDIT_TOPIC_ID)
            .with_allowed_subject_types([ADMISSION_CONTROL_RESOURCE])
            .build(),
    )]
    pub struct RefusalEventV1;
}

/// Derived event-type schema of [`REFUSAL_EVENT_TYPE`], as registered at runtime.
#[must_use]
pub fn refusal_event_type_schema() -> Value {
    RefusalEventV1::gts_schema_with_refs()
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "gts_tests.rs"]
mod gts_tests;
