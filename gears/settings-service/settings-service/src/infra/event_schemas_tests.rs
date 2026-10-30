//! Tests for the `cache_invalidate` broadcast contract.
//!
//! Acceptance criteria: FEATURE `replica-coherence.md` §6 — *no broadcast
//! carries a setting value, a secret, a secret reference or an actor; `data`
//! holds `change_set_id`, `tenant` and `changed_keys` only*, and the identifier
//! constants the broker, the registry and the grants name.

use serde_json::json;
use uuid::Uuid;

use settings_service_sdk::gts::{
    ACTIVATION_SUBJECT_TYPE, CACHE_INVALIDATE_EVENT_TYPE, CACHE_TOPIC_ID,
};

use super::{
    CacheInvalidation, activation_subject_type_schema, cache_invalidate_event_type_schema,
    cache_topic_instance,
};

fn body() -> CacheInvalidation {
    CacheInvalidation {
        change_set_id: Uuid::from_u128(0x1234),
        tenant: "00000000-0000-0000-0000-0000000000aa".to_owned(),
        changed_keys: vec![
            "gts.cf.core.settings.setting_type.v1~acme.settings.network.proxy.v1~".to_owned(),
        ],
        subject_type: None,
        subject_id: None,
    }
}

#[test]
fn the_ids_are_the_ones_the_designs_fix() {
    assert_eq!(
        CACHE_TOPIC_ID,
        "gts.cf.core.events.topic.v1~cf.core.settings.cache.v1"
    );
    assert_eq!(
        CACHE_INVALIDATE_EVENT_TYPE,
        "gts.cf.core.events.event.v1~cf.core.settings.cache_invalidate.v1~"
    );
    assert_eq!(
        ACTIVATION_SUBJECT_TYPE,
        "gts.cf.core.settings.activation.v1~"
    );
}

#[test]
fn the_body_round_trips() {
    let wire = serde_json::to_value(body()).expect("serializes");
    let back: CacheInvalidation = serde_json::from_value(wire).expect("deserializes");
    assert_eq!(back, body());
}

#[test]
fn the_body_holds_identifiers_only() {
    // The whole attribute set of the v1 body, the absent subject pair omitted:
    // a value-bearing field cannot be added without this test naming it.
    let wire = serde_json::to_value(body()).expect("serializes");
    let mut names: Vec<&str> = wire
        .as_object()
        .expect("an object")
        .keys()
        .map(String::as_str)
        .collect();
    names.sort_unstable();
    assert_eq!(names, ["change_set_id", "changed_keys", "tenant"]);
}

#[test]
fn the_optional_subject_pair_travels_when_present() {
    let mut with_subject = body();
    with_subject.subject_type = Some("gts.acme.core.thing.v1~".to_owned());
    with_subject.subject_id = Some("s-1".to_owned());
    let wire = serde_json::to_value(&with_subject).expect("serializes");
    assert_eq!(wire["subject_type"], "gts.acme.core.thing.v1~");
    assert_eq!(wire["subject_id"], "s-1");
    let back: CacheInvalidation = serde_json::from_value(wire).expect("deserializes");
    assert_eq!(back, with_subject);
}

#[test]
fn an_unknown_attribute_is_ignored_on_receipt() {
    let mut wire = serde_json::to_value(body()).expect("serializes");
    wire["added_later"] = json!("x");
    let back: CacheInvalidation = serde_json::from_value(wire).expect("deserializes");
    assert_eq!(back, body());
}

#[test]
fn the_event_type_schema_keeps_the_default_partition_key_and_names_the_subject_type() {
    let schema = cache_invalidate_event_type_schema();
    assert_eq!(
        schema["$id"],
        format!("gts://{CACHE_INVALIDATE_EVENT_TYPE}")
    );
    let traits = &schema["x-gts-traits"];
    assert_eq!(traits["topic"], CACHE_TOPIC_ID);
    assert_eq!(
        traits["allowed_subject_types"],
        json!([ACTIVATION_SUBJECT_TYPE])
    );
    assert_eq!(traits["partition_key"], "/tenant_id");
}

#[test]
fn the_data_schema_admits_the_body_it_describes() {
    let schema = cache_invalidate_event_type_schema();
    let data = &schema["allOf"][1]["properties"]["data"];
    let validator = jsonschema::validator_for(data).expect("a valid schema");
    let wire = serde_json::to_value(body()).expect("serializes");
    assert!(
        validator.is_valid(&wire),
        "the body must satisfy its schema"
    );
    let mut missing = wire.clone();
    missing.as_object_mut().expect("an object").remove("tenant");
    assert!(!validator.is_valid(&missing), "`tenant` is required");
    let mut added = wire;
    added["added_later"] = json!(1);
    assert!(
        validator.is_valid(&added),
        "additive attributes stay admitted"
    );
}

#[test]
fn the_topic_instance_names_the_cache_topic() {
    let topic = cache_topic_instance();
    assert_eq!(topic["id"], CACHE_TOPIC_ID);
    assert!(topic["description"].as_str().is_some_and(|d| !d.is_empty()));
}

#[test]
fn the_subject_type_schema_is_the_activation_type() {
    let schema = activation_subject_type_schema();
    assert_eq!(schema["$id"], format!("gts://{ACTIVATION_SUBJECT_TYPE}"));
}

#[test]
fn the_activation_subject_type_is_in_the_link_time_inventory() {
    // The inventory block is the registration: the Types Registry drains it at
    // its own init, so removing the block leaves the id out of this list.
    let schemas = toolkit_gts::all_inventory_type_schemas().expect("the inventory collects");
    let wanted = format!("gts://{ACTIVATION_SUBJECT_TYPE}");
    assert!(
        schemas.iter().any(|s| s["$id"] == wanted),
        "{ACTIVATION_SUBJECT_TYPE} is not in the link-time inventory"
    );
}
