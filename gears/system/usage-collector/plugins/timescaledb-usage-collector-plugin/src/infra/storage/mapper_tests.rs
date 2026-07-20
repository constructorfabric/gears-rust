use std::collections::BTreeMap;

use rust_decimal::Decimal;
use serde_json::Value as JsonValue;
use time::OffsetDateTime;
use uuid::Uuid;

use usage_collector_sdk::{
    MetadataKey, UsageCollectorPluginError, UsageKind, UsageRecordStatus, UsageTypeGtsId,
};

use super::super::entity::{UsageRecordRow, UsageTypeRow};
use super::{
    gts_id_from_str, kind_to_sql, metadata_jsonb_to_map, metadata_map_to_jsonb, parse_kind,
    parse_status, record_row_to_model, status_to_sql, type_row_to_model,
};

// ── status round-trip ────────────────────────────────────────────────────────

#[test]
fn parse_status_round_trips_through_sql_form() {
    for status in [UsageRecordStatus::Active, UsageRecordStatus::Inactive] {
        let sql = status_to_sql(status);
        assert_eq!(parse_status(sql).unwrap(), status);
    }
}

#[test]
fn parse_status_rejects_unknown() {
    assert!(parse_status("archived").is_err());
}

// ── kind round-trip ──────────────────────────────────────────────────────────

#[test]
fn parse_kind_round_trips_through_sql_form() {
    for kind in [UsageKind::Counter, UsageKind::Gauge] {
        let sql = kind_to_sql(kind);
        assert_eq!(parse_kind(sql).unwrap(), kind);
    }
}

#[test]
fn kind_to_sql_emits_lowercase_wire_tokens() {
    assert_eq!(kind_to_sql(UsageKind::Counter), "counter");
    assert_eq!(kind_to_sql(UsageKind::Gauge), "gauge");
}

#[test]
fn parse_kind_rejects_unknown() {
    assert!(parse_kind("histogram").is_err());
}

// ── metadata jsonb <-> map round-trip ────────────────────────────────────────

#[test]
fn metadata_map_to_jsonb_then_back_round_trips() {
    let mut map = BTreeMap::new();
    map.insert(MetadataKey::new("region").unwrap(), "eu-west".to_owned());
    map.insert(MetadataKey::new("tier").unwrap(), "gold".to_owned());

    let json = metadata_map_to_jsonb(&map);
    let back = metadata_jsonb_to_map(json).unwrap();
    assert_eq!(back, map);
}

#[test]
fn empty_metadata_round_trips() {
    let map: BTreeMap<MetadataKey, String> = BTreeMap::new();
    let json = metadata_map_to_jsonb(&map);
    assert_eq!(json, JsonValue::Object(serde_json::Map::new()));
    assert!(metadata_jsonb_to_map(json).unwrap().is_empty());
}

#[test]
fn metadata_jsonb_null_maps_to_empty() {
    assert!(metadata_jsonb_to_map(JsonValue::Null).unwrap().is_empty());
}

#[test]
fn metadata_jsonb_non_object_is_rejected() {
    assert!(metadata_jsonb_to_map(JsonValue::String("x".to_owned())).is_err());
}

#[test]
fn metadata_jsonb_non_string_value_is_rejected() {
    let mut obj = serde_json::Map::new();
    obj.insert("region".to_owned(), JsonValue::Bool(true));
    assert!(metadata_jsonb_to_map(JsonValue::Object(obj)).is_err());
}

// ── gts_id primitive ─────────────────────────────────────────────────────────

/// A well-formed usage-type GTS instance id deriving from the reserved base
/// (`gts.cf.core.uc.usage_record.v1~`).
const VALID_GTS_ID: &str = "gts.cf.core.uc.usage_record.v1~cf.compute._.vcpu_hours.v1";

#[test]
fn gts_id_from_str_accepts_valid_and_rejects_invalid_as_internal() {
    assert!(gts_id_from_str(VALID_GTS_ID).is_ok());
    // A stored value that no longer validates is a plugin invariant break, not
    // a caller error — it MUST surface as `Internal`.
    assert!(matches!(
        gts_id_from_str("not-a-valid-gts-id"),
        Err(UsageCollectorPluginError::Internal(_))
    ));
}

// ── record row -> model ──────────────────────────────────────────────────────

fn valid_metadata_json() -> JsonValue {
    let mut obj = serde_json::Map::new();
    obj.insert("region".to_owned(), JsonValue::String("eu-west".to_owned()));
    JsonValue::Object(obj)
}

/// A fully valid `usage_records` row. Tests corrupt one field at a time and
/// assert the mapper fails closed with `Internal`.
fn valid_record_row() -> UsageRecordRow {
    UsageRecordRow {
        id: Uuid::from_u128(1),
        tenant_id: Uuid::from_u128(2),
        gts_id: VALID_GTS_ID.to_owned(),
        value: Decimal::new(425, 1), // 42.5
        created_at: OffsetDateTime::from_unix_timestamp(1_700_000_000).unwrap(),
        resource_id: "res-1".to_owned(),
        resource_type: "compute.vm".to_owned(),
        subject_id: Some("subj-1".to_owned()),
        subject_type: Some("user".to_owned()),
        idempotency_key: "idem-1".to_owned(),
        corrects_id: None,
        status: "active".to_owned(),
        metadata: valid_metadata_json(),
        ingested_at: OffsetDateTime::from_unix_timestamp(1_700_000_100).unwrap(),
    }
}

#[test]
fn record_row_to_model_maps_a_valid_row_round_trip() {
    let row = valid_record_row();
    let model = record_row_to_model(row).expect("a fully valid row maps");

    assert_eq!(model.id, Uuid::from_u128(1));
    assert_eq!(model.tenant_id, Uuid::from_u128(2));
    assert_eq!(model.gts_id, UsageTypeGtsId::new(VALID_GTS_ID).unwrap());
    assert_eq!(model.value, Decimal::new(425, 1));
    assert_eq!(
        model.created_at,
        OffsetDateTime::from_unix_timestamp(1_700_000_000).unwrap()
    );
    assert_eq!(model.resource_ref.resource_id(), "res-1");
    assert_eq!(model.resource_ref.resource_type(), "compute.vm");
    let subject = model.subject_ref.as_ref().expect("subject present");
    assert_eq!(subject.subject_id(), "subj-1");
    assert_eq!(subject.subject_type(), Some("user"));
    assert_eq!(model.idempotency_key.as_str(), "idem-1");
    assert_eq!(model.corrects_id, None);
    assert_eq!(model.status, UsageRecordStatus::Active);
    assert_eq!(
        model.metadata.get(&MetadataKey::new("region").unwrap()),
        Some(&"eu-west".to_owned())
    );
}

#[test]
fn record_row_absent_subject_maps_to_none() {
    let mut row = valid_record_row();
    row.subject_id = None;
    row.subject_type = None;
    let model = record_row_to_model(row).expect("a row without a subject maps");
    assert!(model.subject_ref.is_none());
}

#[test]
fn record_row_invalid_gts_id_is_internal() {
    let mut row = valid_record_row();
    row.gts_id = "not-a-valid-gts-id".to_owned();
    assert!(matches!(
        record_row_to_model(row),
        Err(UsageCollectorPluginError::Internal(_))
    ));
}

#[test]
fn record_row_invalid_resource_ref_is_internal() {
    let mut row = valid_record_row();
    row.resource_id = String::new(); // empty resource_id fails ResourceRef::new
    assert!(matches!(
        record_row_to_model(row),
        Err(UsageCollectorPluginError::Internal(_))
    ));
}

#[test]
fn record_row_invalid_subject_ref_is_internal() {
    let mut row = valid_record_row();
    row.subject_id = Some(String::new()); // present-but-empty subject_id is rejected
    assert!(matches!(
        record_row_to_model(row),
        Err(UsageCollectorPluginError::Internal(_))
    ));
}

#[test]
fn record_row_invalid_idempotency_key_is_internal() {
    let mut row = valid_record_row();
    row.idempotency_key = String::new();
    assert!(matches!(
        record_row_to_model(row),
        Err(UsageCollectorPluginError::Internal(_))
    ));
}

#[test]
fn record_row_non_object_metadata_is_internal() {
    let mut row = valid_record_row();
    row.metadata = JsonValue::String("not-an-object".to_owned());
    assert!(matches!(
        record_row_to_model(row),
        Err(UsageCollectorPluginError::Internal(_))
    ));
}

#[test]
fn record_row_unknown_status_is_internal() {
    let mut row = valid_record_row();
    row.status = "archived".to_owned();
    assert!(matches!(
        record_row_to_model(row),
        Err(UsageCollectorPluginError::Internal(_))
    ));
}

// ── usage-type row -> model ──────────────────────────────────────────────────

fn valid_type_row() -> UsageTypeRow {
    UsageTypeRow {
        gts_id: VALID_GTS_ID.to_owned(),
        kind: "counter".to_owned(),
        metadata_fields: vec!["region".to_owned(), "tier".to_owned()],
    }
}

#[test]
fn type_row_to_model_maps_a_valid_row() {
    let model = type_row_to_model(valid_type_row()).expect("a fully valid type row maps");
    assert_eq!(model.gts_id, UsageTypeGtsId::new(VALID_GTS_ID).unwrap());
    assert_eq!(model.kind, UsageKind::Counter);
    assert_eq!(model.metadata_fields.len(), 2);
    assert!(
        model
            .metadata_fields
            .contains(&MetadataKey::new("region").unwrap())
    );
    assert!(
        model
            .metadata_fields
            .contains(&MetadataKey::new("tier").unwrap())
    );
}

#[test]
fn type_row_invalid_gts_id_is_internal() {
    let mut row = valid_type_row();
    row.gts_id = "not-a-valid-gts-id".to_owned();
    assert!(matches!(
        type_row_to_model(row),
        Err(UsageCollectorPluginError::Internal(_))
    ));
}

#[test]
fn type_row_invalid_kind_is_internal() {
    let mut row = valid_type_row();
    row.kind = "histogram".to_owned();
    assert!(matches!(
        type_row_to_model(row),
        Err(UsageCollectorPluginError::Internal(_))
    ));
}

#[test]
fn type_row_invalid_metadata_field_is_internal() {
    let mut row = valid_type_row();
    row.metadata_fields = vec![String::new()]; // empty key fails MetadataKey::new
    assert!(matches!(
        type_row_to_model(row),
        Err(UsageCollectorPluginError::Internal(_))
    ));
}
