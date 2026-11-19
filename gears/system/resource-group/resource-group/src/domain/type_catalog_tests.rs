//! RG type snapshots: schema compilation and the membership binding.

#![allow(clippy::expect_used, clippy::unwrap_used, reason = "test assertions")]

use resource_group_sdk::gts::MEMBERSHIP_TYPE_V1;
use serde_json::{Value, json};
use toolkit_gts::gts_id;
use types_registry_sdk::{GtsId, GtsTypeId, LifecycleStatus, TypeSchema};

use super::{RgType, compile, concrete_membership_type};
use crate::domain::error::DomainError;

const GROUP: &str = gts_id!("cf.core.rg.type.v1~cf.test.rg.team.v1~");
const MEMBERSHIP: &str = gts_id!("cf.core.rg.membership.v1~cf.test.rg.member.v1~");

fn snapshot(content: Value, resolved_schema: Value) -> TypeSchema {
    TypeSchema {
        type_id: GtsTypeId::try_new(GROUP).expect("type identifier"),
        type_uuid: GtsId::try_new(GROUP).expect("identifier").to_uuid(),
        lifecycle_status: LifecycleStatus::Active,
        origin: None,
        content: Some(content),
        resolved_schema: Some(resolved_schema),
        effective_traits: None,
        effective_traits_schema: None,
        provenance: None,
    }
}

fn binding(resolved_schema: Value) -> Result<String, DomainError> {
    RgType::from_schema(&snapshot(json!({}), resolved_schema))
        .expect("group schema compiles")
        .membership_type()
        .map(str::to_owned)
}

fn fixed(id: &str) -> Value {
    json!({ "properties": { "membership_type": { "const": id } } })
}

#[test]
fn an_abstract_type_is_the_callers_error() {
    let err = compile(&snapshot(json!({"x-gts-abstract": true}), json!({}))).unwrap_err();
    assert!(matches!(err, DomainError::Validation { .. }), "{err:?}");
}

#[test]
fn a_schema_gts_cannot_compile_is_internal() {
    for (content, resolved) in [
        (json!({}), json!({"$ref": "urn:missing:schema"})),
        (json!({}), Value::Null),
        (Value::Null, json!({})),
    ] {
        let err = compile(&snapshot(content, resolved)).unwrap_err();
        assert!(matches!(err, DomainError::InternalError), "{err:?}");
    }
}

#[test]
fn the_binding_is_a_const_on_the_schema_or_its_all_of_branches() {
    assert_eq!(binding(fixed(MEMBERSHIP)).unwrap(), MEMBERSHIP);
    assert_eq!(
        binding(json!({ "allOf": [{}, { "allOf": [fixed(MEMBERSHIP)] }] })).unwrap(),
        MEMBERSHIP
    );
}

#[test]
fn only_a_concrete_membership_schema_binds() {
    for id in [
        MEMBERSHIP_TYPE_V1,
        gts_id!("cf.test.rg.member.v1~"),
        "gts.cf.core.rg.membership.v1~not an id",
    ] {
        assert!(concrete_membership_type(id).is_none(), "{id}");
        let err = binding(fixed(id)).unwrap_err();
        assert!(
            matches!(err, DomainError::Validation { .. }),
            "{id}: {err:?}"
        );
    }
    assert!(concrete_membership_type(MEMBERSHIP).is_some());
}

#[test]
fn a_conditional_const_does_not_bind() {
    for resolved in [
        json!({ "anyOf": [fixed(MEMBERSHIP)] }),
        json!({ "oneOf": [fixed(MEMBERSHIP)] }),
        json!({ "if": {}, "then": fixed(MEMBERSHIP) }),
        json!({ "properties": { "membership_type": { "enum": [MEMBERSHIP] } } }),
    ] {
        binding(resolved.clone()).expect_err(&resolved.to_string());
    }
}
