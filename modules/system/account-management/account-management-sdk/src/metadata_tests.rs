//! SDK-level unit tests for the tenant-metadata wire shapes.
//!
//! After the SDK slim-down (`MetadataSchemaId` newtype, `derive_schema_uuid`
//! helper, and the `MetadataValidationError` enum moved into the AM
//! impl crate), only the JSON-serialisable shapes are SDK
//! responsibility. The tests below pin:
//!
//! * `MetadataEntry` serde round-trip on the canonical RFC 3339 +
//!   chained-id shape.
//! * `UpsertMetadataRequest` serde round-trip on the two-field
//!   JSON-object shape.
//! * `MetadataEntryFilterField` declared column set (sanity-pin
//!   against accidental wire-contract drift).

#![allow(clippy::expect_used, clippy::unwrap_used, reason = "test helpers")]

use super::*;
use serde_json::json;
use time::macros::datetime;

/// Canonical valid chained schema id used across positive-path tests.
const VALID_SCHEMA_ID: &str = "gts.cf.core.am.tenant_metadata.v1~vendor.app.metadata.branding.v1~";

fn valid_schema_id() -> GtsSchemaId {
    GtsSchemaId::new(VALID_SCHEMA_ID)
}

#[test]
fn metadata_entry_serde_roundtrip() {
    let when = datetime!(2025-01-15 12:34:56 UTC);
    let entry = MetadataEntry::new(
        valid_schema_id(),
        json!({"theme": "dark", "primary": "#3366ff"}),
        when,
        1,
    );

    let wire = serde_json::to_string(&entry).expect("serialize");
    assert!(wire.contains("\"schema_id\":\""), "schema_id key on wire");
    assert!(
        wire.contains("\"updated_at\":\""),
        "rfc3339 updated_at on wire"
    );

    let parsed: MetadataEntry = serde_json::from_str(&wire).expect("roundtrip");
    assert_eq!(parsed, entry);
}

#[test]
fn upsert_metadata_request_serde_roundtrip() {
    let req = UpsertMetadataRequest::new(valid_schema_id(), json!({"flag": true, "limit": 42}));

    let wire = serde_json::to_string(&req).expect("serialize");
    let parsed: UpsertMetadataRequest = serde_json::from_str(&wire).expect("roundtrip");
    assert_eq!(parsed, req);
}

#[test]
fn upsert_metadata_request_schema_id_serializes_as_plain_string() {
    // Pin the wire shape: `GtsSchemaId` upstream serde forwards to
    // a plain JSON string. Switching the Rust API from `String` to
    // `GtsSchemaId` MUST NOT alter the bytes on the wire.
    let req = UpsertMetadataRequest::new(valid_schema_id(), json!({"k": "v"}));
    let value = serde_json::to_value(&req).expect("serialize");
    let object = value.as_object().expect("object");
    let sid = object.get("schema_id").expect("schema_id key");
    assert!(
        sid.is_string(),
        "schema_id MUST serialize as plain JSON string, got: {sid:?}"
    );
    assert_eq!(sid.as_str(), Some(VALID_SCHEMA_ID));
}

#[test]
fn metadata_entry_filter_fields_are_pinned() {
    // Pinned: the `$filter` / `$orderby` allow-list is part of the
    // public SDK contract. A new column (e.g. `created_at`) is a
    // SemVer minor bump for AM SDK and SHOULD show up here.
    use modkit_odata::filter::FilterField;

    let names: Vec<&'static str> = MetadataEntryFilterField::FIELDS
        .iter()
        .map(modkit_odata::filter::FilterField::name)
        .collect();
    assert_eq!(
        names,
        vec!["updated_at", "schema_uuid"],
        "MetadataEntryQuery filter-field surface drifted; bump SDK minor + update doc-comment when intentional"
    );
}

/// Sanity: omitting `value` from the wire body is a deserialize-time
/// error (serde missing-field). Pinned so a future `#[serde(default)]`
/// addition to the field is flagged as a wire-contract change.
#[test]
fn upsert_metadata_request_rejects_missing_value() {
    let bad = json!({ "schema_id": VALID_SCHEMA_ID });
    let err = serde_json::from_value::<UpsertMetadataRequest>(bad).expect_err("missing `value`");
    assert!(
        err.to_string().contains("missing field `value`"),
        "expected serde missing-field error, got: {err}"
    );
}

/// Sanity: a non-null `value` is accepted at the SDK boundary. The
/// AM impl-side validation rejects `Value::Null` and surfaces it as
/// `AccountManagementError::InvalidRequest` — the SDK type itself is
/// content-agnostic.
#[test]
fn upsert_metadata_request_accepts_any_non_missing_value() {
    let null_payload = json!({ "schema_id": VALID_SCHEMA_ID, "value": null });
    let parsed: UpsertMetadataRequest =
        serde_json::from_value(null_payload).expect("null OK at SDK");
    assert!(parsed.value.is_null(), "SDK does not reject Value::Null");

    let object_payload = json!({
        "schema_id": VALID_SCHEMA_ID,
        "value": { "k": "v" }
    });
    let parsed: UpsertMetadataRequest = serde_json::from_value(object_payload).expect("object");
    assert_eq!(parsed.value, json!({ "k": "v" }));
}

#[test]
fn metadata_entry_omits_unknown_status_field_on_serialize() {
    // Pinned: the SDK shape has exactly four fields. A new field
    // appearing on the wire WITHOUT a corresponding struct addition
    // would silently round-trip as JSON. This test guards the
    // existing shape; SDK-minor field additions update the test.
    let entry = MetadataEntry::new(
        valid_schema_id(),
        json!({"k": "v"}),
        datetime!(2025-01-01 00:00:00 UTC),
        7,
    );
    let value: serde_json::Value = serde_json::to_value(&entry).expect("serialize");
    let object = value.as_object().expect("object");
    let keys: Vec<&str> = object.keys().map(String::as_str).collect();
    let mut keys_sorted = keys.clone();
    keys_sorted.sort_unstable();
    assert_eq!(
        keys_sorted,
        vec!["schema_id", "updated_at", "value", "version"],
        "MetadataEntry wire keys drifted"
    );
}
