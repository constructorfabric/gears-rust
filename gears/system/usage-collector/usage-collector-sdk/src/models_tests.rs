//! Structural unit tests for the foundation SDK models.
//!
//! Scope is intentionally narrow: only the structural invariants that the SDK
//! itself enforces — `gts_id` must derive from
//! `gts.cf.core.uc.usage_record.v1~`, [`UsageKind`] is a closed serde enum
//! plus a hand-rolled `FromStr`, [`UsageTypeGtsId`] routes its `Deserialize`
//! through the same constructor used elsewhere, and the wire round-trip for
//! [`UsageType`] / [`UsageRecord`] carries the documented field shape.

use std::str::FromStr;

use rust_decimal::Decimal;
use serde_json::json;
use uuid::Uuid;

use std::collections::{BTreeMap, BTreeSet};

use super::{
    AggregationBucket, AggregationDimension, AggregationOp, AggregationResult, AggregationSpec,
    IdempotencyKey, MetadataFilter, MetadataKey, ResourceRef, SubjectRef, TimeWindow, UsageKind,
    UsageRecord, UsageRecordStatus, UsageType, UsageTypeGtsId,
};
use crate::error::UsageCollectorError;

fn metadata_key(value: &str) -> MetadataKey {
    MetadataKey::new(value).expect("test fixture supplies a valid metadata key")
}

fn metadata_keys<const N: usize>(values: [&str; N]) -> BTreeSet<MetadataKey> {
    values.into_iter().map(metadata_key).collect()
}

fn metadata_map<const N: usize>(entries: [(&str, &str); N]) -> BTreeMap<MetadataKey, String> {
    entries
        .into_iter()
        .map(|(k, v)| (metadata_key(k), v.to_owned()))
        .collect()
}

const SAMPLE_USAGE_TYPE_ID: &str =
    "gts.cf.core.uc.usage_record.v1~cf.mini_chat._.tokens_consumed.v1";

fn sample_id() -> UsageTypeGtsId {
    UsageTypeGtsId::new(SAMPLE_USAGE_TYPE_ID).expect("valid usage_record-derived id")
}

fn sample_usage_type() -> UsageType {
    UsageType {
        gts_id: sample_id(),
        kind: UsageKind::Counter,
        metadata_fields: metadata_keys(["region", "tier"]),
    }
}

fn sample_usage_record(subject_ref: Option<SubjectRef>, corrects_id: Option<Uuid>) -> UsageRecord {
    UsageRecord {
        uuid: Uuid::parse_str("11111111-1111-1111-1111-111111111111").expect("record uuid"),
        gts_id: sample_id(),
        tenant_id: Uuid::parse_str("22222222-2222-2222-2222-222222222222").expect("tenant uuid"),
        resource_ref: ResourceRef::new("vm-1", "compute.vm").expect("valid resource ref"),
        subject_ref,
        metadata: metadata_map([("region", "eu"), ("tier", "gold")]),
        value: Decimal::from(42),
        idempotency_key: IdempotencyKey::new("k-1").expect("valid idempotency key"),
        corrects_id,
        status: UsageRecordStatus::Active,
        created_at: time::OffsetDateTime::from_unix_timestamp(0).expect("epoch"),
    }
}

// ---------------------------------------------------------------------------
// UsageTypeGtsId — construction validation
// ---------------------------------------------------------------------------

// UsageTypeGtsId::new enforces derivation from gts.cf.core.uc.usage_record.v1~:
// every accepted id must left-prefix-match the base AND carry at least one
// further `~`-separated segment.
#[test]
fn usage_type_gts_id_accepts_one_level_derivation() {
    let input = "gts.cf.core.uc.usage_record.v1~cf.compute._.vcpu_hours.v1";
    let id = UsageTypeGtsId::new(input).expect("one-level derivation accepted");
    assert_eq!(
        id.as_ref(),
        input,
        "AsRef<str> must preserve the input string"
    );
    assert_eq!(
        id.to_string(),
        input,
        "Display must preserve the input string"
    );
}

#[test]
fn usage_type_gts_id_rejects_unknown_base() {
    let err = UsageTypeGtsId::new("gts.cf.core.metric.v1~z")
        .expect_err("non-usage_record base must be rejected");
    assert!(
        matches!(err, UsageCollectorError::InvalidUsageTypeGtsId { .. }),
        "expected InvalidUsageTypeGtsId, got {err:?}"
    );
}

#[test]
fn usage_type_gts_id_rejects_legacy_counter_base() {
    // The old counter/gauge bases must be rejected explicitly to surface
    // wire-shape drift if any legacy producer still emits them.
    let err = UsageTypeGtsId::new("gts.cf.core.usage.counter.v1~legacy")
        .expect_err("legacy counter base must be rejected");
    assert!(matches!(
        err,
        UsageCollectorError::InvalidUsageTypeGtsId { .. }
    ));
}

#[test]
fn usage_type_gts_id_rejects_empty_id() {
    let err = UsageTypeGtsId::new("").expect_err("empty id must be rejected");
    assert!(matches!(
        err,
        UsageCollectorError::InvalidUsageTypeGtsId { .. }
    ));
}

// `GtsInstanceId::new` is infallible and concatenates schema_id + segment
// without validating that the segment is non-empty. `UsageTypeGtsId::new`
// MUST reject a bare base (no derivation segment after the trailing `~`)
// so callers never get a structurally-invalid id past the SDK boundary.
#[test]
fn usage_type_gts_id_rejects_bare_base() {
    let err = UsageTypeGtsId::new(UsageTypeGtsId::USAGE_RECORD_BASE)
        .expect_err("bare base must be rejected");
    assert!(matches!(
        err,
        UsageCollectorError::InvalidUsageTypeGtsId { .. }
    ));
}

// `UsageTypeGtsId` claims to wrap a GTS *instance* id (no trailing `~`).
// A derivation segment that itself ends with `~` would produce a GTS
// *type* id, breaking that invariant — the old byte-level `strip_prefix`
// path accepted it; the GtsID-routed validator rejects it.
#[test]
fn usage_type_gts_id_rejects_derived_type_id_with_trailing_tilde() {
    let err = UsageTypeGtsId::new("gts.cf.core.uc.usage_record.v1~cf.compute._.vcpu_hours.v1~")
        .expect_err("trailing `~` (a type id, not an instance id) must be rejected");
    assert!(matches!(
        err,
        UsageCollectorError::InvalidUsageTypeGtsId { .. }
    ));
}

// Whitespace in the derivation segment is not a valid GTS character. The
// old `strip_prefix` path treated the segment as opaque text and would
// have accepted this; the GtsID parser rejects it as a malformed segment.
#[test]
fn usage_type_gts_id_rejects_whitespace_in_segment() {
    let err = UsageTypeGtsId::new("gts.cf.core.uc.usage_record.v1~cf.compute _.vcpu_hours.v1")
        .expect_err("whitespace in segment must be rejected");
    assert!(matches!(
        err,
        UsageCollectorError::InvalidUsageTypeGtsId { .. }
    ));
}

// A derivation segment that is not itself a syntactically valid GTS
// segment (missing `v<major>` version suffix, wrong number of dot-separated
// fields, etc.) must surface as a validation error rather than producing
// a malformed `GtsInstanceId`. Covers the family of "non-GTS tail" inputs
// the prior implementation silently let through.
#[test]
fn usage_type_gts_id_rejects_malformed_derivation_segment() {
    let err = UsageTypeGtsId::new("gts.cf.core.uc.usage_record.v1~not_a_gts_segment")
        .expect_err("non-GTS-shaped derivation segment must be rejected");
    assert!(matches!(
        err,
        UsageCollectorError::InvalidUsageTypeGtsId { .. }
    ));
}

// Empty inner segment (consecutive `~`) is invalid per the GTS chained-id
// rules. The old `strip_prefix` would return `Some("~foo")` and the
// non-empty check would let it through; `GtsID::new` flags the empty
// segment between the two tildes.
#[test]
fn usage_type_gts_id_rejects_consecutive_tildes() {
    let err = UsageTypeGtsId::new("gts.cf.core.uc.usage_record.v1~~foo.bar.v1")
        .expect_err("consecutive tildes must be rejected");
    assert!(matches!(
        err,
        UsageCollectorError::InvalidUsageTypeGtsId { .. }
    ));
}

// Catalog admits *direct* derivation only: a deeper chain like
// `base~mid.v1~tail.v1` has `get_type_id() == Some("base~mid.v1~")`, which
// is not the bare `USAGE_RECORD_BASE`, so the parent-chain match at
// `models.rs:`-the-`get_type_id`-equality-site must reject it. Pins this
// contract against a future GTS parser change.
#[test]
fn usage_type_gts_id_rejects_deep_derivation_chain() {
    let err = UsageTypeGtsId::new(
        "gts.cf.core.uc.usage_record.v1~cf.compute._.vcpu_hours.v1~cf.compute._.tail.v1",
    )
    .expect_err("deep-derivation chain must be rejected - only direct base derivation is admitted");
    assert!(matches!(
        err,
        UsageCollectorError::InvalidUsageTypeGtsId { .. }
    ));
}

// ---------------------------------------------------------------------------
// UsageTypeGtsId — custom Deserialize routes validation through serde
// ---------------------------------------------------------------------------

#[test]
fn usage_type_gts_id_deserialize_round_trips_valid_string() {
    let decoded: UsageTypeGtsId =
        serde_json::from_value(json!(SAMPLE_USAGE_TYPE_ID)).expect("valid gts_id deserializes");
    assert_eq!(decoded.as_ref(), SAMPLE_USAGE_TYPE_ID);
}

#[test]
fn usage_type_gts_id_deserialize_surfaces_validation_as_serde_error() {
    let err = serde_json::from_value::<UsageTypeGtsId>(json!("gts.cf.core.metric.v1~oops"))
        .expect_err("malformed gts_id must surface as a serde error");
    assert!(
        err.to_string().contains("usage type gts_id"),
        "serde error must carry the Validation detail; got {err}"
    );
}

// ---------------------------------------------------------------------------
// UsageKind — serde + FromStr
// ---------------------------------------------------------------------------

#[test]
fn usage_kind_serde_round_trips_lowercase() {
    let counter_json = serde_json::to_string(&UsageKind::Counter).expect("serialize counter");
    let gauge_json = serde_json::to_string(&UsageKind::Gauge).expect("serialize gauge");
    assert_eq!(counter_json, "\"counter\"");
    assert_eq!(gauge_json, "\"gauge\"");
    let decoded_c: UsageKind = serde_json::from_str("\"counter\"").expect("decode counter");
    let decoded_g: UsageKind = serde_json::from_str("\"gauge\"").expect("decode gauge");
    assert_eq!(decoded_c, UsageKind::Counter);
    assert_eq!(decoded_g, UsageKind::Gauge);
}

#[test]
fn usage_kind_rejects_unknown_variant_at_deserialize_boundary() {
    let err = serde_json::from_str::<UsageKind>("\"histogram\"")
        .expect_err("unknown variant must be rejected at the serde boundary");
    assert!(err.to_string().contains("unknown variant"));
}

#[test]
fn usage_kind_from_str_accepts_counter_and_gauge() {
    assert_eq!(
        UsageKind::from_str("counter").expect("counter"),
        UsageKind::Counter
    );
    assert_eq!(
        UsageKind::from_str("gauge").expect("gauge"),
        UsageKind::Gauge
    );
}

#[test]
fn usage_kind_from_str_rejects_unknown_variant_as_validation_error() {
    let err =
        UsageKind::from_str("histogram").expect_err("unknown kind must be rejected by FromStr");
    assert!(
        matches!(err, UsageCollectorError::InvalidUsageKind { ref raw } if raw == "histogram"),
        "expected InvalidUsageKind, got {err:?}"
    );
}

// ---------------------------------------------------------------------------
// UsageType — wire shape
// ---------------------------------------------------------------------------

#[test]
fn usage_type_serde_round_trip_carries_kind_and_metadata_fields() {
    let usage_type = sample_usage_type();
    let json_str = serde_json::to_string(&usage_type).expect("serialize UsageType");
    let decoded: UsageType = serde_json::from_str(&json_str).expect("deserialize UsageType");
    assert_eq!(usage_type, decoded);
    assert!(
        json_str.contains("\"gts_id\""),
        "wire shape must carry 'gts_id'; got {json_str}"
    );
    assert!(
        json_str.contains("\"kind\""),
        "wire shape must carry 'kind'; got {json_str}"
    );
    assert!(
        json_str.contains("\"counter\""),
        "kind must serialize as 'counter'; got {json_str}"
    );
    assert!(
        json_str.contains("\"metadata_fields\""),
        "wire shape must carry 'metadata_fields'; got {json_str}"
    );
    assert!(
        !json_str.contains("\"created_at\""),
        "wire shape must NOT carry 'created_at'; got {json_str}"
    );
}

#[test]
fn usage_type_rejects_unknown_fields() {
    let payload = json!({
        "gts_id": "gts.cf.core.uc.usage_record.v1~cf.compute._.vcpu_hours.v1",
        "kind": "counter",
        "metadata_fields": [],
        "legacy_schema_field": {"type": "object"},
    });
    let err =
        serde_json::from_value::<UsageType>(payload).expect_err("unknown field must be rejected");
    assert!(err.to_string().contains("unknown field"));
}

#[test]
fn usage_type_rejects_unknown_kind_at_deserialize_boundary() {
    let payload = json!({
        "gts_id": "gts.cf.core.uc.usage_record.v1~cf.compute._.vcpu_hours.v1",
        "kind": "histogram",
        "metadata_fields": [],
    });
    let err =
        serde_json::from_value::<UsageType>(payload).expect_err("unknown kind must be rejected");
    assert!(err.to_string().contains("unknown variant"));
}

// ---------------------------------------------------------------------------
// UsageType — kind-classification predicates
// ---------------------------------------------------------------------------

#[test]
fn usage_type_kind_classifier_predicates_match_kind_per_variant() {
    use UsageKind as K;

    // Compile-time exhaustiveness fence: adding a third UsageKind variant
    // forces a new arm here and signals the developer to also update
    // `UsageType::is_counter` / `is_gauge` and the table below.
    const _FENCE: fn(&UsageKind) = |k| match k {
        K::Counter | K::Gauge => (),
    };

    let cases: &[(UsageKind, bool, bool)] = &[
        // (kind, expected_is_counter, expected_is_gauge)
        (K::Counter, true, false),
        (K::Gauge, false, true),
    ];

    for (kind, expected_counter, expected_gauge) in cases {
        let usage_type = UsageType {
            gts_id: sample_id(),
            kind: *kind,
            metadata_fields: BTreeSet::new(),
        };
        assert_eq!(
            usage_type.is_counter(),
            *expected_counter,
            "is_counter mismatch for {kind:?}",
        );
        assert_eq!(
            usage_type.is_gauge(),
            *expected_gauge,
            "is_gauge mismatch for {kind:?}",
        );
    }
}

// ---------------------------------------------------------------------------
// UsageRecord — wire shape (RFC-3339 `created_at`, optional skipping,
// `status` defaulting)
// ---------------------------------------------------------------------------

#[test]
fn usage_record_serde_round_trip_omits_none_optionals_and_uses_rfc3339_created_at() {
    let record = sample_usage_record(None, None);
    let value = serde_json::to_value(&record).expect("serialize UsageRecord");
    let object = value
        .as_object()
        .expect("UsageRecord serializes as a JSON object");
    assert!(
        !object.contains_key("subject_ref"),
        "subject_ref must be omitted when None; got {object:?}"
    );
    assert!(
        !object.contains_key("corrects_id"),
        "corrects_id must be omitted when None; got {object:?}"
    );
    assert_eq!(
        object.get("created_at").and_then(|v| v.as_str()),
        Some("1970-01-01T00:00:00Z"),
        "created_at must serialize in RFC-3339 form with `Z` UTC marker; got {object:?}"
    );
    assert_eq!(
        object.get("status").and_then(|v| v.as_str()),
        Some("active"),
        "status must serialize as lowercase; got {object:?}"
    );
    let round_tripped: UsageRecord = serde_json::from_value(value).expect("UsageRecord round-trip");
    assert_eq!(record, round_tripped);
}

#[test]
fn usage_record_serde_round_trip_carries_subject_ref_and_corrects_id_when_some() {
    let subject = SubjectRef::new("principal-1", Some("user")).expect("valid subject ref");
    let correction =
        Uuid::parse_str("33333333-3333-3333-3333-333333333333").expect("correction uuid");
    let record = sample_usage_record(Some(subject), Some(correction));
    let value = serde_json::to_value(&record).expect("serialize UsageRecord");
    let object = value
        .as_object()
        .expect("UsageRecord serializes as a JSON object");
    assert!(
        object.contains_key("subject_ref"),
        "subject_ref must be present when Some; got {object:?}"
    );
    assert_eq!(
        object.get("corrects_id").and_then(|v| v.as_str()),
        Some("33333333-3333-3333-3333-333333333333"),
        "corrects_id must serialize as a UUID string; got {object:?}"
    );
    let round_tripped: UsageRecord = serde_json::from_value(value).expect("UsageRecord round-trip");
    assert_eq!(record, round_tripped);
}

// ---------------------------------------------------------------------------
// MetadataFilter — validated construction and wire shape
// ---------------------------------------------------------------------------

#[test]
fn metadata_filter_new_accepts_non_empty_key_and_values() {
    let f =
        MetadataFilter::new("region", ["us-east-1", "eu-west-1"]).expect("valid metadata filter");
    assert_eq!(f.key().as_str(), "region");
    assert_eq!(
        f.values(),
        &["us-east-1".to_owned(), "eu-west-1".to_owned()]
    );
}

#[test]
fn metadata_filter_new_rejects_empty_key() {
    let err = MetadataFilter::new("", ["v"]).expect_err("empty key must be rejected");
    assert!(
        matches!(err, UsageCollectorError::InvalidMetadataFilter { .. }),
        "expected InvalidMetadataFilter, got {err:?}"
    );
}

#[test]
fn metadata_filter_new_rejects_nul_byte_in_key() {
    let err = MetadataFilter::new("region\0", ["v"])
        .expect_err("NUL bytes in key must be rejected for jsonb compatibility");
    assert!(matches!(
        err,
        UsageCollectorError::InvalidMetadataFilter { .. }
    ));
}

#[test]
fn metadata_filter_new_rejects_empty_values() {
    let err = MetadataFilter::new("region", Vec::<&str>::new())
        .expect_err("empty values must be rejected");
    assert!(matches!(
        err,
        UsageCollectorError::InvalidMetadataFilter { .. }
    ));
}

#[test]
fn metadata_filter_serde_round_trips_wire_shape() {
    let f = MetadataFilter::new("region", ["us-east-1"]).expect("valid metadata filter");
    let value = serde_json::to_value(&f).expect("serialize MetadataFilter");
    assert_eq!(
        value,
        json!({"key": "region", "values": ["us-east-1"]}),
        "wire shape must be {{key, values}}; got {value}"
    );
    let decoded: MetadataFilter = serde_json::from_value(value).expect("round-trip");
    assert_eq!(decoded, f);
}

#[test]
fn metadata_filter_deserialize_routes_through_new_and_surfaces_validation() {
    let err = serde_json::from_value::<MetadataFilter>(json!({"key": "", "values": ["v"]}))
        .expect_err("empty key must surface as a serde error");
    assert!(
        err.to_string().contains("metadata key must not be empty"),
        "serde error must carry the MetadataKey validation detail; got {err}"
    );

    let err = serde_json::from_value::<MetadataFilter>(json!({"key": "region", "values": []}))
        .expect_err("empty values must surface as a serde error");
    assert!(
        err.to_string().contains("must carry at least one value"),
        "serde error must carry the Validation detail; got {err}"
    );
}

#[test]
fn metadata_filter_deserialize_rejects_unknown_fields() {
    let err = serde_json::from_value::<MetadataFilter>(
        json!({"key": "region", "values": ["v"], "op": "eq"}),
    )
    .expect_err("unknown field must be rejected at the wire boundary");
    assert!(err.to_string().contains("unknown field"));
}

// ---------------------------------------------------------------------------
// TimeWindow — validated construction + wire shape
// ---------------------------------------------------------------------------

fn t(seconds: i64) -> time::OffsetDateTime {
    time::OffsetDateTime::from_unix_timestamp(seconds).expect("valid unix timestamp")
}

#[test]
fn time_window_new_accepts_strictly_ordered_bounds() {
    let w = TimeWindow::new(t(0), t(1)).expect("from < to");
    assert_eq!(w.from(), t(0));
    assert_eq!(w.to(), t(1));
}

#[test]
fn time_window_new_rejects_equal_bounds() {
    let err = TimeWindow::new(t(0), t(0)).expect_err("from == to must be rejected");
    assert!(matches!(err, UsageCollectorError::InvalidTimeRange { .. }));
}

#[test]
fn time_window_new_rejects_inverted_bounds() {
    let err = TimeWindow::new(t(1), t(0)).expect_err("from > to must be rejected");
    assert!(matches!(err, UsageCollectorError::InvalidTimeRange { .. }));
}

#[test]
fn time_window_serde_round_trips_rfc3339_bounds() {
    let w = TimeWindow::new(t(0), t(3600)).expect("valid window");
    let value = serde_json::to_value(w).expect("serialize TimeWindow");
    assert_eq!(
        value,
        json!({"from": "1970-01-01T00:00:00Z", "to": "1970-01-01T01:00:00Z"}),
        "wire shape must be {{from, to}} in RFC-3339 UTC; got {value}"
    );
    let decoded: TimeWindow = serde_json::from_value(value).expect("round-trip");
    assert_eq!(decoded, w);
}

#[test]
fn time_window_deserialize_routes_through_new() {
    let err = serde_json::from_value::<TimeWindow>(
        json!({"from": "1970-01-01T00:00:01Z", "to": "1970-01-01T00:00:00Z"}),
    )
    .expect_err("inverted bounds must surface as a serde error");
    assert!(
        err.to_string().contains("must be strictly less than"),
        "serde error must carry the Validation detail; got {err}"
    );
}

#[test]
fn time_window_deserialize_rejects_unknown_fields() {
    let err = serde_json::from_value::<TimeWindow>(json!({
        "from": "1970-01-01T00:00:00Z",
        "to": "1970-01-01T00:00:01Z",
        "tz": "UTC",
    }))
    .expect_err("unknown field must be rejected at the wire boundary");
    assert!(err.to_string().contains("unknown field"));
}

// ---------------------------------------------------------------------------
// AggregationOp / AggregationDimension / AggregationSpec — wire shapes
// ---------------------------------------------------------------------------

#[test]
fn aggregation_op_serializes_as_lowercase_strings() {
    for (op, expected) in [
        (AggregationOp::Sum, "\"sum\""),
        (AggregationOp::Count, "\"count\""),
        (AggregationOp::Min, "\"min\""),
        (AggregationOp::Max, "\"max\""),
        (AggregationOp::Avg, "\"avg\""),
    ] {
        let s = serde_json::to_string(&op).expect("serialize AggregationOp");
        assert_eq!(
            s, expected,
            "AggregationOp::{op:?} must serialize as {expected}"
        );
        let decoded: AggregationOp = serde_json::from_str(&s).expect("round-trip");
        assert_eq!(decoded, op);
    }
}

#[test]
fn aggregation_dimension_serializes_unit_variants_as_snake_case_strings() {
    let value = serde_json::to_value(AggregationDimension::TenantId).expect("serialize");
    assert_eq!(value, json!("tenant_id"));
    let decoded: AggregationDimension = serde_json::from_value(value).expect("round-trip");
    assert_eq!(decoded, AggregationDimension::TenantId);
}

#[test]
fn aggregation_dimension_serializes_metadata_variant_as_tagged_object() {
    let value = serde_json::to_value(AggregationDimension::Metadata(metadata_key("region")))
        .expect("serialize Metadata variant");
    assert_eq!(
        value,
        json!({"metadata": "region"}),
        "Metadata variant must serialize as a tagged object; got {value}"
    );
    let decoded: AggregationDimension = serde_json::from_value(value).expect("round-trip");
    assert_eq!(
        decoded,
        AggregationDimension::Metadata(metadata_key("region"))
    );
}

#[test]
fn aggregation_spec_omits_empty_group_by_on_the_wire() {
    let spec = AggregationSpec {
        op: AggregationOp::Sum,
        group_by: Vec::new(),
    };
    let value = serde_json::to_value(&spec).expect("serialize AggregationSpec");
    assert_eq!(
        value,
        json!({"op": "sum"}),
        "empty group_by must be skipped on the wire; got {value}"
    );
    let decoded: AggregationSpec = serde_json::from_value(value).expect("round-trip");
    assert_eq!(decoded, spec);
}

#[test]
fn aggregation_spec_carries_group_by_in_caller_order() {
    let spec = AggregationSpec {
        op: AggregationOp::Avg,
        group_by: vec![
            AggregationDimension::ResourceType,
            AggregationDimension::Metadata(metadata_key("region")),
        ],
    };
    let value = serde_json::to_value(&spec).expect("serialize");
    assert_eq!(
        value,
        json!({
            "op": "avg",
            "group_by": ["resource_type", {"metadata": "region"}],
        }),
    );
}

// ---------------------------------------------------------------------------
// AggregationResult / AggregationBucket — wire shape
// ---------------------------------------------------------------------------

#[test]
fn aggregation_result_carries_empty_key_for_no_grouping() {
    let result = AggregationResult {
        buckets: vec![AggregationBucket {
            key: Vec::new(),
            value: Some(Decimal::from(42)),
        }],
    };
    let value = serde_json::to_value(&result).expect("serialize");
    assert_eq!(
        value,
        json!({"buckets": [{"value": "42"}]}),
        "empty key must be skipped on the wire; got {value}"
    );
    let decoded: AggregationResult = serde_json::from_value(value).expect("round-trip");
    assert_eq!(decoded, result);
}

#[test]
fn aggregation_bucket_key_is_vec_of_strings() {
    // One UUID-shaped string, one plain string. Both must round-trip
    // verbatim as raw JSON strings — no envelope, no discriminator.
    let bucket = AggregationBucket {
        key: vec![
            "00000000-0000-0000-0000-000000000001".to_owned(),
            "us-east-1".to_owned(),
        ],
        value: Some(Decimal::from(7)),
    };
    let value = serde_json::to_value(&bucket).expect("serialize");
    assert_eq!(
        value,
        json!({
            "key": ["00000000-0000-0000-0000-000000000001", "us-east-1"],
            "value": "7",
        }),
        "AggregationBucket.key must serialize as a JSON array of raw strings; got {value}"
    );
    let decoded: AggregationBucket = serde_json::from_value(value).expect("round-trip");
    assert_eq!(decoded, bucket);
}

#[test]
fn aggregation_bucket_tenant_id_uses_canonical_uuid_string() {
    // Pins the per-spec encoding rule for the TenantId dimension: plugins
    // MUST emit the tenant UUID via `Uuid::to_string()` (lowercase,
    // hyphenated). The bucket itself stores a plain String — this test
    // guards that the canonical form is what producers send.
    let tenant = Uuid::parse_str("0123456789ABCDEF0123456789ABCDEF").expect("tenant uuid");
    let bucket = AggregationBucket {
        key: vec![tenant.to_string()],
        value: Some(Decimal::from(1)),
    };
    let value = serde_json::to_value(&bucket).expect("serialize");
    assert_eq!(
        value,
        json!({
            "key": ["01234567-89ab-cdef-0123-456789abcdef"],
            "value": "1",
        }),
        "TenantId dimension must serialize as lowercase hyphenated UUID; got {value}"
    );
}

#[test]
fn aggregation_bucket_carries_none_value_for_empty_aggregation() {
    let bucket = AggregationBucket {
        key: Vec::new(),
        value: None,
    };
    let value = serde_json::to_value(&bucket).expect("serialize");
    assert_eq!(value, json!({"value": null}));
    let decoded: AggregationBucket = serde_json::from_value(value).expect("round-trip");
    assert_eq!(decoded, bucket);
}

#[test]
fn usage_record_deserialize_defaults_status_to_active_when_missing() {
    let mut value =
        serde_json::to_value(sample_usage_record(None, None)).expect("serialize seed UsageRecord");
    value.as_object_mut().expect("object").remove("status");
    let decoded: UsageRecord = serde_json::from_value(value)
        .expect("UsageRecord without status field deserializes via #[serde(default)]");
    assert_eq!(decoded.status, UsageRecordStatus::Active);
}

// ---------------------------------------------------------------------------
// MetadataKey — validated construction and serde routing
// ---------------------------------------------------------------------------

#[test]
fn metadata_key_new_accepts_well_formed_string() {
    let key = MetadataKey::new("region").expect("valid key");
    assert_eq!(key.as_str(), "region");
    assert_eq!(key.to_string(), "region");
}

#[test]
fn metadata_key_new_rejects_empty_string() {
    let err = MetadataKey::new("").expect_err("empty key must be rejected");
    assert!(matches!(
        err,
        UsageCollectorError::InvalidMetadataKey { .. }
    ));
}

#[test]
fn metadata_key_new_rejects_nul_byte() {
    let err = MetadataKey::new("bad\0key").expect_err("NUL byte must be rejected");
    assert!(matches!(
        err,
        UsageCollectorError::InvalidMetadataKey { .. }
    ));
}

#[test]
fn metadata_key_deserialize_routes_through_new() {
    let err = serde_json::from_value::<MetadataKey>(json!(""))
        .expect_err("empty key must surface as a serde error");
    assert!(err.to_string().contains("metadata key must not be empty"));
}

#[test]
fn metadata_key_serializes_transparently() {
    let key = MetadataKey::new("region").expect("valid key");
    let value = serde_json::to_value(&key).expect("serialize");
    assert_eq!(value, json!("region"));
}

// ---------------------------------------------------------------------------
// UsageType.metadata_fields wire shape (BTreeSet<MetadataKey>)
// ---------------------------------------------------------------------------

#[test]
fn usage_type_metadata_fields_deserialize_rejects_empty_member_key() {
    let payload = json!({
        "gts_id": SAMPLE_USAGE_TYPE_ID,
        "kind": "counter",
        "metadata_fields": [""],
    });
    let err = serde_json::from_value::<UsageType>(payload)
        .expect_err("empty member key must be rejected by MetadataKey::deserialize");
    assert!(err.to_string().contains("metadata key must not be empty"));
}

#[test]
fn usage_type_metadata_fields_deserialize_rejects_duplicate_member_keys() {
    // The custom `deserialize_metadata_fields` routes the JSON array through
    // `Vec<MetadataKey>` so duplicate keys are rejected at the SDK wire
    // boundary instead of silently collapsing into the `BTreeSet`. The error
    // message carries the offending zero-based index. The REST DTO path
    // additionally surfaces the typed `UsageCollectorError::DuplicateMetadataField`
    // via `metadata_fields_from_wire`.
    let payload = json!({
        "gts_id": SAMPLE_USAGE_TYPE_ID,
        "kind": "counter",
        "metadata_fields": ["region", "tier", "region"],
    });
    let err = serde_json::from_value::<UsageType>(payload)
        .expect_err("duplicate metadata field must be rejected at deserialize");
    let msg = err.to_string();
    assert!(
        msg.contains("duplicate metadata field") && msg.contains("index 2"),
        "expected duplicate-at-index-2 message, got {msg}"
    );
}

// ---------------------------------------------------------------------------
// UsageRecord.metadata wire shape (BTreeMap<MetadataKey, String>)
// ---------------------------------------------------------------------------

#[test]
fn usage_record_metadata_serializes_as_string_to_string_map() {
    let record = sample_usage_record(None, None);
    let value = serde_json::to_value(&record).expect("serialize UsageRecord");
    let metadata = value
        .get("metadata")
        .expect("metadata present when non-empty");
    assert_eq!(metadata, &json!({"region": "eu", "tier": "gold"}));
}

#[test]
fn usage_record_metadata_omitted_from_wire_when_empty() {
    let mut record = sample_usage_record(None, None);
    record.metadata = BTreeMap::new();
    let value = serde_json::to_value(&record).expect("serialize UsageRecord");
    assert!(
        value.get("metadata").is_none(),
        "empty metadata map must be skipped on the wire; got {value}"
    );
}

#[test]
fn usage_record_metadata_deserialize_rejects_non_string_value() {
    let mut value =
        serde_json::to_value(sample_usage_record(None, None)).expect("serialize seed UsageRecord");
    value
        .as_object_mut()
        .expect("object")
        .insert("metadata".to_owned(), json!({"region": 42}));
    let err = serde_json::from_value::<UsageRecord>(value)
        .expect_err("non-string metadata value must be rejected at the type boundary");
    assert!(err.to_string().contains("invalid type"));
}

#[test]
fn usage_record_metadata_deserialize_defaults_to_empty_when_missing() {
    let mut value =
        serde_json::to_value(sample_usage_record(None, None)).expect("serialize seed UsageRecord");
    value.as_object_mut().expect("object").remove("metadata");
    let decoded: UsageRecord = serde_json::from_value(value)
        .expect("UsageRecord without metadata field deserializes via #[serde(default)]");
    assert!(decoded.metadata.is_empty());
}

#[test]
fn usage_record_rejects_unknown_fields() {
    let mut value =
        serde_json::to_value(sample_usage_record(None, None)).expect("serialize seed UsageRecord");
    value
        .as_object_mut()
        .expect("object")
        .insert("legacy_schema_field".to_owned(), json!({"type": "object"}));
    let err = serde_json::from_value::<UsageRecord>(value)
        .expect_err("unknown field must be rejected at the wire boundary");
    assert!(err.to_string().contains("unknown field"));
}

// ---------------------------------------------------------------------------
// IdempotencyKey — validated construction + serde routing
// ---------------------------------------------------------------------------

#[test]
fn idempotency_key_new_accepts_non_empty_string() {
    let k = IdempotencyKey::new("idem-1").expect("valid key");
    assert_eq!(k.as_str(), "idem-1");
    assert_eq!(k.to_string(), "idem-1");
}

#[test]
fn idempotency_key_new_rejects_empty_string() {
    let err = IdempotencyKey::new("").expect_err("empty key must be rejected");
    assert!(
        matches!(err, UsageCollectorError::InvalidIdempotencyKey { .. }),
        "expected InvalidIdempotencyKey, got {err:?}"
    );
}

#[test]
fn idempotency_key_new_rejects_nul_byte() {
    let err = IdempotencyKey::new("bad\0key").expect_err("NUL byte must be rejected");
    assert!(matches!(
        err,
        UsageCollectorError::InvalidIdempotencyKey { .. }
    ));
}

#[test]
fn idempotency_key_serializes_transparently() {
    let k = IdempotencyKey::new("idem-1").expect("valid key");
    let value = serde_json::to_value(&k).expect("serialize");
    assert_eq!(value, json!("idem-1"));
}

#[test]
fn idempotency_key_deserialize_routes_through_new() {
    let err = serde_json::from_value::<IdempotencyKey>(json!(""))
        .expect_err("empty key must surface as a serde error");
    assert!(
        err.to_string()
            .contains("idempotency_key must not be empty"),
        "serde error must carry the Validation detail; got {err}"
    );
}

#[test]
fn idempotency_key_from_str_routes_through_new() {
    assert!(IdempotencyKey::from_str("idem-1").is_ok());
    let err = IdempotencyKey::from_str("").expect_err("empty key must be rejected");
    assert!(matches!(
        err,
        UsageCollectorError::InvalidIdempotencyKey { .. }
    ));
}

// ---------------------------------------------------------------------------
// ResourceRef — validated construction + serde routing
// ---------------------------------------------------------------------------

#[test]
fn resource_ref_new_accepts_non_empty_components() {
    let r = ResourceRef::new("vm-1", "compute.vm").expect("valid ref");
    assert_eq!(r.resource_id(), "vm-1");
    assert_eq!(r.resource_type(), "compute.vm");
}

#[test]
fn resource_ref_new_rejects_empty_resource_id() {
    let err = ResourceRef::new("", "compute.vm").expect_err("empty resource_id must be rejected");
    assert!(
        matches!(err, UsageCollectorError::InvalidResourceRef { .. }),
        "expected InvalidResourceRef, got {err:?}"
    );
}

#[test]
fn resource_ref_new_rejects_empty_resource_type() {
    let err = ResourceRef::new("vm-1", "").expect_err("empty resource_type must be rejected");
    assert!(matches!(
        err,
        UsageCollectorError::InvalidResourceRef { .. }
    ));
}

#[test]
fn resource_ref_new_rejects_nul_byte_in_resource_id() {
    let err = ResourceRef::new("vm\0bad", "compute.vm")
        .expect_err("NUL byte in resource_id must be rejected");
    assert!(matches!(
        err,
        UsageCollectorError::InvalidResourceRef { .. }
    ));
}

#[test]
fn resource_ref_new_rejects_nul_byte_in_resource_type() {
    let err = ResourceRef::new("vm-1", "compute\0vm")
        .expect_err("NUL byte in resource_type must be rejected");
    assert!(matches!(
        err,
        UsageCollectorError::InvalidResourceRef { .. }
    ));
}

#[test]
fn resource_ref_serde_round_trips_wire_shape() {
    let r = ResourceRef::new("vm-1", "compute.vm").expect("valid ref");
    let value = serde_json::to_value(&r).expect("serialize");
    assert_eq!(
        value,
        json!({"resource_id": "vm-1", "resource_type": "compute.vm"}),
        "wire shape must be {{resource_id, resource_type}}; got {value}"
    );
    let decoded: ResourceRef = serde_json::from_value(value).expect("round-trip");
    assert_eq!(decoded, r);
}

#[test]
fn resource_ref_deserialize_routes_through_new() {
    let err = serde_json::from_value::<ResourceRef>(json!({
        "resource_id": "",
        "resource_type": "compute.vm",
    }))
    .expect_err("empty resource_id must surface as a serde error");
    assert!(
        err.to_string().contains("resource_id must not be empty"),
        "serde error must carry the Validation detail; got {err}"
    );

    let err = serde_json::from_value::<ResourceRef>(json!({
        "resource_id": "vm-1",
        "resource_type": "",
    }))
    .expect_err("empty resource_type must surface as a serde error");
    assert!(err.to_string().contains("resource_type must not be empty"));
}

#[test]
fn resource_ref_deserialize_rejects_unknown_fields() {
    let err = serde_json::from_value::<ResourceRef>(json!({
        "resource_id": "vm-1",
        "resource_type": "compute.vm",
        "tenant_scope": "main",
    }))
    .expect_err("unknown field must be rejected at the wire boundary");
    assert!(err.to_string().contains("unknown field"));
}

// ---------------------------------------------------------------------------
// SubjectRef — validated construction + serde routing
// ---------------------------------------------------------------------------

#[test]
fn subject_ref_new_accepts_subject_id_only() {
    let s = SubjectRef::new("principal-1", Option::<&str>::None).expect("valid ref");
    assert_eq!(s.subject_id(), "principal-1");
    assert!(s.subject_type().is_none());
}

#[test]
fn subject_ref_new_accepts_subject_id_and_subject_type() {
    let s = SubjectRef::new("principal-1", Some("user")).expect("valid ref");
    assert_eq!(s.subject_id(), "principal-1");
    assert_eq!(s.subject_type(), Some("user"));
}

#[test]
fn subject_ref_new_rejects_empty_subject_id() {
    let err = SubjectRef::new("", Some("user")).expect_err("empty subject_id must be rejected");
    assert!(matches!(err, UsageCollectorError::InvalidSubjectRef { .. }));
}

#[test]
fn subject_ref_new_rejects_explicit_empty_subject_type() {
    let err = SubjectRef::new("principal-1", Some(""))
        .expect_err("Some(\"\") subject_type must be rejected");
    assert!(matches!(err, UsageCollectorError::InvalidSubjectRef { .. }));
}

#[test]
fn subject_ref_new_rejects_nul_byte_in_subject_id() {
    let err = SubjectRef::new("principal\0bad", Some("user"))
        .expect_err("NUL byte in subject_id must be rejected");
    assert!(matches!(err, UsageCollectorError::InvalidSubjectRef { .. }));
}

#[test]
fn subject_ref_new_rejects_nul_byte_in_subject_type() {
    let err = SubjectRef::new("principal-1", Some("user\0bad"))
        .expect_err("NUL byte in subject_type must be rejected");
    assert!(matches!(err, UsageCollectorError::InvalidSubjectRef { .. }));
}

#[test]
fn subject_ref_serde_round_trips_omitting_none_subject_type() {
    let s = SubjectRef::new("principal-1", Option::<&str>::None).expect("valid ref");
    let value = serde_json::to_value(&s).expect("serialize");
    assert_eq!(
        value,
        json!({"subject_id": "principal-1"}),
        "subject_type must be omitted when None; got {value}"
    );
    let decoded: SubjectRef = serde_json::from_value(value).expect("round-trip");
    assert_eq!(decoded, s);
}

#[test]
fn subject_ref_serde_round_trips_carrying_subject_type() {
    let s = SubjectRef::new("principal-1", Some("user")).expect("valid ref");
    let value = serde_json::to_value(&s).expect("serialize");
    assert_eq!(
        value,
        json!({"subject_id": "principal-1", "subject_type": "user"}),
    );
    let decoded: SubjectRef = serde_json::from_value(value).expect("round-trip");
    assert_eq!(decoded, s);
}

#[test]
fn subject_ref_deserialize_routes_through_new() {
    let err = serde_json::from_value::<SubjectRef>(json!({
        "subject_id": "",
        "subject_type": "user",
    }))
    .expect_err("empty subject_id must surface as a serde error");
    assert!(err.to_string().contains("subject_id must not be empty"));

    let err = serde_json::from_value::<SubjectRef>(json!({
        "subject_id": "principal-1",
        "subject_type": "",
    }))
    .expect_err("empty subject_type must surface as a serde error");
    assert!(
        err.to_string()
            .contains("subject_type must not be empty when supplied")
    );
}

#[test]
fn subject_ref_deserialize_rejects_unknown_fields() {
    let err = serde_json::from_value::<SubjectRef>(json!({
        "subject_id": "principal-1",
        "subject_type": "user",
        "tenant_id": "00000000-0000-0000-0000-000000000000",
    }))
    .expect_err("unknown field must be rejected at the wire boundary");
    assert!(err.to_string().contains("unknown field"));
}

// ---------------------------------------------------------------------------
// UsageRecordQuery — OData filter surface
// ---------------------------------------------------------------------------
//
// `gts_id` is carried as a typed parameter on `list_usage_records` /
// `query_aggregated_usage_records`. The OData filter surface declared by
// `UsageRecordQuery` deliberately omits it so that
// `parse_odata_filter::<UsageRecordFilterField>` rejects any
// `gts_id`-touching predicate at parse time — implementations and the
// gateway do not need a runtime reject path.

#[test]
fn usage_record_query_filter_surface_rejects_gts_id_eq() {
    let err = toolkit_odata::filter::parse_odata_filter::<crate::models::UsageRecordFilterField>(
        "gts_id eq 'gts.cf.core.uc.usage_record.v1~cf.compute._.vcpu_hours.v1'",
    )
    .expect_err("gts_id must not be exposed on the OData filter surface");
    assert!(
        matches!(
            &err,
            toolkit_odata::filter::FilterError::UnknownField(name) if name == "gts_id"
        ),
        "expected UnknownField(\"gts_id\"), got {err:?}",
    );
}

#[test]
fn usage_record_query_filter_surface_rejects_gts_id_in_list() {
    let err = toolkit_odata::filter::parse_odata_filter::<crate::models::UsageRecordFilterField>(
        "gts_id in ('a', 'b')",
    )
    .expect_err("gts_id must not be exposed on the OData filter surface");
    assert!(
        matches!(
            &err,
            toolkit_odata::filter::FilterError::UnknownField(name) if name == "gts_id"
        ),
        "expected UnknownField(\"gts_id\"), got {err:?}",
    );
}

#[test]
fn usage_record_query_filter_surface_rejects_gts_id_inside_composite() {
    let err = toolkit_odata::filter::parse_odata_filter::<crate::models::UsageRecordFilterField>(
        "tenant_id eq 22222222-2222-2222-2222-222222222222 and gts_id eq 'x'",
    )
    .expect_err("gts_id-touching predicates must be rejected at parse time");
    assert!(
        matches!(
            &err,
            toolkit_odata::filter::FilterError::UnknownField(name) if name == "gts_id"
        ),
        "expected UnknownField(\"gts_id\"), got {err:?}",
    );
}
