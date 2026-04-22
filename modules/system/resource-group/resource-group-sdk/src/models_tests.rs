// Created: 2026-04-16 by Constructor Tech
use super::*;

// -- GtsTypePath: valid cases (table-driven) -- TC-SDK-01, 06, 07, 08, 19, 20

#[test]
fn gts_type_path_valid_cases() {
    let valid = vec![
        // TC-SDK-01: basic valid path
        ("gts.cf.core.rg.type.v1~", "gts.cf.core.rg.type.v1~"),
        // TC-SDK-06: uppercase is lowered
        ("GTS.CF.CORE.RG.TYPE.V1~", "gts.cf.core.rg.type.v1~"),
        // TC-SDK-07: trimmed + lowered
        ("  GTS.CF.CORE.RG.TYPE.V1~  ", "gts.cf.core.rg.type.v1~"),
        // TC-SDK-08: multi-segment
        (
            "gts.cf.core.rg.type.v1~x.test.unit.root.v1~",
            "gts.cf.core.rg.type.v1~x.test.unit.root.v1~",
        ),
        // TC-SDK-19: numeric version in segment
        ("gts.cf.core.rg.type.v2~", "gts.cf.core.rg.type.v2~"),
        // TC-SDK-20: underscores in segments
        ("gts.cf.core.rg.my_type.v1~", "gts.cf.core.rg.my_type.v1~"),
    ];
    for (input, expected) in valid {
        let path = GtsTypePath::new(input);
        assert!(path.is_ok(), "should be valid: {input}");
        assert_eq!(path.unwrap().as_str(), expected, "for input: {input}");
    }
}

// -- GtsTypePath: invalid cases (table-driven) -- TC-SDK-02..05, 09, 10, 18, 21

#[test]
fn gts_type_path_invalid_cases() {
    let cases = vec![
        // TC-SDK-02: empty
        ("", "must not be empty"),
        // TC-SDK-04: wrong prefix
        ("invalid.path~", "Invalid GTS type path format"),
        // TC-SDK-05: no trailing tilde
        ("gts.cf.core.rg.type.v1", "Invalid GTS type path format"),
        // TC-SDK-09: double tilde
        ("gts.cf.core.rg.type.v1~~", "Invalid GTS type path format"),
        // TC-SDK-10: hyphen in segment
        (
            "gts.cf.core.rg.type.v1~hello-world~",
            "Invalid GTS type path format",
        ),
        // TC-SDK-18: empty segment after gts.
        ("gts.~", "Invalid GTS type path format"),
        // TC-SDK-21: whitespace-only
        ("   ", "must not be empty"),
    ];
    for (input, expected_msg) in cases {
        let result = GtsTypePath::new(input);
        assert!(result.is_err(), "should be invalid: '{input}'");
        let err = result.unwrap_err();
        assert!(
            err.contains(expected_msg),
            "for input '{input}': expected '{expected_msg}' in error, got: {err}"
        );
    }
}

// -- GtsTypePath: length boundary tests -- TC-SDK-03, 22, 23

#[test]
#[allow(unknown_lints, de0901_gts_string_pattern)]
fn gts_type_path_max_length_boundary() {
    // TC-SDK-22: exactly 1024 chars -> Ok
    // Base: "gts.cf.core.rg.type.v1~" = 24 chars.
    // Pad the type name to fill remaining: 1024 - 24 = 1000 chars in first segment.
    // "gts.cf.core.rg." (15) + padded_name + ".v1~" (4) = 1024
    // padded_name = 1024 - 15 - 4 = 1005 chars
    let name = "a".repeat(1005);
    let path_1024 = format!("gts.cf.core.rg.{name}.v1~");
    assert_eq!(path_1024.len(), 1024);
    assert!(
        GtsTypePath::new(&path_1024).is_ok(),
        "exactly 1024 chars should be valid: {:?}",
        GtsTypePath::new(&path_1024)
    );

    // TC-SDK-23: > 1024 chars -> Err (exceeds max length)
    let name_long = "a".repeat(1006);
    let path_1025 = format!("gts.cf.core.rg.{name_long}.v1~");
    assert_eq!(path_1025.len(), 1025);
    let result = GtsTypePath::new(&path_1025);
    assert!(result.is_err(), "1025 chars should exceed max length");
    assert!(result.unwrap_err().contains("exceeds maximum length"));
}

// -- GtsTypePath: serde round-trip -- TC-SDK-11, 12

#[test]
fn gts_type_path_serde_round_trip() {
    // TC-SDK-11: serialize then deserialize
    let original = GtsTypePath::new("gts.cf.core.rg.type.v1~").unwrap();
    let json = serde_json::to_string(&original).unwrap();
    let deserialized: GtsTypePath = serde_json::from_str(&json).unwrap();
    assert_eq!(original, deserialized);
}

#[test]
fn gts_type_path_serde_invalid_rejects() {
    // TC-SDK-12: invalid value during deserialization
    let result = serde_json::from_str::<GtsTypePath>("\"invalid\"");
    assert!(result.is_err(), "invalid path should fail deserialization");
}

// -- GtsTypePath: Display / Into<String> -- TC-SDK-13

#[test]
fn gts_type_path_display_and_into_string() {
    let path = GtsTypePath::new("gts.cf.core.rg.type.v1~").unwrap();
    let display = path.to_string();
    let into_string: String = path.into();
    assert_eq!(display, into_string);
}

// -- ResourceGroupType serialization -- TC-SDK-14

#[test]
fn resource_group_type_camel_case_keys() {
    let rgt = ResourceGroupType {
        code: "gts.cf.core.rg.type.v1~".to_owned(),
        can_be_root: true,
        allowed_parent_types: vec!["gts.parent~".to_owned()],
        allowed_membership_types: vec!["gts.member~".to_owned()],
        metadata_schema: None,
    };
    let json = serde_json::to_value(&rgt).unwrap();
    assert!(
        json.get("canBeRoot").is_some(),
        "expected camelCase 'canBeRoot'"
    );
    assert!(
        json.get("allowedParentTypes").is_some(),
        "expected camelCase 'allowedParentTypes'"
    );
    assert!(
        json.get("allowedMembershipTypes").is_some(),
        "expected camelCase 'allowedMembershipTypes'"
    );
    assert!(
        json.get("metadataSchema").is_none(),
        "metadataSchema should be absent when None"
    );
}

// -- ResourceGroup serialization -- TC-SDK-15, 16

#[test]
fn resource_group_type_field_renamed() {
    // TC-SDK-15: Rust field is `code`, serialized as JSON key `"type"`.
    let group = ResourceGroup {
        id: Uuid::nil(),
        code: "gts.cf.core.rg.type.v1~".to_owned(),
        name: "Test".to_owned(),
        hierarchy: GroupHierarchy {
            parent_id: None,
            tenant_id: Uuid::nil(),
        },
        metadata: Some(serde_json::json!({"key": "val"})),
    };
    let json = serde_json::to_value(&group).unwrap();
    assert!(
        json.get("type").is_some(),
        "expected 'type' key on the wire"
    );
    assert!(
        json.get("code").is_none(),
        "'code' (Rust field name) must not leak to JSON -- serde renames it to 'type'"
    );
}

#[test]
fn resource_group_metadata_absent_when_none() {
    // TC-SDK-16: metadata: None -> no "metadata" key
    let group = ResourceGroup {
        id: Uuid::nil(),
        code: "gts.cf.core.rg.type.v1~".to_owned(),
        name: "Test".to_owned(),
        hierarchy: GroupHierarchy {
            parent_id: None,
            tenant_id: Uuid::nil(),
        },
        metadata: None,
    };
    let json = serde_json::to_value(&group).unwrap();
    assert!(
        json.get("metadata").is_none(),
        "metadata should be absent when None, got: {json}"
    );
}

// -- GtsTypePath normalization -- TC-SDK-24

#[test]
fn gts_type_path_trims_and_lowercases() {
    // TC-SDK-24: GtsTypePath::new trims whitespace and lowercases.
    // validate_type_code (in the domain crate) also trims and lowercases,
    // ensuring consistent normalization across SDK and domain layers.
    let input = "  GTS.CF.CORE.RG.TYPE.V1~  ";
    let path = GtsTypePath::new(input);
    assert!(
        path.is_ok(),
        "GtsTypePath::new should accept trimmed/lowered input"
    );
    assert_eq!(path.unwrap().as_str(), "gts.cf.core.rg.type.v1~");
}
