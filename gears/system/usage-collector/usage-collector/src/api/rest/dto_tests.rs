//! Wire-shape tests for the foundation REST DTOs.
//!
//! Pins the serde envelopes the OAS yaml relies on plus the
//! DTO ↔ SDK newtype boundary: the register-request DTO accepts any
//! string at deserialize so the handler can synthesise the canonical
//! `invalid_base_gts_id` `Problem` envelope on rejection rather than
//! surfacing axum's default `text/plain` 422.

use std::collections::BTreeMap;
use std::str::FromStr;

use rust_decimal::Decimal;
use time::OffsetDateTime;
use toolkit_canonical_errors::Problem;
use usage_collector_sdk::{
    IdempotencyKey, ResourceRef, UsageCollectorError, UsageRecord, UsageRecordStatus,
    UsageTypeGtsId, ValidationReason,
};
use uuid::Uuid;

use super::{
    CreateUsageRecordRequest, CreateUsageRecordResultDto, CreateUsageTypeRequest, UsageRecordDto,
};

const SAMPLE_USAGE_TYPE_ID: &str =
    "gts.cf.core.uc.usage_record.v1~cf.mini_chat._.tokens_consumed.v1";

const SAMPLE_RECORD_RFC3339: &str = "2026-06-11T12:34:56Z";
const SAMPLE_RECORD_VALUE: &str = "42.5";
const SAMPLE_IDEMPOTENCY_KEY: &str = "idem-dto-tests-1";

fn sample_record_uuid() -> Uuid {
    Uuid::from_u128(0x1111_1111_1111_1111_1111_1111_1111_1111_u128)
}

fn sample_tenant_uuid() -> Uuid {
    Uuid::from_u128(0x2222_2222_2222_2222_2222_2222_2222_2222_u128)
}

fn sample_persisted_record(status: UsageRecordStatus) -> UsageRecord {
    UsageRecord {
        uuid: sample_record_uuid(),
        gts_id: UsageTypeGtsId::new(SAMPLE_USAGE_TYPE_ID).expect("valid gts_id"),
        tenant_id: sample_tenant_uuid(),
        resource_ref: ResourceRef::new("rsc-dto", "compute.vm").expect("valid resource ref"),
        subject_ref: None,
        metadata: BTreeMap::new(),
        value: Decimal::from_str(SAMPLE_RECORD_VALUE).expect("valid decimal"),
        idempotency_key: IdempotencyKey::new(SAMPLE_IDEMPOTENCY_KEY).expect("valid idem key"),
        corrects_id: None,
        status,
        created_at: OffsetDateTime::parse(
            SAMPLE_RECORD_RFC3339,
            &time::format_description::well_known::Rfc3339,
        )
        .expect("RFC 3339 fixture parses"),
    }
}

#[test]
fn register_request_body_rejects_unknown_fields() {
    // Pin `#[serde(deny_unknown_fields)]`: an accidental field addition
    // on the wire must be rejected, not silently dropped.
    let json = serde_json::json!({
        "gts_id": SAMPLE_USAGE_TYPE_ID,
        "kind": "counter",
        "metadata_fields": ["tenant_id"],
        "extra": "should be rejected",
    });
    let err = serde_json::from_value::<CreateUsageTypeRequest>(json)
        .expect_err("deny_unknown_fields must reject extra members");
    let msg = err.to_string();
    assert!(
        msg.contains("unknown field"),
        "deserialize error MUST identify it as an `unknown field` (got `{msg}`)",
    );
    assert!(
        msg.contains("extra"),
        "deserialize error MUST name the offending field (got `{msg}`)",
    );
}

#[test]
fn register_request_body_is_permissive_at_deserialize() {
    // The DTO's `gts_id` field is a flat `String` so any well-formed
    // JSON string reaches the handler unchanged; structural /
    // base-derivation validation happens at the handler boundary via
    // `UsageTypeGtsId::new`, not at serde time. This test pins that
    // contract so a future tightening of the DTO field type does not
    // silently shift the rejection path back to axum's default
    // `422 text/plain`.
    let json = serde_json::json!({
        "gts_id": "not-a-valid-prefix",
        "kind": "counter",
        "metadata_fields": [],
    });
    let req: CreateUsageTypeRequest =
        serde_json::from_value(json).expect("DTO must accept any string at deserialize");
    let err = UsageTypeGtsId::new(req.gts_id)
        .expect_err("bad base gts_id must be rejected by UsageTypeGtsId::new");
    assert!(
        matches!(
            err,
            UsageCollectorError::InvalidArgument {
                reason: ValidationReason::InvalidBaseGtsId,
                ..
            }
        ),
        "expected InvalidUsageTypeGtsId, got {err:?}"
    );
}

// ---------------------------------------------------------------------------
// CreateUsageRecordRequest wire-contract pins.
//
// The handler tests in `handlers::usage_records_tests` construct
// `CreateUsageRecordRequest` directly with Rust values, bypassing serde — so a
// regression that drops `deny_unknown_fields`, flips `value` to numeric
// deserialization, drops the RFC 3339 wrapper on `created_at`, or removes the
// `Option<…>` / `BTreeMap::is_empty` defaults would still pass CI. These tests
// pin each of those serde attributes directly against the wire shape.
// ---------------------------------------------------------------------------

fn minimal_create_record_json() -> serde_json::Value {
    serde_json::json!({
        "uuid": sample_record_uuid().to_string(),
        "gts_id": SAMPLE_USAGE_TYPE_ID,
        "tenant_id": sample_tenant_uuid().to_string(),
        "resource_ref": {
            "resource_id": "rsc-dto",
            "resource_type": "compute.vm",
        },
        "value": SAMPLE_RECORD_VALUE,
        "idempotency_key": SAMPLE_IDEMPOTENCY_KEY,
        "created_at": SAMPLE_RECORD_RFC3339,
    })
}

#[test]
fn create_usage_record_request_rejects_unknown_fields() {
    // Pin `#[serde(deny_unknown_fields)]` on the per-record request. A future
    // accidental drop of the attribute would silently accept extra wire
    // members and let unrecognised fields through to the handler unnoticed.
    let mut json = minimal_create_record_json();
    json.as_object_mut()
        .expect("object")
        .insert("extra".to_owned(), serde_json::json!("nope"));
    let err = serde_json::from_value::<CreateUsageRecordRequest>(json)
        .expect_err("deny_unknown_fields must reject extra members");
    assert!(
        err.to_string().contains("extra"),
        "deserialize error MUST identify the unknown field (got `{err}`)",
    );
}

#[test]
fn create_usage_record_request_deserialises_value_as_string_only() {
    // Pin `#[serde(with = "rust_decimal::serde::str")]` on `value`: the wire
    // carries the decimal as a JSON string. Accepting a JSON number on input
    // would let callers ship `value: 0.1` and silently lose precision.
    let req: CreateUsageRecordRequest =
        serde_json::from_value(minimal_create_record_json()).expect("string-form value parses");
    assert_eq!(
        req.value,
        Decimal::from_str(SAMPLE_RECORD_VALUE).expect("fixture decimal parses"),
    );

    let mut numeric_json = minimal_create_record_json();
    numeric_json
        .as_object_mut()
        .expect("object")
        .insert("value".to_owned(), serde_json::json!(42.5));
    let err = serde_json::from_value::<CreateUsageRecordRequest>(numeric_json)
        .expect_err("numeric `value` MUST be rejected - wire contract is string-only");
    assert!(
        err.to_string().contains("Decimal"),
        "deserialize error MUST identify the `Decimal` type the codec expected \
         (got `{err}`)",
    );
}

#[test]
fn create_usage_record_request_deserialises_created_at_as_rfc3339_only() {
    // Pin `#[serde(with = "time::serde::rfc3339")]` on `created_at`: the wire
    // carries the timestamp as a JSON string in RFC 3339 form. A regression
    // that swapped this for the default `OffsetDateTime` serde codec would
    // accept a numeric Unix timestamp and break every existing client.
    let req: CreateUsageRecordRequest = serde_json::from_value(minimal_create_record_json())
        .expect("RFC 3339 string for `created_at` parses");
    let expected = OffsetDateTime::parse(
        SAMPLE_RECORD_RFC3339,
        &time::format_description::well_known::Rfc3339,
    )
    .expect("fixture parses");
    assert_eq!(req.created_at, expected);

    let mut numeric_json = minimal_create_record_json();
    numeric_json.as_object_mut().expect("object").insert(
        "created_at".to_owned(),
        serde_json::json!(1_700_000_000_i64),
    );
    let err = serde_json::from_value::<CreateUsageRecordRequest>(numeric_json)
        .expect_err("non-string `created_at` MUST be rejected");
    assert!(
        err.to_string().contains("RFC3339"),
        "deserialize error MUST identify the RFC 3339 codec the field expected \
         (got `{err}`)",
    );
}

#[test]
fn create_usage_record_request_optional_subject_ref_defaults_to_none() {
    // The minimal fixture omits `subject_ref` entirely; pin `#[serde(default,
    // skip_serializing_if = "Option::is_none")]` on the request side by
    // proving the field deserialises to `None` when absent.
    let req: CreateUsageRecordRequest = serde_json::from_value(minimal_create_record_json())
        .expect("missing subject_ref must default to None");
    assert!(
        req.subject_ref.is_none(),
        "absent `subject_ref` MUST deserialise to None (got {:?})",
        req.subject_ref,
    );
}

#[test]
fn create_usage_record_request_optional_metadata_defaults_to_empty_map() {
    // The minimal fixture omits `metadata` entirely; pin `#[serde(default,
    // skip_serializing_if = "BTreeMap::is_empty")]` on the request side by
    // proving the field deserialises to an empty map when absent.
    let req: CreateUsageRecordRequest = serde_json::from_value(minimal_create_record_json())
        .expect("missing metadata must default to empty map");
    assert!(
        req.metadata.is_empty(),
        "absent `metadata` MUST deserialise to an empty BTreeMap (got {:?})",
        req.metadata,
    );
}

#[test]
fn create_usage_record_request_optional_corrects_id_defaults_to_none() {
    // Pin `#[serde(default, skip_serializing_if = "Option::is_none")]` on
    // `corrects_id`: absent on ordinary submissions, present only for counter
    // compensations.
    let req: CreateUsageRecordRequest = serde_json::from_value(minimal_create_record_json())
        .expect("missing corrects_id must default to None");
    assert!(
        req.corrects_id.is_none(),
        "absent `corrects_id` MUST deserialise to None (got {:?})",
        req.corrects_id,
    );
}

// ---------------------------------------------------------------------------
// UsageRecordDto wire-contract pins.
//
// `UsageRecordDto` is the response projection of `UsageRecord`. The handler
// tests assert the persisted-record UUID makes it through but never check the
// status / value / created_at / metadata projections — a regression in any of
// `serde(with = "rust_decimal::serde::str")`, `serde(with =
// "time::serde::rfc3339")`, the status `"active"` / `"inactive"` mapping, or
// the empty-metadata skip would not fail any existing test. The pins below
// cover each.
// ---------------------------------------------------------------------------

#[test]
fn usage_record_dto_serialises_active_status_as_lowercase_string() {
    let dto = UsageRecordDto::from(sample_persisted_record(UsageRecordStatus::Active));
    let json = serde_json::to_value(&dto).expect("UsageRecordDto serialises");
    assert_eq!(
        json.get("status").and_then(serde_json::Value::as_str),
        Some("active"),
        "Active MUST project to lowercase string `active` (got {:?})",
        json.get("status"),
    );
}

#[test]
fn usage_record_dto_serialises_inactive_status_as_lowercase_string() {
    let dto = UsageRecordDto::from(sample_persisted_record(UsageRecordStatus::Inactive));
    let json = serde_json::to_value(&dto).expect("UsageRecordDto serialises");
    assert_eq!(
        json.get("status").and_then(serde_json::Value::as_str),
        Some("inactive"),
        "Inactive MUST project to lowercase string `inactive` (got {:?})",
        json.get("status"),
    );
}

#[test]
fn usage_record_dto_serialises_value_as_string_and_created_at_as_rfc3339() {
    let dto = UsageRecordDto::from(sample_persisted_record(UsageRecordStatus::Active));
    let json = serde_json::to_value(&dto).expect("UsageRecordDto serialises");
    assert_eq!(
        json.get("value").and_then(serde_json::Value::as_str),
        Some(SAMPLE_RECORD_VALUE),
        "`value` MUST be emitted as a JSON string (not a number)",
    );
    assert_eq!(
        json.get("created_at").and_then(serde_json::Value::as_str),
        Some(SAMPLE_RECORD_RFC3339),
        "`created_at` MUST be emitted as an RFC 3339 string",
    );
}

#[test]
fn usage_record_dto_omits_empty_metadata_and_absent_subject_ref() {
    // Empty `metadata` / `None` `subject_ref` / `None` `corrects_id` MUST be
    // skipped on the wire so the OAS response shape stays minimal. A
    // regression that dropped `skip_serializing_if` would surface them as
    // `metadata: {}` / `subject_ref: null` / `corrects_id: null` and break
    // OAS-clients that treat absent and null as distinct.
    let dto = UsageRecordDto::from(sample_persisted_record(UsageRecordStatus::Active));
    let json = serde_json::to_value(&dto).expect("UsageRecordDto serialises");
    let obj = json
        .as_object()
        .expect("UsageRecordDto serialises to an object");
    assert!(
        !obj.contains_key("metadata"),
        "empty metadata MUST be omitted (got {obj:?})",
    );
    assert!(
        !obj.contains_key("subject_ref"),
        "absent subject_ref MUST be omitted (got {obj:?})",
    );
    assert!(
        !obj.contains_key("corrects_id"),
        "absent corrects_id MUST be omitted (got {obj:?})",
    );
}

// ---------------------------------------------------------------------------
// CreateUsageRecordResultDto wire-contract pin.
//
// Pin the externally-tagged `outcome` discriminator: `Accepted` MUST surface
// as `outcome: "accepted"` with sibling `index` / `record` fields, and
// `Rejected` as `outcome: "rejected"` with sibling `index` / `error` fields.
// A regression that dropped the `#[toolkit_macros::api_dto(response)]`
// snake-case rename, or flipped the tag attribute, would shift either side
// silently and break every batched-create consumer.
// ---------------------------------------------------------------------------

#[test]
fn create_usage_record_result_dto_serialises_accepted_with_lowercase_tag() {
    let dto = CreateUsageRecordResultDto::Accepted {
        index: 0,
        record: UsageRecordDto::from(sample_persisted_record(UsageRecordStatus::Active)),
    };
    let json = serde_json::to_value(&dto).expect("Accepted serialises");
    let obj = json.as_object().expect("Accepted serialises to an object");
    assert_eq!(
        obj.get("outcome").and_then(serde_json::Value::as_str),
        Some("accepted"),
        "Accepted MUST tag as `outcome: \"accepted\"` (got {obj:?})",
    );
    assert_eq!(
        obj.get("index").and_then(serde_json::Value::as_u64),
        Some(0),
        "Accepted MUST carry the per-record `index` as a sibling field",
    );
    let record = obj
        .get("record")
        .and_then(serde_json::Value::as_object)
        .expect("Accepted MUST carry a `record` object sibling");
    assert_eq!(
        record.get("uuid").and_then(serde_json::Value::as_str),
        Some(sample_record_uuid().to_string().as_str()),
        "Accepted.record.uuid MUST be the persisted record's uuid \
         - a regression that dropped the projection would surface here",
    );
}

#[test]
fn create_usage_record_result_dto_serialises_rejected_with_lowercase_tag() {
    let problem = Problem {
        problem_type: "gts://gts.cf.core.errors.err.v1~cf.core.err.invalid_argument.v1~".to_owned(),
        title: "Invalid argument".to_owned(),
        status: 400,
        detail: "test rejection".to_owned(),
        instance: None,
        trace_id: None,
        context: serde_json::json!({}),
    };
    let dto = CreateUsageRecordResultDto::Rejected {
        index: 1,
        error: problem,
    };
    let json = serde_json::to_value(&dto).expect("Rejected serialises");
    let obj = json.as_object().expect("Rejected serialises to an object");
    assert_eq!(
        obj.get("outcome").and_then(serde_json::Value::as_str),
        Some("rejected"),
        "Rejected MUST tag as `outcome: \"rejected\"` (got {obj:?})",
    );
    assert_eq!(
        obj.get("index").and_then(serde_json::Value::as_u64),
        Some(1),
        "Rejected MUST carry the per-record `index` as a sibling field",
    );
    let error = obj
        .get("error")
        .and_then(serde_json::Value::as_object)
        .expect("Rejected MUST carry an `error` Problem object sibling");
    assert_eq!(
        error.get("status").and_then(serde_json::Value::as_u64),
        Some(400),
        "Rejected.error.status MUST mirror the Problem's HTTP status \
         (a regression that flattened or shadowed the Problem fields would surface here)",
    );
}
