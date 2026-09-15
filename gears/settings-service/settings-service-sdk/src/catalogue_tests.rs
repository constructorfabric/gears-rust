// Created: 2026-09-06 by Constructor Tech
//! Tests for the starter value-type catalogue.

use std::collections::HashSet;

use serde_json::json;

use super::{CATALOGUE, CRON, REGEX, SECRET_STRING, TEXT, VALUE_TYPE_PREFIX};

#[test]
fn every_entry_is_a_root_type_under_the_toolkit_prefix() {
    let mut seen = HashSet::new();
    for entry in CATALOGUE {
        assert!(entry.id.starts_with(VALUE_TYPE_PREFIX), "{}", entry.id);
        assert!(entry.id.ends_with(".v1~"), "{}", entry.id);
        assert_eq!(
            entry.id.matches('~').count(),
            1,
            "a root type, not a chain: {}",
            entry.id
        );
        assert!(seen.insert(entry.id), "duplicate id {}", entry.id);
    }
    assert_eq!(CATALOGUE.len(), 15);
}

#[test]
fn every_schema_is_a_valid_json_schema_identified_by_its_type_id() {
    for entry in CATALOGUE {
        let schema = (entry.schema)();
        assert_eq!(
            schema["$id"],
            json!(format!("gts://{}", entry.id)),
            "{}",
            entry.id
        );
        assert!(
            schema.get("default").is_none(),
            "a Schema Default lives on the declaration, never in the type: {}",
            entry.id
        );
        jsonschema::validator_for(&schema)
            .unwrap_or_else(|e| panic!("{} is not a valid JSON Schema: {e}", entry.id));
    }
}

#[test]
fn traits_are_declared_where_the_validator_and_reader_need_them() {
    let by_id = |id: &str| {
        CATALOGUE
            .iter()
            .find(|e| e.id == id)
            .map(|e| (e.schema)())
            .expect("entry exists")
    };
    assert_eq!(by_id(SECRET_STRING)["x-gts-traits"]["secret"], json!(true));
    assert_eq!(by_id(TEXT)["x-gts-traits"]["multiline"], json!(true));
    assert_eq!(by_id(REGEX)["x-gts-traits"]["regex"], json!(true));
    assert_eq!(
        by_id(CRON)["x-gts-traits"]["cron_dialect"],
        json!("standard")
    );
    // Traits travel with the schema that lets the registry check them.
    for id in [SECRET_STRING, TEXT, REGEX, CRON] {
        let schema = by_id(id);
        assert!(schema.get("x-gts-traits-schema").is_some(), "{id}");
        let traits =
            jsonschema::validator_for(&schema["x-gts-traits-schema"]).expect("traits schema");
        assert!(
            traits.is_valid(&schema["x-gts-traits"]),
            "{id}: traits fit their schema"
        );
    }
}

#[test]
fn the_catalogue_is_submitted_to_the_inventory() {
    // What the types registry drains at its init: absent from the inventory,
    // no declaration could name the type and validate against it.
    let ids: HashSet<String> = toolkit_gts::all_inventory_type_schemas()
        .expect("inventory renders")
        .into_iter()
        .filter_map(|s| s["$id"].as_str().map(ToOwned::to_owned))
        .collect();
    for entry in CATALOGUE {
        assert!(
            ids.contains(&format!("gts://{}", entry.id)),
            "{} is in the inventory",
            entry.id
        );
    }
}

#[test]
fn the_scalar_shapes_validate_what_they_claim() {
    let check = |id: &str, value: serde_json::Value| -> bool {
        let schema = CATALOGUE
            .iter()
            .find(|e| e.id == id)
            .map(|e| (e.schema)())
            .expect("entry exists");
        jsonschema::options()
            .should_validate_formats(true)
            .build(&schema)
            .expect("valid schema")
            .is_valid(&value)
    };
    assert!(check(super::BOOL_FLAG, json!(true)));
    assert!(!check(super::BOOL_FLAG, json!("true")));
    assert!(check(super::PORT, json!(8080)));
    assert!(!check(super::PORT, json!(0)));
    assert!(check(super::IPV4, json!("10.0.0.1")));
    assert!(!check(super::IPV4, json!("999.1.1.1")));
    assert!(check(super::URL, json!("https://example.com/x")));
    assert!(!check(super::URL, json!("not a url")));
    assert!(check(super::DURATION_SECONDS, json!(0)));
    assert!(!check(super::DURATION_SECONDS, json!(-1)));
    assert!(check(super::JSON, json!({ "any": [1, 2] })));
    assert!(!check(super::JSON, json!("a string is not an object")));
}
