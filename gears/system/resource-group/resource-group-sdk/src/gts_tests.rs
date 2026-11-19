//! The RG base type is a crate-local declaration for group and membership schemas.

#![allow(clippy::expect_used, reason = "test assertions")]

use gts::GtsSchema;
use serde_json::json;
use toolkit_gts::gts_id;

use super::{MEMBERSHIP_TYPE_V1, ResourceGroupTypeV1};

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
    assert!(schema.get("x-gts-traits-schema").is_none());
    assert!(schema.get("x-gts-traits").is_none());
    assert_eq!(schema["properties"]["metadata"]["type"], "object");
    let required = schema
        .get("required")
        .and_then(|r| r.as_array())
        .cloned()
        .unwrap_or_default();
    assert_eq!(required, [json!("parent_type"), json!("membership_type")]);
}
