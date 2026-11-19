//! The RG base type is a crate-local declaration carrying the placement traits.

#![allow(clippy::expect_used, reason = "test assertions")]

use gts::GtsSchema;
use serde_json::json;
use toolkit_gts::gts_id;

use super::{MembershipTypeRef, ResourceGroupTypeTraitsV1, ResourceGroupTypeV1, RgTypeRef};

const BASE: &str = gts_id!("cf.core.rg.type.v1~");

#[test]
fn the_traits_builder_serializes_explicit_policy_choices() {
    let traits = ResourceGroupTypeTraitsV1::builder()
        .with_can_be_root(true)
        .with_allowed_parent_types([BASE])
        .with_allowed_membership_types(["gts.cf.core.am.user.v1~".to_owned()])
        .build();

    assert_eq!(
        serde_json::to_value(traits).expect("traits JSON"),
        json!({
            "can_be_root": true,
            "allowed_parent_types": [BASE],
            "allowed_membership_types": ["gts.cf.core.am.user.v1~"],
        })
    );
}

#[test]
fn the_traits_builder_accepts_empty_lists_and_setters_in_any_order() {
    let traits = ResourceGroupTypeTraitsV1::builder()
        .with_allowed_membership_types(Vec::<String>::new())
        .with_allowed_parent_types(Vec::<String>::new())
        .with_can_be_root(false)
        .build();

    assert_eq!(traits, ResourceGroupTypeTraitsV1::default());
    assert_eq!(
        serde_json::to_value(traits).expect("traits JSON"),
        json!({
            "can_be_root": false,
            "allowed_parent_types": [],
            "allowed_membership_types": [],
        })
    );
}

#[test]
fn the_base_type_is_this_crates_declaration() {
    let ids: Vec<String> = crate::gts_declarations()
        .into_iter()
        .map(|(id, _)| id)
        .collect();
    assert_eq!(ids, vec![BASE.to_owned()]);
}

#[test]
fn the_base_type_declares_the_placement_traits_with_defaults() {
    let schema = ResourceGroupTypeV1::<()>::gts_schema_with_refs();
    let traits_schema = &schema["x-gts-traits-schema"];
    let mut names: Vec<&str> = traits_schema["properties"]
        .as_object()
        .expect("traits schema properties")
        .keys()
        .map(String::as_str)
        .collect();
    names.sort_unstable();
    assert_eq!(
        names,
        [
            "allowed_membership_types",
            "allowed_parent_types",
            "can_be_root"
        ]
    );
    assert_eq!(
        schema["x-gts-traits"],
        json!({
            "can_be_root": false,
            "allowed_parent_types": [],
            "allowed_membership_types": [],
        })
    );
}

#[test]
fn parent_types_must_be_rg_types_and_membership_types_any_gts_type() {
    let schema = ResourceGroupTypeV1::<()>::gts_schema_with_refs();
    let props = &schema["x-gts-traits-schema"]["properties"];
    assert_eq!(
        props["allowed_parent_types"]["items"]["x-gts-type-ref"],
        BASE
    );
    assert_eq!(
        props["allowed_membership_types"]["items"]["x-gts-type-ref"],
        "gts.*"
    );
}

#[test]
fn the_group_envelope_carries_an_optional_metadata_object() {
    let schema = ResourceGroupTypeV1::<()>::gts_schema_with_refs();
    assert_eq!(schema["properties"]["metadata"]["type"], "object");
    let required = schema
        .get("required")
        .and_then(|r| r.as_array())
        .cloned()
        .unwrap_or_default();
    assert!(required.is_empty(), "{required:?}");
}

#[test]
fn traits_round_trip_through_effective_traits_json() {
    let traits: ResourceGroupTypeTraitsV1 = serde_json::from_value(json!({
        "can_be_root": true,
        "allowed_parent_types": [BASE],
        "allowed_membership_types": ["gts.cf.core.am.user.v1~"],
    }))
    .expect("effective traits");
    assert!(traits.can_be_root);
    assert_eq!(
        traits.allowed_parent_types,
        vec![RgTypeRef(BASE.to_owned())]
    );
    assert_eq!(
        traits.allowed_membership_types,
        vec![MembershipTypeRef("gts.cf.core.am.user.v1~".to_owned())]
    );
}
