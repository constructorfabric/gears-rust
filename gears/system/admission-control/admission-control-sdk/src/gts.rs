//! GTS identifiers and schemas owned by admission-control.
//!
//! Compile-time entries reach `types-registry` through the `toolkit-gts`
//! crate-local declarations: the engine plugin specification, the admission
//! resource type, shared audit topic instance and derived refusal event type.
//! Every declaration also enters the process-wide inventory: the event broker
//! still reads that independent catalogue. The generated refusal schema uses
//! an explicit declaration record because it has no matching Rust GTS struct.

use gts::GtsInstanceId;
use schemars::generate::{Contract, SchemaSettings};
use serde_json::Value;
use toolkit_gts::local::{gts_instance, gts_type_schema};
use toolkit_gts::{PluginV1, gts_id};

use event_broker_sdk::gts::TopicV1;

use crate::models::RefusalEvent;

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

/// `data` schema of a refusal event, generated from [`RefusalEvent`] as it
/// serializes, so the schema cannot drift from the payload. Subschemas are
/// inlined, since the schema is embedded in the event-type document where a
/// `$ref` to generated definitions would not resolve. Unknown properties are
/// allowed so fields can be added within a major version.
#[must_use]
pub fn refusal_event_data_schema() -> Value {
    let mut settings = SchemaSettings::draft07();
    settings.inline_subschemas = true;
    settings.meta_schema = None;
    settings.contract = Contract::Serialize;
    settings
        .into_generator()
        .into_root_schema_for::<RefusalEvent>()
        .to_value()
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

// This schema is generated from the event payload instead of a GTS struct.
// Declare it in both catalogues exactly as the local GTS macros do: runtime
// publication owns the SDK declarations, while inventory-based broker readers
// need the same static schema without an init-time registration call.
const REFUSAL_SCHEMA_DECLARATION: toolkit_gts::InventoryTypeSchema =
    toolkit_gts::InventoryTypeSchema {
        type_id: REFUSAL_EVENT_TYPE,
        schema_fn: refusal_event_type_schema_string,
    };

toolkit_gts::inventory::submit! { REFUSAL_SCHEMA_DECLARATION }
toolkit_gts::inventory::submit! {
    crate::__toolkit_gts_inventory::TypeSchema(REFUSAL_SCHEMA_DECLARATION)
}

fn refusal_event_type_schema_string() -> String {
    refusal_event_type_schema().to_string()
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "gts_tests.rs"]
mod gts_tests;
