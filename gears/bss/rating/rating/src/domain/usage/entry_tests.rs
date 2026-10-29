#![allow(clippy::unwrap_used, clippy::expect_used)]

use serde_json::{Value, json};
use time::OffsetDateTime;
use uuid::Uuid;

use super::{
    CURRENT_DIGEST_VERSION, DigestVersion, MAX_RAW_ENTRY_BYTES, bounded, canonical_instant, digest,
    identify,
};
use crate::domain::digest::Sha256Hex;

fn entry() -> Value {
    json!({
        "id": "0192f1a0-0000-7000-8000-0000000000a1",
        "tenant_id": "00000000-0000-0000-0000-000000000010",
        "gts_type_id": "gts.cf.core.uc.usage_record.v2~cf.oss.cloudlet_hours.v1~",
        "resource_ref": {"resource_id": "ct-7f3a", "resource_type": "container"},
        "quantity": "8",
        "idempotency_key": "k1",
        "accepted_at": "2026-10-08T11:00:07Z",
        "origin": "live",
        "window_start": "2026-10-08T10:00:00Z",
        "window_end": "2026-10-08T11:00:00Z",
    })
}

fn digest_v1(value: &Value) -> Sha256Hex {
    digest(value, CURRENT_DIGEST_VERSION)
}

#[test]
fn an_entry_is_identified_by_its_tenant_and_id() {
    let identity = identify(&entry()).unwrap();
    assert_eq!(identity.tenant_id, Uuid::from_u128(0x10));
    for unreadable in [
        json!({"id": "nope", "tenant_id": "00000000-0000-0000-0000-000000000010"}),
        json!({"id": "0192f1a0-0000-7000-8000-0000000000a1"}),
        json!({"id": "00000000-0000-0000-0000-000000000000", "tenant_id": "00000000-0000-0000-0000-000000000010"}),
        json!("not an object"),
    ] {
        assert_eq!(identify(&unreadable), None);
    }
}

#[test]
fn an_optional_field_added_later_does_not_change_the_digest() {
    // V74.
    let mut extended = entry();
    extended["new_optional_field"] = json!("added by the collector later");
    assert_eq!(digest_v1(&extended), digest_v1(&entry()));
}

#[test]
fn member_order_does_not_change_the_digest() {
    let reordered: Value = serde_json::from_str(
        r#"{"window_end":"2026-10-08T11:00:00Z","window_start":"2026-10-08T10:00:00Z",
            "origin":"live","accepted_at":"2026-10-08T11:00:07Z","idempotency_key":"k1",
            "quantity":"8","resource_ref":{"resource_type":"container","resource_id":"ct-7f3a"},
            "gts_type_id":"gts.cf.core.uc.usage_record.v2~cf.oss.cloudlet_hours.v1~",
            "tenant_id":"00000000-0000-0000-0000-000000000010",
            "id":"0192f1a0-0000-7000-8000-0000000000a1"}"#,
    )
    .unwrap();
    assert_eq!(digest_v1(&reordered), digest_v1(&entry()));
}

#[test]
fn an_absent_metadata_and_an_empty_one_digest_alike() {
    let mut empty = entry();
    empty["metadata"] = json!({});
    assert_eq!(digest_v1(&empty), digest_v1(&entry()));
    let mut filled = entry();
    filled["metadata"] = json!({"region": "eu"});
    assert_ne!(digest_v1(&filled), digest_v1(&entry()));
}

#[test]
fn one_instant_written_two_ways_digests_alike() {
    let mut fractional = entry();
    fractional["window_start"] = json!("2026-10-08T12:00:00.000000+02:00");
    assert_eq!(digest_v1(&fractional), digest_v1(&entry()));
}

#[test]
fn a_changed_caller_field_changes_the_digest() {
    // V64: the same id re-read with other content.
    for (field, value) in [
        ("quantity", json!("9")),
        ("quantity", json!("8.0")),
        ("idempotency_key", json!("k2")),
        ("window_end", json!("2026-10-08T11:30:00Z")),
        ("reason_code", json!("RETRACTED")),
    ] {
        let mut changed = entry();
        changed[field] = value;
        assert_ne!(
            digest_v1(&changed),
            digest_v1(&entry()),
            "{field} is pinned"
        );
    }
}

#[test]
fn entry_type_is_not_pinned() {
    let mut stated = entry();
    stated["entry_type"] = json!("record");
    assert_eq!(digest_v1(&stated), digest_v1(&entry()));
}

#[test]
fn a_stored_digest_version_is_known_only_when_this_build_digests_it() {
    assert_eq!(DigestVersion::parse(1), Some(DigestVersion::V1));
    assert_eq!(DigestVersion::V1.as_i16(), 1);
    assert_eq!(DigestVersion::parse(2), None);
    assert_eq!(DigestVersion::parse(0), None);
}

#[test]
fn an_entry_within_the_row_bound_is_stored_whole() {
    let bounded = bounded(&entry());
    assert_eq!(bounded.value, entry());
    assert_eq!(bounded.oversize, None);
}

#[test]
fn an_oversized_entry_is_stored_as_a_stub_with_its_size_and_an_excerpt() {
    let mut big = entry();
    // Multi-byte characters, so the excerpt must cut on a character boundary.
    big["metadata"] = json!({"note": "\u{e9}".repeat(MAX_RAW_ENTRY_BYTES)});
    let size = big.to_string().len();
    let bounded = bounded(&big);
    assert_eq!(bounded.oversize, Some(size));
    let stub = &bounded.value["oversize"];
    assert_eq!(stub["bytes"], json!(size));
    let excerpt = stub["excerpt"].as_str().unwrap();
    assert!(excerpt.len() <= 1024 && big.to_string().starts_with(excerpt));
    assert!(bounded.value.to_string().len() < 2048);
    // The digest is still taken over the whole entry.
    assert_ne!(digest_v1(&bounded.value), digest_v1(&big));
}

#[test]
fn the_canonical_instant_is_the_collector_identity_form() {
    assert_eq!(
        canonical_instant("2026-10-08T10:00:00Z").as_deref(),
        Some("2026-10-08T10:00:00.000000Z")
    );
    assert_eq!(canonical_instant("2026-10-08T10:00:00.0000001Z"), None);
    assert_eq!(canonical_instant("yesterday"), None);
}

proptest::proptest! {
    #[test]
    fn every_spelling_of_one_microsecond_instant_canonicalizes_alike(
        seconds in 0_i64..253_402_300_799,
        micros in 0_u32..1_000_000,
        offset_minutes in -1_439_i32..=1_439,
        fraction_digits in 0_usize..=9,
    ) {
        let utc = OffsetDateTime::from_unix_timestamp(seconds)
            .unwrap()
            .replace_microsecond(micros)
            .unwrap();
        let offset = time::UtcOffset::from_whole_seconds(offset_minutes * 60).unwrap();
        let local = utc.to_offset(offset);
        // The instant spelled at the offset, with as many fractional digits as it needs or more.
        let digits = if micros == 0 { fraction_digits } else { fraction_digits.max(6) };
        let fraction = if digits == 0 {
            String::new()
        } else {
            format!(".{:0>9}", local.nanosecond())[..=digits].to_owned()
        };
        let sign = if offset_minutes < 0 { '-' } else { '+' };
        let offset = time::UtcOffset::from_whole_seconds(offset_minutes.abs() * 60).unwrap();
        let spelled = format!(
            "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}{fraction}{sign}{:02}:{:02}",
            local.year(),
            u8::from(local.month()),
            local.day(),
            local.hour(),
            local.minute(),
            local.second(),
            offset.whole_hours(),
            offset.minutes_past_hour(),
        );
        let canonical = canonical_instant(&spelled);
        proptest::prop_assume!(utc.year() <= 9999 && local.year() <= 9999 && local.year() >= 0);
        let expected = format!(
            "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:06}Z",
            utc.year(),
            u8::from(utc.month()),
            utc.day(),
            utc.hour(),
            utc.minute(),
            utc.second(),
            micros
        );
        proptest::prop_assert_eq!(canonical.as_deref(), Some(expected.as_str()), "{}", spelled);
        // The canonical form is a fixed point.
        proptest::prop_assert_eq!(canonical_instant(&expected), Some(expected));
    }
}
