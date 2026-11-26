//! The SDK snapshot-to-GTS bridge; validator rules themselves belong to `gts`.

use std::error::Error as _;

use serde_json::{Value, json};
use toolkit_gts::gts_id;

use crate::{
    CompilePayloadValidatorError, EntityField, GtsCompilePayloadValidatorError, GtsId, GtsTypeId,
    LifecycleStatus, PayloadValidationError, TypeSchema,
};

const TYPE_ID: &str = gts_id!("cf.test.payload.base.v1~cf.test.payload.leaf.v1~");

fn snapshot(content: Value, resolved_schema: Value) -> TypeSchema {
    TypeSchema {
        type_id: GtsTypeId::try_new(TYPE_ID).expect("type identifier"),
        type_uuid: GtsId::try_new(TYPE_ID).expect("identifier").to_uuid(),
        lifecycle_status: LifecycleStatus::Active,
        ownership: crate::models::Ownership::Global,
        origin: None,
        content: Some(content),
        resolved_schema: Some(resolved_schema),
        effective_traits: None,
        effective_traits_schema: None,
        provenance: None,
    }
}

#[test]
fn minimum_projection_compiles_a_reusable_validator_over_the_resolved_schema() {
    let schema = snapshot(
        json!({"type": "object"}),
        json!({
            "$schema": "http://json-schema.org/draft-07/schema#",
            "type": "object",
            "properties": {"name": {"type": "string", "default": "unnamed"}},
            "required": ["name"]
        }),
    );
    let before = schema.clone();
    let validator = schema
        .compile_payload_validator()
        .expect("minimal projection");
    assert_eq!(validator.type_id(), &schema.type_id);
    for name in ["first", "second"] {
        validator
            .validate(&json!({"name": name}))
            .expect("valid payload");
    }
    let missing = json!({});
    assert!(matches!(
        validator.validate(&missing),
        Err(PayloadValidationError::Constraints(_))
    ));
    assert_eq!(missing, json!({}), "defaults must not mutate input");
    assert_eq!(schema, before, "compilation must not mutate the snapshot");
}

#[test]
fn each_required_projection_field_is_reported_by_name() {
    for field in [EntityField::Content, EntityField::ResolvedSchema] {
        let mut schema = snapshot(json!({}), json!({}));
        match field {
            EntityField::Content => schema.content = None,
            EntityField::ResolvedSchema => schema.resolved_schema = None,
            _ => unreachable!("only required fields"),
        }
        let error = schema
            .compile_payload_validator()
            .expect_err("missing projection");
        assert!(matches!(
            error,
            CompilePayloadValidatorError::MissingField { type_id, field: missing }
                if type_id == schema.type_id && missing == field
        ));
    }
}

#[test]
fn selected_non_object_content_is_invalid_rather_than_unselected() {
    for content in [
        Value::Null,
        json!(false),
        json!([]),
        json!(42),
        json!("text"),
    ] {
        let schema = snapshot(content, json!({}));
        assert!(matches!(
            schema.compile_payload_validator(),
            Err(CompilePayloadValidatorError::InvalidContent { type_id })
                if type_id == schema.type_id
        ));
    }
}

#[test]
fn an_abstract_authored_leaf_preserves_the_native_error_and_its_source() {
    let schema = snapshot(json!({"x-gts-abstract": true}), json!({}));
    let error = schema
        .compile_payload_validator()
        .expect_err("abstract leaf");
    assert!(matches!(
        &error,
        CompilePayloadValidatorError::Upstream(GtsCompilePayloadValidatorError::Abstract(id))
            if id == &schema.type_id
    ));
    assert!(matches!(
        error.source().and_then(|source| source.downcast_ref::<GtsCompilePayloadValidatorError>()),
        Some(GtsCompilePayloadValidatorError::Abstract(id)) if id == &schema.type_id
    ));
}

#[test]
fn inherited_abstractness_does_not_make_a_concrete_leaf_abstract() {
    for content in [
        json!({}),
        json!({"x-gts-abstract": false}),
        json!({"x-gts-abstract": null}),
    ] {
        let schema = snapshot(content, json!({"x-gts-abstract": true}));
        schema
            .compile_payload_validator()
            .expect("the authored leaf is concrete");
    }
}

#[test]
fn null_and_unavailable_resolved_schemas_preserve_native_compilation_errors() {
    for resolved in [Value::Null, json!({"$ref": "urn:missing:schema"})] {
        let schema = snapshot(json!({}), resolved);
        assert!(matches!(
            schema.compile_payload_validator(),
            Err(CompilePayloadValidatorError::Upstream(
                GtsCompilePayloadValidatorError::InvalidSchema { type_id, .. }
            )) if type_id == schema.type_id
        ));
    }
}

#[test]
fn boolean_resolved_schemas_keep_their_native_meaning() {
    for allowed in [true, false] {
        let schema = snapshot(json!({}), json!(allowed));
        let validator = schema.compile_payload_validator().expect("boolean schema");
        assert_eq!(validator.is_valid(&json!({})), allowed);
    }
}

#[test]
fn self_references_use_the_selected_id_despite_an_inherited_schema_id() {
    let base = gts_id!("cf.test.payload.base.v1~");
    let schema = snapshot(
        json!({}),
        json!({
            "$id": format!("gts://{base}"),
            "$schema": "http://json-schema.org/draft-07/schema#",
            "type": "object",
            "properties": {"kind": {"type": "string", "x-gts-type-ref": "/$id"}},
            "required": ["kind"]
        }),
    );
    let validator = schema
        .compile_payload_validator()
        .expect("selected identifier");
    assert!(validator.is_valid(&json!({"kind": TYPE_ID})));
    assert!(!validator.is_valid(&json!({"kind": base})));
}
