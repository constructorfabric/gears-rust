#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::*;
use crate::models::{FailureCondition, PolicyReference, RefusalEvent, RefusalEventCause};
use crate::test_support::variants;
use uuid::Uuid;

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

fn event() -> RefusalEvent {
    RefusalEvent {
        enforcing_gear: "g".to_owned(),
        action: "a".to_owned(),
        resource_type: "t".to_owned(),
        resource_id: None,
        subject_id: Uuid::from_u128(4),
        subject_tenant_id: Uuid::from_u128(5),
        enforced: true,
        cause: RefusalEventCause::RequestTooLarge,
        condition: None,
        policy: None,
        property_names: Vec::new(),
    }
}

/// Turns a valid serialized event into an invalid one.
type Mutation = fn(&mut Value);

fn validator() -> jsonschema::Validator {
    let mut store = gts::GtsStore::new();
    store
        .register_schema(
            EventV1::<()>::TYPE_ID,
            &EventV1::<()>::gts_schema_with_refs(),
        )
        .unwrap();
    store
        .register_schema(REFUSAL_EVENT_TYPE, &RefusalEventV1::gts_schema_with_refs())
        .unwrap();
    let resolved = store
        .validate_schema_with(REFUSAL_EVENT_TYPE, gts::GtsRefValidation::None)
        .unwrap();
    jsonschema::validator_for(&event_broker_sdk::gts::data_contract(&resolved.schema)).unwrap()
}

#[test]
fn every_event_shape_validates_against_the_data_schema() {
    let validator = validator();
    let policy = PolicyReference {
        bundle_id: Uuid::from_u128(6),
        version_id: Uuid::from_u128(7),
        document_id: Uuid::from_u128(8),
        document_name: "d".to_owned(),
    };
    let mut events = vec![
        event(),
        RefusalEvent {
            resource_id: Some(Uuid::from_u128(2)),
            cause: RefusalEventCause::Policy,
            policy: Some(policy.clone()),
            property_names: vec!["name".to_owned()],
            ..event()
        },
        RefusalEvent {
            enforced: false,
            cause: RefusalEventCause::Policy,
            policy: Some(policy),
            ..event()
        },
        RefusalEvent {
            cause: RefusalEventCause::InvalidRequest,
            property_names: vec!["name".to_owned()],
            ..event()
        },
    ];
    events.extend(
        variants::<FailureCondition>()
            .into_iter()
            .map(|condition| RefusalEvent {
                cause: RefusalEventCause::CouldNotRun,
                condition: Some(condition),
                ..event()
            }),
    );
    for event in events {
        let json = serde_json::to_value(&event).unwrap();
        let errors: Vec<String> = validator
            .iter_errors(&json)
            .map(|e| e.to_string())
            .collect();
        assert!(errors.is_empty(), "{json}: {errors:?}");
    }
}

#[test]
fn the_data_schema_refuses_what_no_event_serializes_to() {
    let validator = validator();
    let valid = serde_json::to_value(event()).unwrap();
    assert!(validator.is_valid(&valid));
    let mutations: [(&str, Mutation); 9] = [
        ("unknown cause", |e| e["cause"] = "bogus".into()),
        ("unknown condition", |e| e["condition"] = "bogus".into()),
        ("null optional", |e| e["resource_id"] = Value::Null),
        ("missing required field", |e| {
            e.as_object_mut().unwrap().remove("enforcing_gear");
        }),
        ("mistyped field", |e| e["enforced"] = "yes".into()),
        ("incomplete policy", |e| {
            e["policy"] = serde_json::json!({ "bundle_id": Uuid::nil() });
        }),
        ("property values instead of names", |e| {
            e["property_names"] = serde_json::json!([{ "name": "x" }]);
        }),
        ("undeclared property", |e| {
            e["property_value"] = "secret".into();
        }),
        ("null data", |e| *e = Value::Null),
    ];
    for (case, mutate) in mutations {
        let mut invalid = valid.clone();
        mutate(&mut invalid);
        assert!(!validator.is_valid(&invalid), "{case}: {invalid}");
    }
}

#[test]
fn the_macro_registers_exactly_one_refusal_schema() {
    let entries: Vec<_> = toolkit_gts::inventory::iter::<toolkit_gts::InventoryTypeSchema>
        .into_iter()
        .filter(|entry| entry.type_id == REFUSAL_EVENT_TYPE)
        .collect();
    let [entry] = entries.as_slice() else {
        panic!(
            "expected exactly one refusal schema, found {}",
            entries.len()
        );
    };
    assert_eq!(
        serde_json::from_str::<Value>(&(entry.schema_fn)()).unwrap(),
        RefusalEventV1::gts_schema_with_refs()
    );
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
