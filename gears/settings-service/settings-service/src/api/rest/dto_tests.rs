// Created: 2026-08-13 by Constructor Tech
//! Tests for the category wire shapes.
//!
//! Acceptance: FEATURE `category-management.md` §6 — the request and response
//! shapes are a contract consuming clients depend on.

use super::{CategoryDto, CreateCategoryRequest, UpdateCategoryRequest};
use crate::api::precondition::ETag;
use crate::domain::category::{Category, CategoryKey};
use crate::domain::error::DomainError;
use crate::field;
use uuid::Uuid;

fn category() -> Category {
    Category {
        id: Uuid::nil(),
        key: CategoryKey::parse("network").expect("valid"),
        name: "Network".to_owned(),
        description: None,
        domain_affinity: Some("infra".to_owned()),
        sort_order: 3,
        icon: None,
        etag: ETag::new("v1"),
    }
}

#[test]
fn the_response_never_carries_the_etag() {
    // The tag is a response header. Carrying it in the body too would give a
    // client two sources for one precondition, one of which it might send back
    // stale.
    let json = serde_json::to_string(&CategoryDto::from(category())).expect("serializes");
    assert!(!json.contains("etag"), "got `{json}`");
    assert!(!json.contains("v1"), "got `{json}`");
}

#[test]
fn absent_optionals_are_omitted_rather_than_null() {
    let json = serde_json::to_value(CategoryDto::from(category())).expect("serializes");
    assert!(json.get("description").is_none());
    assert!(json.get("icon").is_none());
    assert_eq!(json["domain_affinity"], "infra");
}

#[test]
fn the_wire_shape_is_snake_case() {
    // The platform convention, enforced by `#[toolkit_macros::api_dto]` rather
    // than chosen here -- every gear's DTOs serialize the same way, so a client
    // does not need to know which service it is talking to. Consuming clients
    // match on these names; a rename is a breaking change.
    let json = serde_json::to_value(CategoryDto::from(category())).expect("serializes");
    let mut fields: Vec<_> = json.as_object().expect("object").keys().cloned().collect();
    fields.sort();
    assert_eq!(
        fields,
        ["domain_affinity", "id", "key", "name", "sort_order"]
    );
}

#[test]
fn a_request_refuses_an_unknown_field() {
    // `deny_unknown_fields`: a mistyped `sortOrder` would otherwise be silently
    // dropped and the category created with a weight the caller never chose.
    let err = serde_json::from_value::<CreateCategoryRequest>(serde_json::json!({
        "key": "network", "name": "Network", "sort_orde": 5
    }))
    .expect_err("must not parse");
    assert!(err.to_string().contains("sort_orde"), "got `{err}`");
}

#[test]
fn sort_order_defaults_to_zero() {
    let req: CreateCategoryRequest =
        serde_json::from_value(serde_json::json!({ "key": "network", "name": "Network" }))
            .expect("parses");
    assert_eq!(req.sort_order, 0);
}

#[test]
fn a_request_validates_its_key_on_the_way_in() {
    let req = CreateCategoryRequest {
        key: "net/work".to_owned(),
        name: "Network".to_owned(),
        description: None,
        domain_affinity: None,
        sort_order: 0,
        icon: None,
    };
    match req.into_draft() {
        Err(DomainError::Validation { code, .. }) => {
            assert_eq!(code, field::CATEGORY_KEY_RESERVED_SEPARATOR);
        }
        other => panic!("expected a key violation, got {other:?}"),
    }
}

fn update_req(key: Option<&str>) -> UpdateCategoryRequest {
    UpdateCategoryRequest {
        key: key.map(str::to_owned),
        name: "Network".to_owned(),
        description: None,
        domain_affinity: None,
        sort_order: 0,
        icon: None,
    }
}

#[test]
fn an_update_carrying_a_key_is_refused_as_immutable() {
    // Not a parse failure: the field exists and is returned in every response,
    // so the caller must be told it may not change -- not that it is unknown.
    match update_req(Some("network")).into_patch() {
        Err(DomainError::Validation { field, code, .. }) => {
            assert_eq!(field, "key");
            assert_eq!(code, crate::field::CATEGORY_KEY_IMMUTABLE);
        }
        other => panic!("expected an immutability violation, got {other:?}"),
    }
}

#[test]
fn an_echoed_key_is_refused_just_the_same() {
    // Even a key identical to the stored one. The contract is that an update
    // carries no key at all; accepting an echo would make it depend on a value
    // the caller has no way to change.
    assert!(update_req(Some("network")).into_patch().is_err());
}

#[test]
fn an_update_without_a_key_yields_a_patch() {
    let patch = update_req(None).into_patch().expect("no key supplied");
    assert_eq!(patch.name, "Network");
}

#[test]
fn key_is_optional_on_the_wire() {
    // `deny_unknown_fields` is on, so this also pins that omitting `key`
    // deserializes rather than failing as a missing field.
    let req: UpdateCategoryRequest =
        serde_json::from_value(serde_json::json!({ "name": "Network" })).expect("parses");
    assert!(req.key.is_none());
}

#[test]
fn create_enforces_the_descriptive_bounds() {
    // Wiring, not the rule itself: `bounds` owns the rule and has its own tests.
    // What matters here is that the create path actually consults it.
    let req = CreateCategoryRequest {
        key: "network".to_owned(),
        name: "n".repeat(257),
        description: None,
        domain_affinity: None,
        sort_order: 0,
        icon: None,
    };
    match req.into_draft() {
        Err(DomainError::Validation { field, code, .. }) => {
            assert_eq!(field, "name");
            assert_eq!(code, crate::field::CATEGORY_NAME_LENGTH);
        }
        other => panic!("expected a name violation, got {other:?}"),
    }
}

#[test]
fn update_enforces_the_descriptive_bounds_too() {
    let mut req = update_req(None);
    req.description = Some("d".repeat(4097));
    match req.into_patch() {
        Err(DomainError::Validation { field, .. }) => assert_eq!(field, "description"),
        other => panic!("expected a description violation, got {other:?}"),
    }
}

#[test]
fn the_key_refusal_precedes_the_bounds_check() {
    // A body that breaks both must report the immutable key: it is a contract
    // violation, while an over-long name is merely a value the caller can fix.
    let mut req = update_req(Some("network"));
    req.name = "n".repeat(257);
    match req.into_patch() {
        Err(DomainError::Validation { code, .. }) => {
            assert_eq!(code, crate::field::CATEGORY_KEY_IMMUTABLE);
        }
        other => panic!("expected the key refusal, got {other:?}"),
    }
}
