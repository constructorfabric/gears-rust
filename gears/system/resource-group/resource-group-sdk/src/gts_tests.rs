//! The RG base type is a crate-local declaration for group and membership schemas.

#![allow(clippy::expect_used, reason = "test assertions")]

use gts::GtsSchema;
use serde_json::json;
use toolkit_gts::gts_id;

use super::{MEMBERSHIP_TYPE_V1, ResourceGroupMembershipV1, ResourceGroupTypeV1};

const BASE: &str = gts_id!("cf.core.rg.type.v1~");

#[test]
fn the_base_types_are_this_crates_declarations() {
    let ids: Vec<String> = crate::gts_declarations()
        .into_iter()
        .map(|(id, _)| id)
        .collect();
    assert_eq!(ids, [MEMBERSHIP_TYPE_V1, BASE]);
}

#[test]
fn the_group_envelope_carries_an_optional_metadata_object() {
    let schema = ResourceGroupTypeV1::<()>::gts_schema_with_refs();
    let traits = &schema["x-gts-traits-schema"];
    assert_eq!(traits["required"], json!(["membership_type"]));
    assert_eq!(
        traits["properties"]["membership_type"]["x-gts-type-ref"],
        MEMBERSHIP_TYPE_V1
    );
    assert!(schema["properties"].get("membership_type").is_none());
    assert!(schema.get("x-gts-traits").is_none());
    assert_eq!(schema["properties"]["metadata"]["type"], "object");
    assert_eq!(schema["required"], json!(["parent_type"]));
}

#[test]
fn the_membership_base_requires_an_any_type_resource_type() {
    let schema = ResourceGroupMembershipV1::gts_schema_with_refs();
    assert_eq!(schema["x-gts-abstract"], true, "{schema}");
    assert_eq!(
        schema["required"],
        json!(["resource_type"]),
        "only the member type is part of a membership payload"
    );
    let resource_type = &schema["properties"]["resource_type"];
    assert_eq!(resource_type["type"], "string");
    assert_eq!(resource_type["x-gts-type-ref"], "gts.*");
}
