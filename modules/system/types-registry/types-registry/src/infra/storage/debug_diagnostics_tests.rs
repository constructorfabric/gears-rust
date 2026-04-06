use super::*;
use serde_json::json;

#[test]
fn test_extract_schema_id() {
    assert_eq!(
        extract_schema_id("gts.vendor.pkg.ns.type.v1~vendor.app.instance.v1"),
        Some("gts.vendor.pkg.ns.type.v1~".to_owned())
    );
    // Schema IDs (ending with ~) don't have an "instance" portion
    assert_eq!(extract_schema_id("gts.vendor.pkg.ns.type.v1~"), None);
    assert_eq!(extract_schema_id("no-tilde"), None);
}

#[test]
fn test_normalize_gts_ref() {
    assert_eq!(
        normalize_gts_ref("gts://gts.vendor.pkg.ns.type.v1~"),
        Some("gts.vendor.pkg.ns.type.v1~".to_owned())
    );
    assert_eq!(
        normalize_gts_ref("gts.vendor.pkg.ns.type.v1~"),
        Some("gts.vendor.pkg.ns.type.v1~".to_owned())
    );
    assert_eq!(normalize_gts_ref("#/definitions/Something"), None);
    assert_eq!(normalize_gts_ref("http://example.com/schema"), None);
}

#[test]
fn test_collect_schema_refs() {
    let schema = json!({
        "$ref": "gts://gts.vendor.pkg.ns.base.v1~",
        "allOf": [
            { "$ref": "gts.vendor.pkg.ns.mixin.v1~" }
        ],
        "x-gts-ref": "gts.vendor.pkg.ns.other.v1~"
    });

    let refs = collect_schema_refs(&schema);
    assert_eq!(refs.len(), 3);
    assert!(refs.contains(&"gts.vendor.pkg.ns.base.v1~".to_owned()));
    assert!(refs.contains(&"gts.vendor.pkg.ns.mixin.v1~".to_owned()));
    assert!(refs.contains(&"gts.vendor.pkg.ns.other.v1~".to_owned()));
}

#[test]
fn test_collect_schema_refs_empty() {
    let schema = json!({
        "type": "object",
        "properties": {
            "name": { "type": "string" }
        }
    });

    let refs = collect_schema_refs(&schema);
    assert!(refs.is_empty());
}

#[test]
fn test_cycle_detection_in_visited_set() {
    let mut visited: HashSet<String> = HashSet::new();
    let schema_id = "gts.vendor.pkg.ns.type.v1~";

    // First visit should succeed
    assert!(!visited.contains(schema_id));
    visited.insert(schema_id.to_owned());

    // Second visit should be detected as cycle
    assert!(visited.contains(schema_id));
}

#[test]
fn test_log_registration_failure_with_gts_id() {
    // This test verifies the function doesn't panic
    let entity = json!({
        "$id": "gts://gts.acme.core.events.test.v1~",
        "type": "object"
    });
    log_registration_failure(Some("gts.acme.core.events.test.v1~"), &entity, "Test error");
}

#[test]
fn test_log_registration_failure_without_gts_id() {
    // This test verifies the function doesn't panic
    let entity = json!({
        "type": "object"
    });
    log_registration_failure(None, &entity, "No GTS ID found");
}

#[test]
fn test_log_schema_validation_failure() {
    // This test verifies the function doesn't panic
    let schema = json!({
        "$id": "gts://gts.acme.core.events.test.v1~",
        "$schema": "http://json-schema.org/draft-07/schema#",
        "type": "invalid_type"
    });
    log_schema_validation_failure("gts.acme.core.events.test.v1~", &schema, "Invalid type");
}

#[test]
fn test_extract_schema_id_with_chained_instance() {
    // Instance with multiple segments
    assert_eq!(
        extract_schema_id("gts.a.b.c.d.v1~vendor.app.x.y.v1"),
        Some("gts.a.b.c.d.v1~".to_owned())
    );
}

#[test]
fn test_collect_schema_refs_nested_allof() {
    let schema = json!({
        "allOf": [
            { "$ref": "gts.vendor.pkg.ns.base1.v1~" },
            { "$ref": "gts.vendor.pkg.ns.base2.v1~" },
            { "type": "object" }
        ]
    });

    let refs = collect_schema_refs(&schema);
    assert_eq!(refs.len(), 2);
    assert!(refs.contains(&"gts.vendor.pkg.ns.base1.v1~".to_owned()));
    assert!(refs.contains(&"gts.vendor.pkg.ns.base2.v1~".to_owned()));
}
