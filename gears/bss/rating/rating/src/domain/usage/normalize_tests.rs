#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeSet;

use serde_json::{Value, json};
use uuid::Uuid;

use super::{
    EntryKind, EntryType, InvalidationRef, MAX_ATTRIBUTION_LEN, MAX_IDEMPOTENCY_KEY_LEN,
    MAX_QUANTITY_SIGNIFICANT_DIGITS, MAX_REASON_CODE_LEN, Origin, Quantity, RejectReason,
    UsageSubject, normalize,
};
use crate::domain::usage::declarations::{Fold, MeterDeclaration, PageDeclarations};
use crate::domain::usage::entry::EntryIdentity;
use crate::domain::usage::feed::MeterId;

const METER: &str = "gts.cf.core.uc.usage_record.v2~cf.oss.cloudlet_hours.v1~";

fn identity() -> EntryIdentity {
    EntryIdentity {
        tenant_id: Uuid::from_u128(0x10),
        usage_record_id: Uuid::from_u128(0xa1),
    }
}

fn record() -> Value {
    json!({
        "id": identity().usage_record_id,
        "tenant_id": identity().tenant_id,
        "gts_type_id": METER,
        "resource_ref": {"resource_id": "ct-7f3a", "resource_type": "container"},
        "quantity": "8",
        "idempotency_key": "k1",
        "accepted_at": "2026-10-08T11:00:07Z",
        "origin": "live",
        "window_start": "2026-10-08T10:00:00Z",
        "window_end": "2026-10-08T11:00:00Z",
    })
}

fn declarations(fold: Fold, keys: &[&str]) -> PageDeclarations {
    let mut page = PageDeclarations::new();
    page.insert(
        MeterId::parse(METER).unwrap(),
        Some(MeterDeclaration {
            fold,
            canonical_unit: "h".to_owned(),
            metadata_keys: keys
                .iter()
                .map(|k| (*k).to_owned())
                .collect::<BTreeSet<_>>(),
        }),
    );
    page
}

fn rejected(entry: &Value, page: &PageDeclarations) -> RejectReason {
    normalize(entry, identity(), page).unwrap_err().reason
}

#[test]
fn a_v2_record_maps_onto_the_normalized_columns() {
    let normalized = normalize(&record(), identity(), &declarations(Fold::Sum, &[])).unwrap();
    assert_eq!(normalized.gts_type.as_str(), METER);
    assert_eq!(normalized.entry_type(), EntryType::Record);
    assert_eq!(normalized.kind, EntryKind::Record);
    assert_eq!(normalized.invalidation(), None);
    assert_eq!(normalized.quantity.as_str(), "8");
    assert_eq!(normalized.resource_id, "ct-7f3a");
    assert_eq!(normalized.origin, Origin::Live);
    assert!(normalized.metadata.is_empty());
    assert_eq!(normalized.metadata_json(), json!({}));
    assert!(normalized.interval_end > normalized.interval_start);
}

#[test]
fn an_entry_carrying_invalidates_is_an_invalidation() {
    let mut entry = record();
    entry["invalidates"] = json!(Uuid::from_u128(0xa0));
    entry["reason_code"] = json!("DUPLICATE_EMISSION");
    let normalized = normalize(&entry, identity(), &declarations(Fold::Sum, &[])).unwrap();
    assert_eq!(normalized.entry_type(), EntryType::Invalidation);
    assert_eq!(
        normalized.invalidation(),
        Some(&InvalidationRef {
            target: Uuid::from_u128(0xa0),
            reason_code: "DUPLICATE_EMISSION".to_owned(),
        })
    );
    // A stated entry type (the REST shape) must agree.
    entry["entry_type"] = json!("record");
    assert_eq!(
        rejected(&entry, &declarations(Fold::Sum, &[])),
        RejectReason::UsageMalformed
    );
    entry["entry_type"] = json!("invalidation");
    assert!(normalize(&entry, identity(), &declarations(Fold::Sum, &[])).is_ok());
}

#[test]
fn invalidates_and_reason_code_are_both_or_neither() {
    let page = declarations(Fold::Sum, &[]);
    let mut half = record();
    half["invalidates"] = json!(Uuid::from_u128(0xa0));
    assert_eq!(rejected(&half, &page), RejectReason::UsageMalformed);
    let mut other_half = record();
    other_half["reason_code"] = json!("X");
    assert_eq!(rejected(&other_half, &page), RejectReason::UsageMalformed);
}

#[test]
fn an_entry_without_a_covered_period_is_interval_missing() {
    let page = declarations(Fold::Sum, &[]);
    let mut entry = record();
    entry.as_object_mut().unwrap().remove("window_end");
    assert_eq!(rejected(&entry, &page), RejectReason::IntervalMissing);
    let mut inverted = record();
    inverted["window_end"] = json!("2026-10-08T09:00:00Z");
    assert_eq!(rejected(&inverted, &page), RejectReason::UsageMalformed);
}

#[test]
fn a_point_event_has_equal_bounds() {
    let mut entry = record();
    entry["window_end"] = entry["window_start"].clone();
    assert!(normalize(&entry, identity(), &declarations(Fold::Sum, &[])).is_ok());
}

#[test]
fn an_undeclared_meter_or_metadata_key_is_malformed() {
    assert_eq!(
        rejected(&record(), &PageDeclarations::new()),
        RejectReason::UsageMalformed
    );
    let mut not_declared = PageDeclarations::new();
    not_declared.insert(MeterId::parse(METER).unwrap(), None);
    assert_eq!(
        rejected(&record(), &not_declared),
        RejectReason::UsageMalformed
    );
    let mut entry = record();
    entry["metadata"] = json!({"region": "eu"});
    assert_eq!(
        rejected(&entry, &declarations(Fold::Sum, &[])),
        RejectReason::UsageMalformed
    );
    assert!(normalize(&entry, identity(), &declarations(Fold::Sum, &["region"])).is_ok());
    entry["metadata"] = json!({"region": 1});
    assert_eq!(
        rejected(&entry, &declarations(Fold::Sum, &["region"])),
        RejectReason::UsageMalformed
    );
}

#[test]
fn a_meter_whose_fold_is_not_sum_is_not_rejected() {
    // 12 §4.7: evaluation decides (usage_type_not_chargeable, unsupported_input_fold), never intake.
    for fold in [Fold::Count, Fold::Max, Fold::Min, Fold::Latest] {
        assert!(normalize(&record(), identity(), &declarations(fold, &[])).is_ok());
    }
}

#[test]
fn a_quantity_follows_the_collector_pattern() {
    for accepted in [
        "0",
        "8",
        "-8",
        "0.5",
        "42.500",
        "1234567890123456789012345678",
        "0.1234567890123456789012345678",
        "-0.5",
    ] {
        assert!(
            Quantity::parse(accepted).is_some(),
            "{accepted} is a quantity"
        );
    }
    for refused in [
        "",
        "08",
        "1.",
        ".5",
        "1e3",
        "-0",
        "-0.000",
        "+1",
        "12345678901234567890123456789",
        "1.12345678901234567890123456789",
        "1234567890123456.1234567890123",
    ] {
        assert!(Quantity::parse(refused).is_none(), "{refused} is refused");
    }
    let mut number = record();
    number["quantity"] = json!(8);
    assert_eq!(
        rejected(&number, &declarations(Fold::Sum, &[])),
        RejectReason::UsageMalformed
    );
}

#[test]
fn bounds_finer_than_a_microsecond_are_malformed() {
    let mut entry = record();
    entry["window_start"] = json!("2026-10-08T10:00:00.0000001Z");
    assert_eq!(
        rejected(&entry, &declarations(Fold::Sum, &[])),
        RejectReason::UsageMalformed
    );
}

#[test]
fn every_required_field_is_checked() {
    let page = declarations(Fold::Sum, &[]);
    for field in [
        "gts_type_id",
        "quantity",
        "resource_ref",
        "idempotency_key",
        "origin",
        "accepted_at",
    ] {
        let mut entry = record();
        entry.as_object_mut().unwrap().remove(field);
        assert_eq!(
            rejected(&entry, &page),
            RejectReason::UsageMalformed,
            "{field} is required"
        );
    }
    let mut long_key = record();
    long_key["idempotency_key"] = json!("k".repeat(MAX_IDEMPOTENCY_KEY_LEN + 1));
    assert_eq!(rejected(&long_key, &page), RejectReason::UsageMalformed);
}

#[test]
fn the_natural_key_is_the_collector_dedup_identity() {
    let normalized = normalize(&record(), identity(), &declarations(Fold::Sum, &[])).unwrap();
    let key = normalized.natural_key();
    assert_eq!(key.tenant_id, identity().tenant_id);
    assert_eq!(key.idempotency_key, "k1");
    assert_eq!(key.entry_type, EntryType::Record);
}

#[test]
fn reject_reasons_round_trip() {
    for reason in RejectReason::ALL {
        assert_eq!(RejectReason::parse(reason.as_str()), Some(reason));
    }
}

#[test]
fn a_malformed_meter_keeps_the_reason_the_meter_id_was_refused() {
    let mut entry = record();
    entry["gts_type_id"] = json!("gts.cf.core.uc.usage_record.v2~two~segments~");
    let rejection = normalize(&entry, identity(), &declarations(Fold::Sum, &[])).unwrap_err();
    assert_eq!(rejection.reason, RejectReason::UsageMalformed);
    assert!(
        rejection.detail.contains("exactly one segment"),
        "{}",
        rejection.detail
    );
}

#[test]
fn an_invalidation_target_must_be_a_non_nil_entry_id() {
    for target in [
        json!(Uuid::nil()),
        json!("not-a-uuid"),
        json!(42),
        json!({"id": "x"}),
    ] {
        let mut entry = record();
        entry["invalidates"] = target.clone();
        entry["reason_code"] = json!("DUPLICATE_EMISSION");
        let page = declarations(Fold::Sum, &[]);
        assert_eq!(
            rejected(&entry, &page),
            RejectReason::UsageMalformed,
            "{target}"
        );
    }
}

#[test]
fn a_reason_code_holds_at_most_128_characters() {
    let mut entry = record();
    entry["invalidates"] = json!(Uuid::from_u128(0xa0));
    entry["reason_code"] = json!("r".repeat(MAX_REASON_CODE_LEN));
    let page = declarations(Fold::Sum, &[]);
    assert!(normalize(&entry, identity(), &page).is_ok());
    entry["reason_code"] = json!("r".repeat(MAX_REASON_CODE_LEN + 1));
    assert_eq!(rejected(&entry, &page), RejectReason::UsageMalformed);
}

#[test]
fn a_subject_ref_keeps_its_two_validated_fields_only() {
    let mut entry = record();
    entry["subject_ref"] = json!({
        "subject_id": "u-1",
        "subject_type": "user",
        "unbounded": {"nested": "x".repeat(10_000)},
    });
    let normalized = normalize(&entry, identity(), &declarations(Fold::Sum, &[])).unwrap();
    let subject = normalized.subject.unwrap();
    assert_eq!(
        subject,
        UsageSubject {
            subject_id: "u-1".to_owned(),
            subject_type: Some("user".to_owned()),
        }
    );
    assert_eq!(
        subject.to_json(),
        json!({"subject_id": "u-1", "subject_type": "user"})
    );
    entry["subject_ref"] = json!({"subject_id": "u-1"});
    let normalized = normalize(&entry, identity(), &declarations(Fold::Sum, &[])).unwrap();
    assert_eq!(
        normalized.subject.unwrap().to_json(),
        json!({"subject_id": "u-1"})
    );
}

#[test]
fn a_subject_ref_that_is_not_an_object_or_lacks_a_bounded_id_is_malformed() {
    let page = declarations(Fold::Sum, &[]);
    for subject in [
        json!("u-1"),
        json!({}),
        json!({"subject_type": "user"}),
        json!({"subject_id": ""}),
        json!({"subject_id": "u-1", "subject_type": "t".repeat(MAX_ATTRIBUTION_LEN + 1)}),
        json!({"subject_id": "u\u{1}1"}),
    ] {
        let mut entry = record();
        entry["subject_ref"] = subject.clone();
        assert_eq!(
            rejected(&entry, &page),
            RejectReason::UsageMalformed,
            "{subject}"
        );
    }
}

#[test]
fn metadata_is_a_map_of_strings_without_nul_under_declared_keys() {
    let page = declarations(Fold::Sum, &["region"]);
    let mut entry = record();
    entry["metadata"] = json!({"region": "eu-west"});
    let normalized = normalize(&entry, identity(), &page).unwrap();
    assert_eq!(
        normalized.metadata.get("region").map(String::as_str),
        Some("eu-west")
    );
    assert_eq!(normalized.metadata_json(), json!({"region": "eu-west"}));
    entry["metadata"] = json!({"region": "eu\u{0}west"});
    assert_eq!(rejected(&entry, &page), RejectReason::UsageMalformed);
    entry["metadata"] = json!({"region": 7});
    assert_eq!(rejected(&entry, &page), RejectReason::UsageMalformed);
}

#[test]
fn text_fields_refuse_what_the_collector_refuses_and_nothing_more() {
    let page = declarations(Fold::Sum, &[]);
    // An ASCII control character (C0 or DEL) is refused, as the collector refuses it.
    for refused in ["ct\u{7}a", "ct\u{7f}a", "ct\na"] {
        let mut entry = record();
        entry["resource_ref"]["resource_id"] = json!(refused);
        assert_eq!(
            rejected(&entry, &page),
            RejectReason::UsageMalformed,
            "{refused:?}"
        );
    }
    // A C1 control or a bidi override the collector admitted is kept: refusing it would leave
    // metered usage rejected with no retry that could fix it.
    for admitted in ["ct\u{85}a", "ct\u{202e}a"] {
        let mut entry = record();
        entry["resource_ref"]["resource_id"] = json!(admitted);
        let normalized = normalize(&entry, identity(), &page).unwrap();
        assert_eq!(normalized.resource_id, admitted);
    }
    // The meter id applies the same rule.
    assert!(MeterId::parse("gts.cf.core.uc.usage_record.v2~cf.\u{85}.v1~").is_ok());
    assert!(MeterId::parse("gts.cf.core.uc.usage_record.v2~cf.\u{7}.v1~").is_err());
}

/// The collector's quantity pattern (`usage-collector-sdk/src/v2/quantity.rs`).
static QUANTITY_PATTERN: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
    regex::Regex::new(r"^-?(0|[1-9][0-9]{0,27})(\.[0-9]{1,28})?$").unwrap()
});

/// Whether the collector admits `text`: its pattern, at most 28 significant digits, no `-0`.
fn collector_admits(text: &str) -> bool {
    if !QUANTITY_PATTERN.is_match(text) {
        return false;
    }
    let digits: String = text.chars().filter(char::is_ascii_digit).collect();
    let significant = digits.trim_start_matches('0').len();
    significant <= MAX_QUANTITY_SIGNIFICANT_DIGITS && !(text.starts_with('-') && significant == 0)
}

proptest::proptest! {
    #[test]
    fn a_quantity_is_parsed_exactly_when_the_collector_admits_it(
        text in r"-?(0|[0-9]{1,30})(\.[0-9]{0,30})?|[-0-9.eE+ ]{0,12}|\PC{0,8}"
    ) {
        let parsed = Quantity::parse(&text);
        proptest::prop_assert_eq!(parsed.is_some(), collector_admits(&text), "{:?}", text);
        if let Some(quantity) = parsed {
            proptest::prop_assert_eq!(quantity.as_str(), text.as_str());
        }
    }
}
