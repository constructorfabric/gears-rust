//! Unit tests for the shape-validation algorithm.

use std::collections::{BTreeMap, BTreeSet};

use rust_decimal::Decimal;
use time::OffsetDateTime;
use usage_collector_sdk::{
    ConflictReason, IdempotencyKey, MetadataKey, ResourceRef, SubjectRef, UsageCollectorError,
    UsageKind, UsageRecord, UsageRecordStatus, UsageType, UsageTypeGtsId, ValidationReason,
};
use uuid::Uuid;

use super::{
    RECORD_METADATA_SIZE_CAP_BYTES, SemanticsOutcome, metadata_fields_from_wire,
    validate_record_semantics, validate_submit_record_metadata, verify_l1_corrects_id,
};

fn mk_key(value: &str) -> MetadataKey {
    MetadataKey::new(value).expect("test fixture supplies a valid metadata key")
}

fn mk_keys<const N: usize>(values: [&str; N]) -> BTreeSet<MetadataKey> {
    values.into_iter().map(mk_key).collect()
}

const SAMPLE_COUNTER_ID: &str = "gts.cf.core.uc.usage_record.v1~tenant.example._.foo.v1";
const SAMPLE_GAUGE_ID: &str = "gts.cf.core.uc.usage_record.v1~tenant.example._.bar.v1";

fn counter_id() -> UsageTypeGtsId {
    UsageTypeGtsId::new(SAMPLE_COUNTER_ID).expect("valid usage_record-derived id")
}

fn gauge_id() -> UsageTypeGtsId {
    UsageTypeGtsId::new(SAMPLE_GAUGE_ID).expect("valid usage_record-derived id")
}

#[test]
fn metadata_fields_from_wire_empty_vec_yields_empty_set() {
    let set = metadata_fields_from_wire(Vec::new()).expect("empty input accepted");
    assert!(set.is_empty());

    let usage_type = UsageType {
        gts_id: counter_id(),
        kind: UsageKind::Counter,
        metadata_fields: set,
    };
    validate_submit_record_metadata(&usage_type, &BTreeMap::new())
        .expect("empty metadata_fields must accept an empty record metadata payload");

    let mut bad = BTreeMap::new();
    bad.insert(mk_key("region"), "us-east".to_owned());
    let err = validate_submit_record_metadata(&usage_type, &bad)
        .expect_err("empty metadata_fields must reject any wire metadata key");
    assert!(
        matches!(
            err,
            UsageCollectorError::InvalidArgument {
                reason: ValidationReason::UnknownMetadataKey,
                ..
            }
        ),
        "expected UnknownMetadataKey, got {err:?}"
    );
}

#[test]
fn metadata_fields_from_wire_legitimate_passes() {
    let set =
        metadata_fields_from_wire(vec!["region".to_owned()]).expect("single declared key accepted");
    assert_eq!(set, mk_keys(["region"]));
}

// `inst-algo-shape-invalid-metadata-fields` — duplicate entry at index `2`
// returns `DuplicateMetadataField` carrying the offending index.
#[test]
fn metadata_fields_from_wire_duplicate_returns_metadata_validation_error() {
    let err = metadata_fields_from_wire(vec![
        "region".to_owned(),
        "az".to_owned(),
        "region".to_owned(),
    ])
    .expect_err("duplicate metadata field must be rejected");
    assert!(
        matches!(
            err,
            UsageCollectorError::InvalidArgument {
                reason: ValidationReason::MetadataFieldDuplicate,
                ref field,
                ..
            } if field == "metadata_fields[2]"
        ),
        "expected DuplicateMetadataField {{ index: 2 }}, got {err:?}"
    );
}

// `inst-algo-shape-invalid-metadata-fields` — first empty string at index `1`
// is rejected before any duplicate-detection pass continues.
#[test]
fn metadata_fields_from_wire_empty_string_rejected() {
    let err = metadata_fields_from_wire(vec!["region".to_owned(), String::new()])
        .expect_err("empty metadata field must be rejected");
    match err {
        UsageCollectorError::InvalidArgument {
            reason: ValidationReason::MetadataFieldEmptyString,
            ref field,
            ..
        } => {
            assert_eq!(field, "metadata_fields[1]", "expected index 1, got {field}");
        }
        other => panic!("expected InvalidMetadataField, got {other:?}"),
    }
}

// `MetadataKey::new` rejects NUL bytes — wire-shape conversion surfaces
// them as a typed `InvalidMetadataField` alongside the other malformed-key
// outcomes, with the offending key's index attached.
#[test]
fn metadata_fields_from_wire_nul_byte_rejected() {
    let err = metadata_fields_from_wire(vec!["bad\0key".to_owned()])
        .expect_err("NUL byte in metadata key must be rejected");
    assert!(
        matches!(
            err,
            UsageCollectorError::InvalidArgument {
                reason: ValidationReason::MetadataFieldInvalidKey,
                ref field,
                ..
            } if field == "metadata_fields[0]"
        ),
        "expected InvalidMetadataField {{ index: 0, .. }}, got {err:?}"
    );
}

// ---------------------------------------------------------------------------
// Metadata size-cap enforcement
// (`cpt-cf-usage-collector-algo-usage-emission-metadata-size-cap-enforcement`)
//
// The serialized-size gate (`size > RECORD_METADATA_SIZE_CAP_BYTES`) is a
// `>`, not `>=`, comparison — exactly at the cap is accepted; one byte over
// is rejected. The value length is sized off the measured single-entry
// overhead so the boundary holds regardless of `MetadataKey`'s serde shape.
// ---------------------------------------------------------------------------

fn size_cap_usage_type() -> UsageType {
    UsageType {
        gts_id: counter_id(),
        kind: UsageKind::Counter,
        metadata_fields: mk_keys(["blob"]),
    }
}

/// Serialized overhead of a one-entry `{ "blob": "" }` map, so a value can be
/// sized to land the serialized payload exactly on the cap boundary.
fn single_entry_overhead() -> usize {
    let mut probe = BTreeMap::new();
    probe.insert(mk_key("blob"), String::new());
    serde_json::to_vec(&probe)
        .expect("probe serialization is infallible")
        .len()
}

#[test]
fn metadata_one_byte_over_size_cap_is_rejected() {
    let usage_type = size_cap_usage_type();
    let mut metadata = BTreeMap::new();
    metadata.insert(
        mk_key("blob"),
        "x".repeat(RECORD_METADATA_SIZE_CAP_BYTES - single_entry_overhead() + 1),
    );
    let err = validate_submit_record_metadata(&usage_type, &metadata)
        .expect_err("metadata one byte over the cap must reject");
    assert!(
        matches!(
            err,
            UsageCollectorError::InvalidArgument {
                reason: ValidationReason::MetadataValidation,
                ..
            }
        ),
        "expected MetadataValidation size-cap rejection, got {err:?}",
    );
}

#[test]
fn metadata_exactly_at_size_cap_is_accepted() {
    let usage_type = size_cap_usage_type();
    let mut metadata = BTreeMap::new();
    metadata.insert(
        mk_key("blob"),
        "x".repeat(RECORD_METADATA_SIZE_CAP_BYTES - single_entry_overhead()),
    );
    validate_submit_record_metadata(&usage_type, &metadata)
        .expect("metadata serialized exactly to the cap must be accepted");
}

// ---------------------------------------------------------------------------
// Semantics-enforcement algorithm
// (`cpt-cf-usage-collector-algo-usage-emission-semantics-enforcement-on-ingest-v2`)
//
// Covers each cell of the locked four-cell value matrix plus every L1
// referential failure mode.
// ---------------------------------------------------------------------------

fn counter_usage_type() -> UsageType {
    UsageType {
        gts_id: counter_id(),
        kind: UsageKind::Counter,
        metadata_fields: BTreeSet::new(),
    }
}

fn gauge_usage_type() -> UsageType {
    UsageType {
        gts_id: gauge_id(),
        kind: UsageKind::Gauge,
        metadata_fields: BTreeSet::new(),
    }
}

fn ordinary_counter_record(value: Decimal) -> UsageRecord {
    UsageRecord {
        uuid: Uuid::new_v4(),
        gts_id: counter_id(),
        tenant_id: Uuid::from_u128(1),
        resource_ref: ResourceRef::new("rsc-1", "compute.vm").expect("valid resource ref"),
        subject_ref: None,
        metadata: BTreeMap::new(),
        value,
        idempotency_key: IdempotencyKey::new(format!("idem-validation-test-{value}"))
            .expect("valid idempotency key"),
        corrects_id: None,
        status: UsageRecordStatus::Active,
        created_at: OffsetDateTime::UNIX_EPOCH,
    }
}

fn counter_compensation_record(value: Decimal, corrects_id: Uuid) -> UsageRecord {
    let mut record = ordinary_counter_record(value);
    record.corrects_id = Some(corrects_id);
    record
}

fn referenced_ordinary_row(tenant: Uuid) -> UsageRecord {
    let mut record = ordinary_counter_record(Decimal::from(10));
    record.tenant_id = tenant;
    record
}

#[test]
fn counter_ordinary_with_non_negative_value_is_valid() {
    let ut = counter_usage_type();
    let record = ordinary_counter_record(Decimal::ZERO);
    assert_eq!(
        validate_record_semantics(&ut, &record).unwrap(),
        SemanticsOutcome::Valid
    );
}

#[test]
fn counter_ordinary_with_negative_value_is_semantics_violation() {
    let ut = counter_usage_type();
    let record = ordinary_counter_record(Decimal::from(-1));
    let err = validate_record_semantics(&ut, &record).expect_err("negative counter rejected");
    assert!(
        matches!(
            err,
            UsageCollectorError::InvalidArgument {
                reason: ValidationReason::SemanticsViolation,
                ref detail,
                ..
            } if detail.contains("value >= 0") && detail.contains("got -1")
        ),
        "expected NegativeCounterValue {{ value: -1 }}, got {err:?}"
    );
}

#[test]
fn counter_compensation_with_non_negative_value_is_semantics_violation() {
    let ut = counter_usage_type();
    let record = counter_compensation_record(Decimal::ZERO, Uuid::new_v4());
    let err = validate_record_semantics(&ut, &record).expect_err("zero compensation rejected");
    assert!(
        matches!(
            err,
            UsageCollectorError::InvalidArgument {
                reason: ValidationReason::SemanticsViolation,
                ref detail,
                ..
            } if detail.contains("value < 0") && detail.contains("got 0")
        ),
        "expected NonNegativeCounterCompensation {{ value: 0 }}, got {err:?}"
    );
}

#[test]
fn counter_compensation_with_negative_value_needs_l1_lookup() {
    let ut = counter_usage_type();
    let corrects_id = Uuid::new_v4();
    let record = counter_compensation_record(Decimal::from(-5), corrects_id);
    assert_eq!(
        validate_record_semantics(&ut, &record).unwrap(),
        SemanticsOutcome::NeedsL1Lookup { corrects_id },
    );
}

#[test]
fn gauge_ordinary_accepts_any_signed_value() {
    let ut = gauge_usage_type();
    let mut record = ordinary_counter_record(Decimal::from(-9999));
    record.gts_id = gauge_id();
    assert_eq!(
        validate_record_semantics(&ut, &record).unwrap(),
        SemanticsOutcome::Valid
    );
}

#[test]
fn gauge_with_corrects_id_is_rejected_dedicated_variant() {
    let ut = gauge_usage_type();
    let mut record = counter_compensation_record(Decimal::from(-1), Uuid::new_v4());
    record.gts_id = gauge_id();
    let err = validate_record_semantics(&ut, &record).expect_err("gauge compensation rejected");
    assert!(matches!(
        err,
        UsageCollectorError::InvalidArgument {
            reason: ValidationReason::GaugeCompensationRejected,
            ..
        }
    ));
}

// The `corrects_id not found` translation lives at the service-layer call
// site (`Service::create_usage_record` / `create_usage_records`) where the
// plugin's `UsageRecordNotFound` is re-classified as `CorrectsIdNotFound`;
// `verify_l1_corrects_id` only sees an existing row.

#[test]
fn l1_referenced_row_that_is_compensation_is_rejected() {
    let record = counter_compensation_record(Decimal::from(-1), Uuid::new_v4());
    let mut referenced = referenced_ordinary_row(record.tenant_id);
    referenced.corrects_id = Some(Uuid::new_v4());
    let err = verify_l1_corrects_id(
        &record,
        record.corrects_id.expect("test fixture sets corrects_id"),
        &referenced,
    )
    .expect_err("compensation target rejected");
    assert!(matches!(
        err,
        UsageCollectorError::Conflict {
            reason: ConflictReason::CorrectsIdTargetsCompensation,
            ..
        }
    ));
}

#[test]
fn l1_cross_tenant_reference_is_rejected_as_wrong_scope() {
    let record = counter_compensation_record(Decimal::from(-1), Uuid::new_v4());
    let referenced = referenced_ordinary_row(Uuid::from_u128(42));
    let err = verify_l1_corrects_id(
        &record,
        record.corrects_id.expect("test fixture sets corrects_id"),
        &referenced,
    )
    .expect_err("cross-tenant rejected");
    assert!(matches!(
        err,
        UsageCollectorError::Conflict {
            reason: ConflictReason::CorrectsIdWrongScope,
            ..
        }
    ));
}

#[test]
fn l1_cross_usage_type_reference_is_rejected_as_wrong_scope() {
    let record = counter_compensation_record(Decimal::from(-1), Uuid::new_v4());
    let mut referenced = referenced_ordinary_row(record.tenant_id);
    referenced.gts_id = gauge_id();
    let err = verify_l1_corrects_id(
        &record,
        record.corrects_id.expect("test fixture sets corrects_id"),
        &referenced,
    )
    .expect_err("cross-usage-type rejected");
    assert!(matches!(
        err,
        UsageCollectorError::Conflict {
            reason: ConflictReason::CorrectsIdWrongScope,
            ..
        }
    ));
}

#[test]
fn l1_different_resource_is_rejected_as_wrong_scope() {
    let record = counter_compensation_record(Decimal::from(-1), Uuid::new_v4());
    let mut referenced = referenced_ordinary_row(record.tenant_id);
    referenced.resource_ref =
        ResourceRef::new("rsc-other", "compute.vm").expect("valid resource ref");
    let err = verify_l1_corrects_id(
        &record,
        record.corrects_id.expect("test fixture sets corrects_id"),
        &referenced,
    )
    .expect_err("cross-resource compensation rejected");
    assert!(matches!(
        err,
        UsageCollectorError::Conflict {
            reason: ConflictReason::CorrectsIdWrongScope,
            ..
        }
    ));
}

#[test]
fn l1_subject_presence_mismatch_is_rejected_as_wrong_scope() {
    let record = counter_compensation_record(Decimal::from(-1), Uuid::new_v4());
    let mut referenced = referenced_ordinary_row(record.tenant_id);
    referenced.subject_ref =
        Some(SubjectRef::new("user-1", Option::<&str>::None).expect("valid subject ref"));
    let err = verify_l1_corrects_id(
        &record,
        record.corrects_id.expect("test fixture sets corrects_id"),
        &referenced,
    )
    .expect_err("subject-presence mismatch rejected");
    assert!(matches!(
        err,
        UsageCollectorError::Conflict {
            reason: ConflictReason::CorrectsIdWrongScope,
            ..
        }
    ));
}

#[test]
fn l1_different_subject_is_rejected_as_wrong_scope() {
    let subject_a = SubjectRef::new("user-a", Some("end_user")).expect("valid subject ref");
    let subject_b = SubjectRef::new("user-b", Some("end_user")).expect("valid subject ref");
    let mut record = counter_compensation_record(Decimal::from(-1), Uuid::new_v4());
    record.subject_ref = Some(subject_a);
    let mut referenced = referenced_ordinary_row(record.tenant_id);
    referenced.subject_ref = Some(subject_b);
    let err = verify_l1_corrects_id(
        &record,
        record.corrects_id.expect("test fixture sets corrects_id"),
        &referenced,
    )
    .expect_err("cross-subject compensation rejected");
    assert!(matches!(
        err,
        UsageCollectorError::Conflict {
            reason: ConflictReason::CorrectsIdWrongScope,
            ..
        }
    ));
}

#[test]
fn l1_matching_subject_passes_referential_check() {
    let subject = SubjectRef::new("user-1", Some("end_user")).expect("valid subject ref");
    let mut record = counter_compensation_record(Decimal::from(-1), Uuid::new_v4());
    record.subject_ref = Some(subject.clone());
    let mut referenced = referenced_ordinary_row(record.tenant_id);
    referenced.subject_ref = Some(subject);
    assert!(
        verify_l1_corrects_id(
            &record,
            record.corrects_id.expect("test fixture sets corrects_id"),
            &referenced
        )
        .is_ok()
    );
}

#[test]
fn l1_inactive_referenced_row_is_rejected() {
    let record = counter_compensation_record(Decimal::from(-1), Uuid::new_v4());
    let mut referenced = referenced_ordinary_row(record.tenant_id);
    referenced.status = UsageRecordStatus::Inactive;
    let err = verify_l1_corrects_id(
        &record,
        record.corrects_id.expect("test fixture sets corrects_id"),
        &referenced,
    )
    .expect_err("inactive rejected");
    assert!(matches!(
        err,
        UsageCollectorError::Conflict {
            reason: ConflictReason::CorrectsIdInactive,
            ..
        }
    ));
}

#[test]
fn l1_active_in_scope_ordinary_row_passes_referential_check() {
    let record = counter_compensation_record(Decimal::from(-1), Uuid::new_v4());
    let referenced = referenced_ordinary_row(record.tenant_id);
    assert!(
        verify_l1_corrects_id(
            &record,
            record.corrects_id.expect("test fixture sets corrects_id"),
            &referenced
        )
        .is_ok()
    );
}
