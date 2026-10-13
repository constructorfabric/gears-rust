// Created: 2026-10-12 by Constructor Tech
use super::*;

use serde_json::json;
use uuid::Uuid;

fn def_with_schema(schema: Option<serde_json::Value>) -> RgTypeDef {
    RgTypeDef {
        uuid: Uuid::now_v7(),
        code: format!("{RG_TYPE_PREFIX}x.test.t.v1~"),
        can_be_root: true,
        allowed_parent_types: vec![],
        allowed_membership_types: vec![],
        schema,
    }
}

/// Shape of a TR-resolved RG type: `allOf` with the base inlined, no
/// top-level `properties.metadata`.
fn resolved_schema() -> serde_json::Value {
    json!({
        "allOf": [
            {
                "type": "object",
                "additionalProperties": false,
                "properties": { "id": {"type": "string"}, "metadata": {"type": "object"} }
            },
            {
                "properties": {
                    "metadata": {
                        "type": "object",
                        "additionalProperties": false,
                        "properties": { "status": {"enum": ["active", "suspended"]} }
                    }
                }
            }
        ]
    })
}

#[test]
fn no_metadata_or_no_schema_passes() {
    let def = def_with_schema(Some(resolved_schema()));
    assert!(validate_metadata_against_def(None, &def).is_ok());
    let schemaless = def_with_schema(None);
    assert!(validate_metadata_against_def(Some(&json!({"anything": 1})), &schemaless).is_ok());
}

#[test]
fn metadata_is_validated_inside_the_wrapper_document() {
    let def = def_with_schema(Some(resolved_schema()));
    assert!(validate_metadata_against_def(Some(&json!({"status": "active"})), &def).is_ok());

    let err = validate_metadata_against_def(Some(&json!({"unknown": true})), &def).unwrap_err();
    match err {
        DomainError::Validation { message } => {
            assert!(
                message.starts_with("Metadata does not match type schema: "),
                "{message}"
            );
        }
        other => panic!("expected Validation, got {other:?}"),
    }

    assert!(validate_metadata_against_def(Some(&json!({"status": "gone"})), &def).is_err());
}

#[test]
fn schema_that_fails_to_compile_is_a_validation_error() {
    let def = def_with_schema(Some(json!({"type": 12})));
    match validate_metadata_against_def(Some(&json!({})), &def).unwrap_err() {
        DomainError::Validation { message } => {
            assert!(
                message.starts_with("Type metadata_schema is invalid: "),
                "{message}"
            );
        }
        other => panic!("expected Validation, got {other:?}"),
    }
}
