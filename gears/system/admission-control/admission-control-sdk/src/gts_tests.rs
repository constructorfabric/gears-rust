#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeSet;

use super::*;
use crate::models::{FailureCondition, PolicyReference, RefusalEvent, RefusalEventCause};
use time::OffsetDateTime;
use uuid::Uuid;

fn keys(v: &Value) -> BTreeSet<String> {
    v.as_object().unwrap().keys().cloned().collect()
}

#[test]
fn identifiers_are_valid_and_consistent() {
    assert_eq!(
        <AdmissionEnginePluginSpecV1 as gts::GtsSchema>::TYPE_ID,
        "gts.cf.toolkit.plugins.plugin.v1~cf.core.admission_control.engine.v1~"
    );
    for type_id in [ADMISSION_CONTROL_RESOURCE, REFUSAL_EVENT_TYPE] {
        assert!(type_id.ends_with('~'), "{type_id} is not a type id");
        gts::GtsId::try_new(type_id).unwrap();
    }
    gts::GtsId::try_new(AUDIT_TOPIC_ID).unwrap();
    assert!(!AUDIT_TOPIC_ID.ends_with('~'));
}

#[test]
fn audit_topic_instance_is_in_inventory() {
    let instances = toolkit_gts::all_inventory_instances().unwrap();
    assert!(instances.iter().any(|i| i["id"] == AUDIT_TOPIC_ID));
}

#[test]
fn event_type_schema_names_topic_and_type() {
    let schema = refusal_event_type_schema();
    assert!(
        schema["$id"]
            .as_str()
            .unwrap()
            .ends_with(REFUSAL_EVENT_TYPE)
    );
    assert_eq!(schema["x-gts-traits"]["topic"], AUDIT_TOPIC_ID);
}

#[test]
fn refusal_schema_matches_serialized_field_set() {
    let event = RefusalEvent {
        correlation_id: Uuid::from_u128(1),
        occurred_at: OffsetDateTime::UNIX_EPOCH,
        enforcing_gear: "g".to_owned(),
        action: "a".to_owned(),
        resource_type: "t".to_owned(),
        resource_id: Some(Uuid::from_u128(2)),
        resource_tenant_id: Uuid::from_u128(3),
        subject_id: Uuid::from_u128(4),
        subject_tenant_id: Uuid::from_u128(5),
        enforced: true,
        cause: RefusalEventCause::Policy,
        builtin_policy_id: Some("p".to_owned()),
        condition: Some(FailureCondition::Internal),
        policy: Some(PolicyReference {
            bundle_id: Uuid::from_u128(6),
            version_id: Uuid::from_u128(7),
            document_id: Uuid::from_u128(8),
            document_name: "d".to_owned(),
        }),
        property_names: Vec::new(),
    };
    let json = serde_json::to_value(&event).unwrap();
    let schema = refusal_event_data_schema();
    assert_eq!(keys(&json), keys(&schema["properties"]));
    assert_eq!(
        keys(&json["policy"]),
        keys(&schema["properties"]["policy"]["properties"])
    );
    for required in schema["required"].as_array().unwrap() {
        assert!(json.get(required.as_str().unwrap()).is_some());
    }
}

#[test]
fn resource_type_schema_is_in_inventory_under_its_constant() {
    use gts::GtsSchema;
    assert_eq!(AdmissionResourceV1::TYPE_ID, ADMISSION_CONTROL_RESOURCE);
    let schemas = toolkit_gts::all_inventory_type_schemas().unwrap();
    assert!(
        serde_json::to_string(&schemas)
            .unwrap()
            .contains(ADMISSION_CONTROL_RESOURCE)
    );
}
