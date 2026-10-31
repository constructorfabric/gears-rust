//! Wire-shape and construction tests for the aggregate read path's fold,
//! dimensions and result.

use std::collections::HashSet;

use bigdecimal::BigDecimal;
use serde_json::json;
use uuid::Uuid;

use super::{AggregationBucket, AggregationDimension, AggregationFold, AggregationResult};
use crate::v2::error::UsageCollectorError;
use crate::v2::models::MetadataKey;

fn metadata_key(value: &str) -> MetadataKey {
    MetadataKey::new(value).expect("test fixture supplies a valid metadata key")
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
fn aggregation_result_carries_empty_key_for_no_grouping() {
    let result = AggregationResult {
        buckets: vec![AggregationBucket {
            key: Vec::new(),
            value: Some(BigDecimal::from(42)),
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
        value: Some(BigDecimal::from(7)),
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
        value: Some(BigDecimal::from(1)),
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
fn aggregation_bucket_value_above_rust_decimal_ceiling_round_trips() {
    // 2^96 rounded up — beyond rust_decimal's ~7.9e28 ceiling, so this would
    // 500 under the old `Decimal` carrier. It must round-trip exactly now.
    let big = "79228162514264337593543950400";
    let bucket = AggregationBucket {
        key: Vec::new(),
        value: Some(big.parse::<BigDecimal>().expect("bigdecimal parses")),
    };
    let value = serde_json::to_value(&bucket).expect("serialize");
    assert_eq!(value, json!({ "value": "79228162514264337593543950400" }));
    let decoded: AggregationBucket = serde_json::from_value(value).expect("round-trip");
    assert_eq!(decoded, bucket);
}

#[test]
fn aggregation_bucket_negative_value_round_trips() {
    // A measured decrease is an ordinary entry with a negative quantity, so
    // a bucket can carry one (and a set of them can net to zero). The sign
    // must survive the string wire encoding round-trip.
    let bucket = AggregationBucket {
        key: Vec::new(),
        value: Some(BigDecimal::from(-42)),
    };
    let value = serde_json::to_value(&bucket).expect("serialize");
    assert_eq!(value, json!({ "value": "-42" }));
    let decoded: AggregationBucket = serde_json::from_value(value).expect("round-trip");
    assert_eq!(decoded, bucket);
}

#[test]
fn aggregation_fold_serde_round_trips_screaming_case() {
    for (fold, wire) in [
        (AggregationFold::Sum, "\"SUM\""),
        (AggregationFold::Count, "\"COUNT\""),
        (AggregationFold::Max, "\"MAX\""),
        (AggregationFold::Min, "\"MIN\""),
        (AggregationFold::Latest, "\"LATEST\""),
    ] {
        assert_eq!(serde_json::to_string(&fold).unwrap(), wire);
        assert_eq!(serde_json::from_str::<AggregationFold>(wire).unwrap(), fold);
    }
}

#[test]
fn aggregation_fold_rejects_avg() {
    // AVG is not a declared fold: a declaration naming it must fail
    // resolution rather than silently pick another.
    assert!(serde_json::from_str::<AggregationFold>("\"AVG\"").is_err());

    let err = "AVG"
        .parse::<AggregationFold>()
        .expect_err("AVG must be rejected by FromStr");
    assert!(
        matches!(err, UsageCollectorError::InvalidArgument { ref field, ref detail, .. } if field == "aggregation_fold" && detail.contains("AVG")),
        "expected InvalidArgument on field `aggregation_fold` naming AVG, got {err:?}"
    );
}

#[test]
fn aggregation_fold_from_str_matches_the_wire_shape() {
    assert_eq!(
        "SUM".parse::<AggregationFold>().unwrap(),
        AggregationFold::Sum
    );
    assert_eq!(
        "LATEST".parse::<AggregationFold>().unwrap(),
        AggregationFold::Latest
    );
    // Case-sensitive on purpose: the enum in the trait schema is upper case,
    // and accepting "sum" would admit a declaration the registry rejects.
    assert!("sum".parse::<AggregationFold>().is_err());
}

#[test]
fn aggregation_fold_as_str_matches_the_wire_spelling() {
    assert_eq!(AggregationFold::Sum.as_str(), "SUM");
    assert_eq!(AggregationFold::Count.as_str(), "COUNT");
    assert_eq!(AggregationFold::Max.as_str(), "MAX");
    assert_eq!(AggregationFold::Min.as_str(), "MIN");
    assert_eq!(AggregationFold::Latest.as_str(), "LATEST");
}

#[test]
fn aggregation_fold_display_matches_as_str() {
    assert_eq!(AggregationFold::Latest.to_string(), "LATEST");
}

/// serde and strum each derive the spelling from their own `rename_all`
/// attribute, so this pins the two together for every fold, including one
/// added later.
#[test]
fn every_fold_spells_and_parses_as_its_serde_wire_form() {
    for &fold in AggregationFold::ALL {
        let wire = serde_json::to_value(fold).expect("serialize");
        assert_eq!(wire, json!(fold.as_str()), "{fold:?}");
        assert_eq!(fold.to_string(), fold.as_str(), "{fold:?}");
        assert_eq!(fold.as_str().parse::<AggregationFold>().unwrap(), fold);
    }
}

/// `ALL_UNIT_VARIANTS` is hand-listed (`strum::VariantArray` refuses the
/// data-carrying `Metadata`), so this pins it to the enum: every variant but
/// `Metadata`, each once.
#[test]
fn all_unit_variants_lists_every_dimension_but_metadata() {
    let unit = AggregationDimension::ALL_UNIT_VARIANTS;
    assert_eq!(
        unit.len(),
        <AggregationDimension as strum::EnumCount>::COUNT - 1,
        "a dimension was added to the enum but not to ALL_UNIT_VARIANTS"
    );
    let distinct: HashSet<&AggregationDimension> = unit.iter().collect();
    assert_eq!(
        distinct.len(),
        unit.len(),
        "ALL_UNIT_VARIANTS repeats a variant"
    );
    assert!(
        !unit
            .iter()
            .any(|d| matches!(d, AggregationDimension::Metadata(_))),
        "Metadata carries a key and cannot be listed"
    );
}

/// Pins the published bucket cap. It is `AggregatedQueryResult.buckets`'
/// `maxItems` on the wire and the bound plugins size their
/// `LIMIT MAX_AGGREGATION_BUCKETS + 1` from, so changing it is a wire change.
#[test]
fn the_published_aggregation_bucket_cap_is_one_hundred_thousand() {
    assert_eq!(crate::v2::MAX_AGGREGATION_BUCKETS, 100_000);
}

/// `AggregationBucketV2` lists `value` in `required`, and `None` means the
/// bucket matched no rows — a real answer, not an absent field.
#[test]
fn an_aggregation_bucket_without_a_value_is_refused() {
    let with_value = serde_json::json!({ "key": ["eu"], "value": "42" });
    serde_json::from_value::<AggregationBucket>(with_value).expect("a present value decodes");

    let explicit_null = serde_json::json!({ "key": ["eu"], "value": null });
    let decoded = serde_json::from_value::<AggregationBucket>(explicit_null)
        .expect("null is the empty-bucket answer");
    assert!(decoded.value.is_none());

    let omitted = serde_json::json!({ "key": ["eu"] });
    assert!(
        serde_json::from_value::<AggregationBucket>(omitted).is_err(),
        "an omitted `value` is a missing required field, not an empty bucket",
    );
}
