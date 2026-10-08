//! S1-06 independent frozen bytes; no production encoder or provider claim.
#[path = "conformance_support/encoding.rs"]
mod encoding;
use serde_json::{Value, json};

#[allow(clippy::expect_used)] // A corrupt frozen test fixture must stop the test immediately.
fn vectors() -> Vec<Value> {
    serde_json::from_str::<Value>(include_str!("fixtures/conformance-v1.json"))
        .expect("frozen vector fixture")["vectors"]
        .as_array()
        .expect("frozen vector fixture")
        .clone()
}
#[allow(clippy::expect_used)] // Every referenced fixture ID is part of this checked-in corpus.
fn named(id: &str) -> Value {
    vectors()
        .into_iter()
        .find(|v| v["id"] == id)
        .expect("frozen vector fixture")
}

#[test]
fn independent_preimages_match_reference_encoder() {
    for vector in vectors() {
        let bytes = encoding::encode(vector["kind"].as_str().unwrap(), &vector["input"]).unwrap();
        assert_eq!(
            encoding::hex(&bytes),
            vector["preimage_hex"],
            "{}",
            vector["id"]
        );
        // SHA-256 is independently checked by the Python oracle; no new test crypto dependency.
        assert_eq!(
            encoding::unhex(vector["sha256"].as_str().unwrap())
                .unwrap()
                .len(),
            32
        );
    }
}

#[test]
fn every_audit_field_changes_bytes_or_is_rejected() {
    let base = named("v2-after-v1-caller-reason")["input"].clone();
    let original = encoding::audit(&base).unwrap();
    for (key, value) in base.as_object().unwrap() {
        let mut changed = base.clone();
        changed[key] = if value.is_null() {
            json!("changed")
        } else {
            Value::Null
        };
        if let Ok(bytes) = encoding::audit(&changed) {
            assert_ne!(bytes, original, "uncovered field {key}");
        }
    }
    for (key, value) in [
        ("hash_version", json!(4)),
        ("sequence", json!("0")),
        ("sequence", json!("9223372036854775808")),
        ("version", json!("2147483648")),
        ("created_at", json!("1.25")),
        ("prev_hash", json!("abcd")),
    ] {
        let mut bad = base.clone();
        bad[key] = value;
        assert!(encoding::audit(&bad).is_err(), "accepted {key}");
    }
    let mut v1 = base;
    v1["hash_version"] = json!(1);
    assert!(
        encoding::audit(&v1).is_err(),
        "v1 cannot silently omit caller evidence"
    );
}

#[test]
fn null_empty_boundaries_and_cross_version_links_are_distinct() {
    for (a, b) in [
        ("null-empty", "empty-null"),
        ("adjacent-ab-c", "adjacent-a-bc"),
        ("v2-caller-null", "v2-caller-empty"),
    ] {
        assert_ne!(named(a)["preimage_hex"], named(b)["preimage_hex"]);
        assert_ne!(named(a)["sha256"], named(b)["sha256"]);
    }
    assert_eq!(
        named("v2-after-v1-caller-reason")["input"]["prev_hash"],
        named("v1-create")["sha256"]
    );
    for version in [1, 2] {
        assert_eq!(
            named(&format!("v{version}-later-resource-change"))["input"]["prev_hash"],
            named(&format!("v{version}-create"))["sha256"]
        );
        assert_eq!(
            named(&format!("v{version}-create"))["input"]["prev_hash"],
            named("order-genesis")["sha256"]
        );
    }
}

#[test]
fn checkpoint_duplicate_unsorted_missing_and_unknown_inputs_refuse() {
    let base = named("checkpoint-2-members")["input"].clone();
    for mode in 0..5 {
        let mut bad = base.clone();
        match mode {
            0 => bad["members"][1] = bad["members"][0].clone(),
            1 => bad["members"].as_array_mut().unwrap().reverse(),
            2 => bad["member_count"] = json!("3"),
            3 => bad["format_version"] = json!(2),
            _ => {
                bad["members"][0]
                    .as_object_mut()
                    .unwrap()
                    .remove("entry_hash");
            }
        }
        assert!(encoding::checkpoint(&bad).is_err(), "case {mode}");
    }
}

#[test]
fn request_fingerprint_fixture_binds_each_semantic_input() {
    let base = named("request-submit")["input"].clone();
    let bytes = encoding::encode("request", &base).unwrap();
    for key in base.as_object().unwrap().keys() {
        let mut changed = base.clone();
        changed[key] = Value::Null;
        assert_ne!(
            encoding::encode("request", &changed).unwrap(),
            bytes,
            "{key}"
        );
    }
    // Reordering object keys is irrelevant; array order and exact scalar strings remain semantic.
    let mut reordered = serde_json::Map::new();
    for (key, value) in base.as_object().unwrap().iter().rev() {
        reordered.insert(key.clone(), value.clone());
    }
    assert_eq!(
        encoding::encode("request", &Value::Object(reordered)).unwrap(),
        bytes
    );
    for excluded in [
        "correlation_id",
        "requested_at",
        "headers",
        "server_snapshot",
        "principal_scope",
        "idempotency_key",
    ] {
        assert!(
            base.get(excluded).is_none(),
            "not fingerprint input: {excluded}"
        );
    }
    let create = named("request-create");
    assert!(create["input"]["order_id"].is_null());
    assert!(create["input"]["expected_version"].is_null());
}

#[test]
fn cursor_fixture_binds_every_collection_context_and_microsecond_position() {
    for endpoint in ["orders", "versions", "lines", "acceptances", "audit"] {
        let base = named(&format!("cursor-{endpoint}"))["input"].clone();
        let bytes = encoding::encode("cursor", &base).unwrap();
        for key in [
            "endpoint",
            "parent_order_id",
            "subject_tenant_id",
            "subject_id",
            "filters",
            "sort",
            "position",
            "format_version",
        ] {
            let mut changed = base.clone();
            changed[key] = json!("different");
            assert_ne!(
                encoding::encode("cursor", &changed).unwrap(),
                bytes,
                "{endpoint}/{key}"
            );
        }
    }
    let mut audit = named("cursor-audit")["input"].clone();
    let before = encoding::encode("cursor", &audit).unwrap();
    audit["position"]["created_at_micros"] = json!("1791288000123457");
    assert_ne!(encoding::encode("cursor", &audit).unwrap(), before);
}

#[test]
fn force_request_observation_is_hashed_closed_and_never_a_chain_sequence() {
    let row = named("v3-force-request")["input"].clone();
    let original = encoding::audit(&row).unwrap();
    assert!(row["sequence"].is_null() && row["prev_hash"].is_null());
    for field in ["audit_sequence", "state", "version"] {
        assert_ne!(
            named(&format!("v3-force-changed-{field}"))["preimage_hex"],
            named("v3-force-request")["preimage_hex"]
        );
        let mut bad = row.clone();
        bad["force_request_observation"]
            .as_object_mut()
            .unwrap()
            .remove(field);
        assert!(encoding::audit(&bad).is_err());
    }
    for value in [
        json!(null),
        json!({}),
        json!({"audit_sequence":"55","state":"in_fulfillment","version":"7","extra":true}),
        json!({"audit_sequence":"0","state":"in_fulfillment","version":"7"}),
        json!({"audit_sequence":"9223372036854775808","state":"in_fulfillment","version":"7"}),
        json!({"audit_sequence":"55","state":"invented","version":"7"}),
        json!({"audit_sequence":"55","state":"in_fulfillment","version":"4"}),
    ] {
        let mut bad = row.clone();
        bad["force_request_observation"] = value;
        assert!(encoding::audit(&bad).is_err());
    }
    let mut changed = row.clone();
    changed["force_request_observation"]["audit_sequence"] = json!("56");
    assert_ne!(encoding::audit(&changed).unwrap(), original);
    for version in [1, 2] {
        let mut bad = row.clone();
        bad["hash_version"] = json!(version);
        assert!(
            encoding::audit(&bad).is_err(),
            "old encoding cannot omit new evidence"
        );
    }
    assert_eq!(
        named("v3-after-v2")["input"]["prev_hash"],
        named("v2-after-v1-caller-reason")["sha256"]
    );
}
