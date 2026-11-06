#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::*;
use crate::models::{FailureCondition, PolicyReference, RefusalEvent, RefusalEventCause};
use crate::test_support::variants;
use event_broker_sdk::gts::data_contract;
use gts::GtsStore;
use uuid::Uuid;

#[test]
fn audit_topic_instance_is_in_inventory() {
    let instances = toolkit_gts::all_inventory_instances().unwrap();
    assert!(instances.iter().any(|i| i["id"] == AUDIT_TOPIC_ID));
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

/// The declared payload contract of the refusal event type, resolved against
/// the event base the way the registry resolves it.
fn data_schema() -> Value {
    let mut store = GtsStore::new();
    store
        .register_schema(
            EventV1::<()>::TYPE_ID,
            &EventV1::<()>::gts_schema_with_refs(),
        )
        .unwrap();
    let resolved = store
        .resolve_schema_refs(&refusal_event_type_schema())
        .unwrap();
    data_contract(&resolved)
}

fn validator() -> jsonschema::Validator {
    jsonschema::validator_for(&data_schema()).unwrap()
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
    let mutations: [(&str, Mutation); 7] = [
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
    ];
    for (case, mutate) in mutations {
        let mut invalid = valid.clone();
        mutate(&mut invalid);
        assert!(!validator.is_valid(&invalid), "{case}: {invalid}");
    }
}

#[test]
fn the_declaration_names_every_field_of_the_payload() {
    // The payload (`RefusalEvent`) is what is sent and the declaration is what is
    // registered; a field on only one side would otherwise go unnoticed when it
    // is optional.
    let full = RefusalEvent {
        resource_id: Some(Uuid::from_u128(2)),
        condition: Some(variants::<FailureCondition>().remove(0)),
        policy: Some(PolicyReference {
            bundle_id: Uuid::from_u128(6),
            version_id: Uuid::from_u128(7),
            document_id: Uuid::from_u128(8),
            document_name: "d".to_owned(),
        }),
        property_names: vec!["name".to_owned()],
        ..event()
    };
    let sent = serde_json::to_value(full).unwrap();
    let declared = RefusalEventV1::gts_schema_with_refs();
    let declared = declared["allOf"][1]["properties"]["data"]["properties"]
        .as_object()
        .unwrap();
    let mut sent_names: Vec<_> = sent.as_object().unwrap().keys().collect();
    let mut declared_names: Vec<_> = declared.keys().collect();
    sent_names.sort();
    declared_names.sort();
    assert_eq!(sent_names, declared_names);
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
