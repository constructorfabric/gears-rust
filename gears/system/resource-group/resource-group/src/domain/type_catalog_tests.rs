//! RG type snapshots: schema compilation and the membership binding.

#![allow(clippy::expect_used, clippy::unwrap_used, reason = "test assertions")]

use resource_group_sdk::gts::GROUP_MEMBERSHIP_TYPE_ID;
use serde_json::{Value, json};
use toolkit_gts::gts_id;
use types_registry_sdk::{GtsId, GtsTypeId, LifecycleStatus, TypeSchema};

use super::{RgType, compile, concrete_membership_type};
use crate::domain::error::DomainError;

const GROUP: &str = gts_id!("cf.core.rg.group.v1~cf.test.rg.team.v1~");
const MEMBERSHIP: &str = gts_id!("cf.core.rg.group_membership.v1~cf.test.rg.member.v1~");

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

fn binding(traits: Value) -> Result<String, DomainError> {
    let mut schema = snapshot(json!({}), json!({}));
    schema.effective_traits = Some(traits);
    RgType::from_schema(&schema)
        .expect("group schema compiles")
        .membership_type()
        .map(str::to_owned)
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
fn the_binding_comes_from_effective_trait_values() {
    assert_eq!(
        binding(json!({"membership_type": MEMBERSHIP})).unwrap(),
        MEMBERSHIP
    );
}

#[test]
fn only_a_concrete_membership_schema_binds() {
    for id in [
        GROUP_MEMBERSHIP_TYPE_ID,
        gts_id!("cf.test.rg.member.v1~"),
        "gts.cf.core.rg.group_membership.v1~not an id",
    ] {
        assert!(concrete_membership_type(id).is_none(), "{id}");
        let err = binding(json!({"membership_type": id})).unwrap_err();
        assert!(
            matches!(err, DomainError::Validation { .. }),
            "{id}: {err:?}"
        );
    }
    assert!(concrete_membership_type(MEMBERSHIP).is_some());
}

#[test]
fn a_payload_const_cannot_supply_a_missing_trait() {
    let group = snapshot(
        json!({}),
        json!({"properties": {"membership_type": {"const": MEMBERSHIP}}}),
    );
    assert!(
        RgType::from_schema(&group)
            .unwrap()
            .membership_type()
            .is_err()
    );
    for traits in [Value::Null, json!({}), json!({"membership_type": 42})] {
        assert!(binding(traits).is_err());
    }
}

#[test]
fn placement_attribution_preserves_local_refs_and_required_metadata() {
    let schema = snapshot(
        json!({}),
        json!({
            "$schema": "http://json-schema.org/draft-07/schema#",
            "definitions": { "parent": { "const": GROUP } },
            "allOf": [{ "properties": { "parent_type": { "$ref": "#/definitions/parent" } } }],
            "required": ["metadata"]
        }),
    );
    let group = RgType::from_schema(&schema).unwrap();
    assert!(matches!(
        group.validate_group(None, Some(&json!({}))),
        Err(DomainError::InvalidParentType { .. })
    ));
    assert!(matches!(
        group.validate_group(Some(GROUP), None),
        Err(DomainError::Validation { .. })
    ));
    group.validate_group(Some(GROUP), Some(&json!({}))).unwrap();
}

#[test]
fn conditional_metadata_rule_is_a_payload_error() {
    let schema = snapshot(
        json!({}),
        json!({
            "properties": { "parent_type": { "enum": [null, GROUP] } },
            "oneOf": [
                { "properties": { "parent_type": { "const": GROUP }, "metadata": { "const": "team" } } },
                { "properties": { "parent_type": { "type": "null" }, "metadata": { "const": "root" } } }
            ]
        }),
    );
    let group = RgType::from_schema(&schema).unwrap();
    assert!(group.placement.is_some());
    assert!(matches!(
        group.validate_group(Some(GROUP), Some(&json!("bad"))),
        Err(DomainError::Validation { .. })
    ));
}
