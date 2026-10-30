//! Tests for the declaration wire shape.

use serde_json::json;

use super::{CreateDeclarationRequest, DeclarationDto, UpdateDeclarationRequest};
use crate::domain::declaration::service::RenderedDeclaration;
use crate::domain::declaration::{Declaration, DeclarationStatus};
use uuid::Uuid;

fn rendered(traits: serde_json::Value) -> RenderedDeclaration {
    RenderedDeclaration {
        declaration: Declaration {
            id: Uuid::nil(),
            key: "gts.cf.core.settings.setting_type.v1~acme.settings.network.proxy.v1~".to_owned(),
            leaf_slug: "proxy".to_owned(),
            value_type_id: "gts.cf.core.settings.type_bool_flag.v1~".to_owned(),
            category_id: Uuid::nil(),
            scope_class: "local".to_owned(),
            mode: "standard".to_owned(),
            status: DeclarationStatus::Active,
            domain_affinity: None,
            licence_feature: None,
            owner_module: None,
            display_name: None,
            description: None,
            default_value: json!(true),
            has_secret_trait: false,
            data_classification: "public".to_owned(),
            requires_step_up: true,
            anonymous_exposable: false,
            source: "admin_authored".to_owned(),
            last_change_at: time::OffsetDateTime::UNIX_EPOCH,
            updated_at: time::OffsetDateTime::UNIX_EPOCH,
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
    assert_eq!(dto.value_type_id, "gts.cf.core.settings.type_bool_flag.v1~");
    assert!(!dto.key.contains(&dto.value_type_id));
}

#[test]
fn the_flags_default_and_classification_travel_with_the_declaration() {
    let dto = DeclarationDto::from(rendered(json!({})));
    assert_eq!(dto.default_value, json!(true));
    assert_eq!(dto.data_classification, "public");
    assert!(dto.requires_step_up && !dto.anonymous_exposable && !dto.has_secret_trait);
    assert_eq!(dto.source, "admin_authored");
    assert_eq!(dto.last_change_at, "1970-01-01T00:00:00Z");
    assert_eq!(dto.etag, "0", "the epoch's tag");
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

#[test]
fn a_display_name_is_serialized_when_set_and_omitted_when_not() {
    // Absent means absent: a client derives its own label from `leaf_slug`, so
    // the wire must not carry a `null` or an empty string in its place.
    let mut with = rendered(json!({}));
    with.declaration.display_name = Some("Minimum TLS version".to_owned());
    let wire = serde_json::to_value(DeclarationDto::from(with)).expect("serializes");
    assert_eq!(wire["display_name"], json!("Minimum TLS version"));

    let without =
        serde_json::to_value(DeclarationDto::from(rendered(json!({})))).expect("serializes");
    assert!(
        without.get("display_name").is_none(),
        "an unset display name has no key: {without}"
    );
}

#[test]
fn a_create_request_accepts_a_display_name_and_hands_it_to_the_domain() {
    let body = r#"{"value_type_id": "gts.cf.core.settings.type_bool_flag.v1~",
        "vendor": "acme", "name": "tls", "category_id": "00000000-0000-0000-0000-000000000000",
        "default_value": true, "scope_class": "cascading",
        "display_name": "Minimum TLS version"}"#;
    let request: CreateDeclarationRequest = serde_json::from_str(body).expect("accepted");
    assert_eq!(request.display_name.as_deref(), Some("Minimum TLS version"));
    let domain = crate::domain::declaration::CreateDeclaration::try_from(request)
        .expect("a well-formed request converts");
    assert_eq!(domain.display_name.as_deref(), Some("Minimum TLS version"));
}

#[test]
fn a_create_request_without_a_display_name_or_with_null_reads_as_unsupplied() {
    // An explicit `null` on create counts as unsupplied, the same as omission.
    for tail in ["", r#", "display_name": null"#] {
        let body = format!(
            r#"{{"value_type_id": "gts.cf.core.settings.type_bool_flag.v1~",
            "vendor": "acme", "name": "tls", "category_id": "00000000-0000-0000-0000-000000000000",
            "default_value": true, "scope_class": "cascading"{tail}}}"#
        );
        let request: CreateDeclarationRequest = serde_json::from_str(&body).expect("accepted");
        assert!(request.display_name.is_none(), "{tail}");
    }
}

#[test]
fn an_update_request_documents_the_display_name_and_reads_an_absent_one_as_absent() {
    // The handler reads the raw object for absent-versus-null; this shape only
    // publishes the field.
    let request: UpdateDeclarationRequest =
        serde_json::from_value(json!({ "display_name": "Minimum TLS version" })).expect("accepted");
    assert_eq!(request.display_name.as_deref(), Some("Minimum TLS version"));
    let empty: UpdateDeclarationRequest = serde_json::from_value(json!({})).expect("accepted");
    assert!(empty.display_name.is_none(), "an absent field stays absent");
}
