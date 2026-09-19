use super::*;
use serde_json::json;

#[test]
fn schema_all_of_merges_declared_object_properties() -> Result<(), EngineConfigError> {
    let shape = schema(
        &json!({
            "allOf": [
                { "type": "object", "properties": { "region": { "type": "string" } } },
                { "type": "object", "properties": { "weight": { "type": "integer" } } }
            ]
        }),
        0,
    )?;

    let Kind::Object(fields) = shape.kind else {
        panic!("allOf object must produce a typed object")
    };
    assert_eq!(fields.get("region"), Some(&Shape::new(Kind::String)));
    assert_eq!(fields.get("weight"), Some(&Shape::new(Kind::Int)));
    Ok(())
}

#[test]
fn schema_union_keeps_only_properties_common_to_every_non_null_alternative()
-> Result<(), EngineConfigError> {
    let shape = schema(
        &json!({
            "oneOf": [
                { "type": "null" },
                { "type": "object", "properties": {
                    "shared": { "type": "integer", "const": 1 },
                    "left_only": { "type": "boolean" }
                }},
                { "type": "object", "properties": {
                    "shared": { "type": "integer", "const": 2 },
                    "right_only": { "type": "string" }
                }}
            ]
        }),
        0,
    )?;

    let Kind::Object(fields) = shape.kind else {
        panic!("compatible object alternatives must produce an object")
    };
    assert_eq!(fields.len(), 1);
    assert_eq!(
        fields.get("shared").and_then(|field| field.domain.as_ref()),
        Some(&vec![json!(1), json!(2)])
    );
    Ok(())
}

#[test]
fn schema_union_rejects_ambiguous_scalar_alternatives() {
    let error = schema(
        &json!({
            "anyOf": [
                { "type": "integer" },
                { "type": "string" }
            ]
        }),
        0,
    )
    .expect_err("integer and string alternatives are ambiguous");

    assert_eq!(error.message, "ambiguous types across schema alternatives");
    assert_eq!((error.line, error.column), (None, None));
}

#[test]
fn schema_rejects_untyped_arrays_and_excessive_nesting() {
    let missing_items = schema(&json!({ "type": "array" }), 0)
        .expect_err("array items must carry their own schema");
    assert_eq!(missing_items.message, "array schema requires typed items");

    let too_deep =
        schema(&json!({ "type": "integer" }), MAX_DEPTH + 1).expect_err("schema depth is bounded");
    assert_eq!(too_deep.message, "schema nesting exceeds the CEL bound");
}

#[test]
fn compatible_recurses_through_lists_and_shared_object_fields() {
    let int_list = Shape::list(Shape::new(Kind::Int));
    let optional_int_list = Shape::list(Shape::new(Kind::OptionalInt));
    assert!(compatible(&int_list, &optional_int_list));

    let left = Shape::new(Kind::Object(BTreeMap::from([
        ("shared".to_owned(), Shape::new(Kind::String)),
        ("left_only".to_owned(), Shape::new(Kind::Bool)),
    ])));
    let right = Shape::new(Kind::Object(BTreeMap::from([(
        "shared".to_owned(),
        Shape::new(Kind::Int),
    )])));
    assert!(!compatible(&left, &right));
    assert!(compatible(&Shape::new(Kind::Unknown), &right));
}
