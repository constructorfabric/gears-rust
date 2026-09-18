// Created: 2026-08-12 by Constructor Tech
//! Tests for the GTS resource-type vocabulary.

use super::{CATEGORY_SCHEMA, DECLARATION_SCHEMA, Resource, VALUE_SCHEMA};

#[test]
fn every_constant_round_trips_through_the_typed_view() {
    for wire in [DECLARATION_SCHEMA, VALUE_SCHEMA, CATEGORY_SCHEMA] {
        let typed = Resource::from_wire(wire);
        assert_eq!(
            typed.as_wire(),
            wire,
            "`{wire}` must survive a round trip through the typed view"
        );
        assert!(
            !matches!(typed, Resource::Unknown(_)),
            "`{wire}` is a modelled resource and must not fall through to Unknown"
        );
    }
}

#[test]
fn an_unmodelled_resource_is_preserved_not_discarded() {
    // A resource type this SDK does not know must survive intact, so a consumer
    // can still report it and a later version can model it without data loss.
    let typed = Resource::from_wire("gts.cf.core.settings.change_set.v1~");
    assert_eq!(
        typed,
        Resource::Unknown("gts.cf.core.settings.change_set.v1~".to_owned())
    );
    assert_eq!(typed.as_wire(), "gts.cf.core.settings.change_set.v1~");
}

#[test]
fn declaration_and_value_are_different_resources() {
    // The projection leans on this distinction to tell "no such setting" from
    // "no credential configured"; if these ever collapsed, that would break.
    assert_ne!(
        Resource::from_wire(DECLARATION_SCHEMA),
        Resource::from_wire(VALUE_SCHEMA)
    );
}

#[test]
fn the_base_schema_is_identified_by_the_wire_constant() {
    // Two spellings of one identifier: the constant callers read off policies
    // and audit records, and the `$id` the registry holds. If they drift, keys
    // parse against a base that is not the one registered.
    use super::{SETTING_TYPE_BASE, setting_type_base_schema};
    let schema = setting_type_base_schema();
    assert_eq!(
        schema["$id"],
        serde_json::json!(format!("gts://{SETTING_TYPE_BASE}"))
    );
    assert_eq!(schema["x-gts-abstract"], serde_json::json!(true));
    assert_eq!(
        schema["properties"]["payload"],
        serde_json::json!({}),
        "a derived type may narrow the payload to any shape"
    );
    assert!(
        schema.get("default").is_none(),
        "the Schema Default lives on the declaration, never in the type"
    );
}

#[test]
fn the_base_is_submitted_to_the_inventory() {
    // What the types registry drains at its init. Absent from the inventory,
    // no declaration could ever register a derived type.
    use super::SETTING_TYPE_BASE;
    let wanted = serde_json::json!(format!("gts://{SETTING_TYPE_BASE}"));
    assert!(
        toolkit_gts::all_inventory_type_schemas()
            .expect("inventory renders")
            .into_iter()
            .any(|schema| schema["$id"] == wanted),
        "the setting base is in the link-time inventory"
    );
}
