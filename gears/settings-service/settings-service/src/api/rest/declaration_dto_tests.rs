// Created: 2026-08-26 by Constructor Tech
//! Tests for the declaration wire shape.

use serde_json::json;

use super::DeclarationDto;
use crate::domain::declaration::Declaration;
use crate::domain::declaration::service::RenderedDeclaration;
use uuid::Uuid;

fn rendered(traits: serde_json::Value) -> RenderedDeclaration {
    RenderedDeclaration {
        declaration: Declaration {
            id: Uuid::nil(),
            key: "gts.cf.core.settings.setting_type.v1~acme.settings.network.proxy.v1~".to_owned(),
            leaf_slug: "proxy".to_owned(),
            value_type_id: "gts.cf.toolkit.settings.type_bool_flag.v1~".to_owned(),
            category_id: Uuid::nil(),
            scope_class: "local".to_owned(),
            mode: "standard".to_owned(),
            status: "active".to_owned(),
            domain_affinity: None,
            licence_feature: None,
            owner_module: None,
            description: None,
            default_value: json!(true),
            has_secret_trait: false,
            data_classification: "public".to_owned(),
            requires_step_up: true,
            anonymous_exposable: false,
            source: "admin_authored".to_owned(),
        },
        traits,
    }
}

#[test]
fn the_wire_shape_is_snake_case() {
    // Applied by `api_dto` rather than chosen here, and asserted so a change to
    // the macro's convention is caught in this gear rather than by a client.
    let dto = DeclarationDto::from(rendered(json!({})));
    let wire = serde_json::to_value(dto).expect("serializes");
    for expected in ["value_type_id", "category_id", "leaf_slug", "scope_class"] {
        assert!(wire.get(expected).is_some(), "missing `{expected}`");
    }
}

#[test]
fn the_value_type_travels_beside_the_key() {
    // The key is a GTS type id under the settings base (ADR-002); the value type
    // is a separate fact of the declaration. A client learns it from this field,
    // not by splitting the key -- there is nothing in the key to split off.
    let dto = DeclarationDto::from(rendered(json!({})));
    assert_eq!(
        dto.value_type_id,
        "gts.cf.toolkit.settings.type_bool_flag.v1~"
    );
    assert!(!dto.key.contains(&dto.value_type_id));
}

#[test]
fn traits_are_carried_through_verbatim() {
    let dto = DeclarationDto::from(rendered(json!({ "secret": true, "unit": "ms" })));
    assert_eq!(dto.traits, json!({ "secret": true, "unit": "ms" }));
}

#[test]
fn an_unresolved_trait_set_is_an_empty_object_not_an_absent_field() {
    // Always present so a client renders one shape rather than branching on
    // whether the registry happened to answer.
    let dto = DeclarationDto::from(rendered(json!({})));
    let wire = serde_json::to_value(dto).expect("serializes");
    assert_eq!(wire.get("traits"), Some(&json!({})));
}

#[test]
fn absent_optionals_are_omitted_rather_than_null() {
    let dto = DeclarationDto::from(rendered(json!({})));
    let wire = serde_json::to_value(dto).expect("serializes");
    for omitted in [
        "domain_affinity",
        "licence_feature",
        "owner_module",
        "description",
    ] {
        assert!(wire.get(omitted).is_none(), "`{omitted}` must be omitted");
    }
}
