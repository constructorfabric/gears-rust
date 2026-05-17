use chrono::{TimeZone, Utc};
use modkit_odata::{CursorV1, Page, PageInfo, SortDir};
use modkit_security::AccessScope;
use serde_json::json;
use uuid::Uuid;

use super::{
    AggregationFn, AggregationResult, AllowedMetric, BucketSize, GroupByDimension, ModuleConfig,
    RawQuery, Subject, UsageKind, UsageRecord,
};

fn make_record() -> UsageRecord {
    UsageRecord {
        module: "test-module".to_owned(),
        tenant_id: Uuid::nil(),
        metric: "test.metric".to_owned(),
        kind: UsageKind::Gauge,
        value: 1.0,
        resource_id: Uuid::nil(),
        resource_type: "test.resource".to_owned(),
        subject: Some(Subject::with_type(Uuid::nil(), "test.subject")),
        idempotency_key: "00000000-0000-0000-0000-000000000001".to_owned(),
        timestamp: Utc.with_ymd_and_hms(2026, 1, 2, 3, 4, 5).unwrap(),
        metadata: None,
    }
}

#[test]
fn usage_record_roundtrip_serde_full_fields() {
    let rec = make_record();
    let json = serde_json::to_string(&rec).unwrap();
    let deserialized: UsageRecord = serde_json::from_str(&json).unwrap();
    assert_eq!(deserialized, rec);
}

#[test]
fn usage_record_roundtrip_serde_with_metadata() {
    let rec = UsageRecord {
        metadata: Some(json!({"region": "eu-west", "shard": 7})),
        ..make_record()
    };
    let json = serde_json::to_string(&rec).unwrap();
    assert!(
        json.contains("\"metadata\""),
        "metadata must be present in JSON when Some, got: {json}"
    );
    let deserialized: UsageRecord = serde_json::from_str(&json).unwrap();
    assert_eq!(deserialized, rec);
}

#[test]
fn usage_record_subject_none_serde() {
    let rec = UsageRecord {
        subject: None,
        ..make_record()
    };
    let json = serde_json::to_string(&rec).unwrap();
    assert!(
        !json.contains("\"subject\""),
        "subject key must be absent from JSON when None, got: {json}"
    );
    let deserialized: UsageRecord = serde_json::from_str(&json).unwrap();
    assert_eq!(deserialized, rec);
}

#[test]
fn subject_rejects_unknown_nested_fields() {
    // Subject carries authz-relevant identity. `deny_unknown_fields` on the
    // nested struct ensures a typo like `subject: { idd: ... }` is a loud
    // error rather than a silent PDP-scoping downgrade — the top-level
    // `deny_unknown_fields` on UsageRecord cannot catch nested typos.
    let mut rec_value = serde_json::to_value(make_record()).unwrap();
    let subject_obj = rec_value
        .get_mut("subject")
        .and_then(|v| v.as_object_mut())
        .expect("make_record() produces a Some(subject) for this test");
    subject_obj.insert("idd".to_owned(), serde_json::Value::Null);
    let err = serde_json::from_value::<UsageRecord>(rec_value).unwrap_err();
    assert!(
        err.to_string().contains("idd") || err.to_string().contains("unknown field"),
        "deserialization error must mention the offending nested field, got: {err}"
    );
}

#[test]
fn usage_record_metadata_none_omitted_from_json() {
    let rec = UsageRecord {
        metadata: None,
        ..make_record()
    };
    let json = serde_json::to_string(&rec).unwrap();
    assert!(
        !json.contains("\"metadata\""),
        "metadata must be absent from JSON when None (not serialized as null), got: {json}"
    );
}

#[test]
fn usage_kind_wire_format_is_snake_case() {
    assert_eq!(
        serde_json::to_string(&UsageKind::Gauge).unwrap(),
        "\"gauge\""
    );
    assert_eq!(
        serde_json::to_string(&UsageKind::Counter).unwrap(),
        "\"counter\""
    );
    assert_eq!(
        serde_json::from_str::<UsageKind>("\"gauge\"").unwrap(),
        UsageKind::Gauge
    );
    assert_eq!(
        serde_json::from_str::<UsageKind>("\"counter\"").unwrap(),
        UsageKind::Counter
    );
    assert!(
        serde_json::from_str::<UsageKind>("\"Gauge\"").is_err(),
        "PascalCase must not deserialize - snake_case is the wire contract"
    );
}

#[test]
fn usage_kind_rejects_unknown_variants() {
    // Pins the closed-set contract: only "gauge" and "counter" are valid wire
    // values. An unknown variant like a third metric kind, or a JSON null,
    // must fail to deserialize rather than silently degrade to a default.
    assert!(
        serde_json::from_str::<UsageKind>("\"histogram\"").is_err(),
        "unknown variant must not deserialize"
    );
    assert!(
        serde_json::from_str::<UsageKind>("null").is_err(),
        "null must not deserialize as a UsageKind"
    );
}

#[test]
fn usage_record_non_finite_value_encodes_as_null_and_breaks_roundtrip() {
    // Pins the contract documented on `UsageRecord::value`: serde_json encodes
    // non-finite floats as JSON `null`, which then fails to round-trip back
    // into an `f64`. The operational signal is a *decode-time* error at the
    // next hop, not an encode error. Emitters are responsible for filtering
    // non-finite values before submit. This test catches a future swap to a
    // serializer with different non-finite handling (e.g. one that errors at
    // encode, or one that emits `"NaN"`), which would change the failure mode
    // operators see.
    for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        let rec = UsageRecord {
            value: bad,
            ..make_record()
        };
        let encoded = serde_json::to_string(&rec)
            .unwrap_or_else(|e| panic!("encoding {bad} must succeed (got error: {e})"));
        // Probe the actual encoding so the contract is pinned in one place.
        assert!(
            encoded.contains("\"value\":null"),
            "non-finite {bad} must encode as JSON null (silent data loss \
             contract — emitter is responsible for filtering); got: {encoded}"
        );
        // Round-trip must fail: null is not a valid f64 on the wire, which is
        // the eventual *downstream* signal operators will see if the emitter
        // does not filter.
        assert!(
            serde_json::from_str::<UsageRecord>(&encoded).is_err(),
            "non-finite-derived null must fail to round-trip into UsageRecord; \
             this is the operational failure mode at decode time"
        );
    }
}

#[test]
fn usage_record_rejects_unknown_fields() {
    // UsageRecord carries authz-relevant context (subject_id/subject_type).
    // deny_unknown_fields ensures a typo like `subject_idd` is a loud error
    // rather than a silent PDP-scoping downgrade.
    let rec = make_record();
    let mut value = serde_json::to_value(&rec).unwrap();
    value
        .as_object_mut()
        .unwrap()
        .insert("subject_idd".to_owned(), serde_json::Value::Null);
    let err = serde_json::from_value::<UsageRecord>(value).unwrap_err();
    assert!(
        err.to_string().contains("subject_idd") || err.to_string().contains("unknown field"),
        "deserialization error must mention the offending field, got: {err}"
    );
}

#[test]
fn module_config_tolerates_unknown_fields() {
    // ModuleConfig is documented as extensible (future rate-limit / quota
    // fields). This test pins the *opposite* choice from UsageRecord: an
    // unknown field must be silently ignored so newer collectors can add
    // fields without breaking older SDK consumers.
    let json = r#"{"allowed_metrics": [], "max_metadata_bytes": 8192, "future_quota_field": 42}"#;
    let cfg: ModuleConfig = serde_json::from_str(json).expect(
        "ModuleConfig must accept unknown fields for forward compatibility - \
         flip this test only after updating the doc on ModuleConfig",
    );
    assert!(cfg.allowed_metrics.is_empty());
    assert_eq!(cfg.max_metadata_bytes, 8192);
}

#[test]
fn module_config_roundtrip_with_max_metadata_bytes() {
    // Full round-trip with a non-default `max_metadata_bytes` value pins the
    // serde shape: the field is on the wire as `max_metadata_bytes` and the
    // value survives a serialize/deserialize cycle unchanged.
    let cfg = ModuleConfig {
        allowed_metrics: vec![AllowedMetric {
            name: "requests.total".to_owned(),
            kind: UsageKind::Counter,
        }],
        max_metadata_bytes: 16384,
    };
    let json = serde_json::to_string(&cfg).unwrap();
    assert!(
        json.contains("\"max_metadata_bytes\":16384"),
        "max_metadata_bytes must appear on the wire with the configured value, got: {json}"
    );
    let deserialized: ModuleConfig = serde_json::from_str(&json).unwrap();
    assert_eq!(deserialized, cfg);
}

#[test]
fn module_config_rejects_missing_max_metadata_bytes() {
    // `max_metadata_bytes` is a *required* serde field with no #[serde(default)].
    // Older payloads that omit it MUST fail to decode — the in-repo collector
    // and emitter ship together, so this wire-break surfaces version skew
    // rather than silently using an unspecified default. The exact JSON shape
    // below (mandated by the phase manifest) intentionally also uses the older
    // string-array form of `allowed_metrics` to mirror a pre-`AllowedMetric`
    // payload; either omission alone is enough to break the wire, and the
    // assertion only pins that deserialization fails.
    let json = r#"{
      "module_id": "test-module",
      "allowed_metrics": ["cpu", "mem"]
    }"#;
    let result = serde_json::from_str::<ModuleConfig>(json);
    assert!(
        result.is_err(),
        "payload omitting max_metadata_bytes must fail to deserialize, got: {result:?}"
    );
}

#[test]
fn module_config_roundtrip_with_zero_max_metadata_bytes() {
    // `0` carries the documented "metadata disabled" semantics at the SDK
    // layer; the emitter behavior under `0` is asserted in a later phase.
    // This test only pins that `0` is a valid wire value and round-trips.
    let cfg = ModuleConfig {
        allowed_metrics: Vec::new(),
        max_metadata_bytes: 0,
    };
    let json = serde_json::to_string(&cfg).unwrap();
    assert!(
        json.contains("\"max_metadata_bytes\":0"),
        "zero must serialize explicitly (not be dropped as default), got: {json}"
    );
    let deserialized: ModuleConfig = serde_json::from_str(&json).unwrap();
    assert_eq!(deserialized, cfg);
    assert_eq!(deserialized.max_metadata_bytes, 0);
}

#[test]
fn allowed_metric_roundtrip_serde() {
    let metric = AllowedMetric {
        name: "requests.total".to_owned(),
        kind: UsageKind::Counter,
    };
    let json = serde_json::to_string(&metric).unwrap();
    let deserialized: AllowedMetric = serde_json::from_str(&json).unwrap();
    assert_eq!(deserialized, metric);
    assert!(
        json.contains("\"counter\""),
        "AllowedMetric.kind must serialize as snake_case, got: {json}"
    );
}

#[test]
fn module_config_roundtrip_serde() {
    let cfg = ModuleConfig {
        allowed_metrics: vec![
            AllowedMetric {
                name: "requests.total".to_owned(),
                kind: UsageKind::Counter,
            },
            AllowedMetric {
                name: "bytes.in_flight".to_owned(),
                kind: UsageKind::Gauge,
            },
        ],
        max_metadata_bytes: 8192,
    };
    let json = serde_json::to_string(&cfg).unwrap();
    let deserialized: ModuleConfig = serde_json::from_str(&json).unwrap();
    assert_eq!(deserialized, cfg);
}

// ── Query-side SDK types (Feature 3) ─────────────────────────────────────

#[test]
fn aggregation_fn_serde_names_are_snake_case_round_trip() {
    for (variant, name) in [
        (AggregationFn::Sum, "\"sum\""),
        (AggregationFn::Count, "\"count\""),
        (AggregationFn::Min, "\"min\""),
        (AggregationFn::Max, "\"max\""),
        (AggregationFn::Avg, "\"avg\""),
    ] {
        assert_eq!(serde_json::to_string(&variant).unwrap(), name);
        assert_eq!(
            serde_json::from_str::<AggregationFn>(name).unwrap(),
            variant
        );
    }
    assert!(
        serde_json::from_str::<AggregationFn>("\"Sum\"").is_err(),
        "PascalCase must not deserialize - snake_case is the wire contract"
    );
}

#[test]
fn bucket_size_serde_names_are_snake_case_round_trip() {
    for (variant, name) in [
        (BucketSize::Minute, "\"minute\""),
        (BucketSize::Hour, "\"hour\""),
        (BucketSize::Day, "\"day\""),
        (BucketSize::Week, "\"week\""),
        (BucketSize::Month, "\"month\""),
    ] {
        assert_eq!(serde_json::to_string(&variant).unwrap(), name);
        assert_eq!(serde_json::from_str::<BucketSize>(name).unwrap(), variant);
    }
}

#[test]
fn group_by_dimension_serde_externally_tagged_for_time_bucket() {
    // TimeBucket carries a payload → externally-tagged object on the wire.
    let tb = GroupByDimension::TimeBucket(BucketSize::Day);
    let json = serde_json::to_string(&tb).unwrap();
    assert_eq!(json, "{\"time_bucket\":\"day\"}");
    let round: GroupByDimension = serde_json::from_str(&json).unwrap();
    assert_eq!(round, tb);
    // Unit variants serialize as bare strings.
    for (variant, name) in [
        (GroupByDimension::UsageType, "\"usage_type\""),
        (GroupByDimension::Subject, "\"subject\""),
        (GroupByDimension::Resource, "\"resource\""),
        (GroupByDimension::Source, "\"source\""),
    ] {
        assert_eq!(serde_json::to_string(&variant).unwrap(), name);
        assert_eq!(
            serde_json::from_str::<GroupByDimension>(name).unwrap(),
            variant
        );
    }
}

#[test]
fn aggregation_result_serde_omits_absent_dimensions() {
    let result = AggregationResult {
        function: AggregationFn::Count,
        value: 42.0,
        bucket_start: None,
        usage_type: Some("compute.cpu".to_owned()),
        subject_id: None,
        subject_type: None,
        resource_id: None,
        resource_type: None,
        source: None,
    };
    let json = serde_json::to_string(&result).unwrap();
    assert!(
        !json.contains("bucket_start"),
        "absent Option must not appear in JSON, got: {json}"
    );
    assert!(
        !json.contains("subject_id"),
        "absent subject_id must not appear in JSON, got: {json}"
    );
    assert!(
        json.contains("\"usage_type\":\"compute.cpu\""),
        "present grouping dimension must appear in JSON, got: {json}"
    );
    let deserialized: AggregationResult = serde_json::from_str(&json).unwrap();
    assert_eq!(deserialized, result);
}

#[test]
fn aggregation_result_serde_round_trip_with_all_dimensions() {
    let bucket = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();
    let result = AggregationResult {
        function: AggregationFn::Avg,
        value: 3.5,
        bucket_start: Some(bucket),
        usage_type: Some("network.bytes".to_owned()),
        subject_id: Some(Uuid::nil()),
        subject_type: Some("user".to_owned()),
        resource_id: Some(Uuid::nil()),
        resource_type: Some("compute.vm".to_owned()),
        source: Some("billing".to_owned()),
    };
    let json = serde_json::to_string(&result).unwrap();
    let deserialized: AggregationResult = serde_json::from_str(&json).unwrap();
    assert_eq!(deserialized, result);
}

#[test]
fn raw_query_carries_optional_cursor() {
    // CursorV1 is the keyset cursor type for paginated raw queries — the SDK
    // re-exports it from modkit-odata.
    let cursor = CursorV1 {
        k: vec![
            "2026-01-01T06:00:00+00:00".to_owned(),
            Uuid::nil().to_string(),
        ],
        o: SortDir::Asc,
        s: "+timestamp,+id".to_owned(),
        f: None,
        d: "fwd".to_owned(),
    };
    let from = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();
    let to = Utc.with_ymd_and_hms(2026, 1, 2, 0, 0, 0).unwrap();
    let q = RawQuery {
        scope: AccessScope::deny_all(),
        time_range: (from, to),
        usage_type: Some("network.bytes".to_owned()),
        resource_id: None,
        resource_type: None,
        subject_type: Some("user".to_owned()),
        subject_id: None,
        cursor: Some(cursor.clone()),
        page_size: 50,
    };
    let q_cursor = q.cursor.as_ref().expect("cursor must round-trip");
    assert_eq!(q_cursor.k, cursor.k);
    assert_eq!(q_cursor.s, cursor.s);
}

#[test]
fn cursor_v1_encode_decode_round_trip() {
    // Re-pin the modkit-odata cursor contract the SDK depends on: a freshly
    // encoded cursor decodes back into an equivalent value.
    let original = CursorV1 {
        k: vec![
            "2026-01-01T06:00:00+00:00".to_owned(),
            Uuid::nil().to_string(),
        ],
        o: SortDir::Asc,
        s: "+timestamp,+id".to_owned(),
        f: None,
        d: "fwd".to_owned(),
    };
    let encoded = original.encode().expect("CursorV1 encode is infallible");
    let decoded = CursorV1::decode(&encoded).expect("decoded a freshly-encoded cursor");
    assert_eq!(decoded.k, original.k);
    assert_eq!(decoded.s, original.s);
    assert_eq!(decoded.d, original.d);
}

#[test]
fn page_round_trip_with_usage_record() {
    let pi = PageInfo {
        next_cursor: Some("cursor-token".to_owned()),
        prev_cursor: None,
        limit: 50,
    };
    let page: Page<UsageRecord> = Page::new(vec![make_record()], pi);
    let json = serde_json::to_string(&page).unwrap();
    let decoded: Page<UsageRecord> = serde_json::from_str(&json).unwrap();
    assert_eq!(decoded.items.len(), 1);
    assert_eq!(decoded.page_info.limit, 50);
    assert_eq!(
        decoded.page_info.next_cursor.as_deref(),
        Some("cursor-token")
    );
}
