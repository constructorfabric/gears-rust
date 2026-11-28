//! The canonical rendering and the domain-separated quota digests an adapter reproduces
//! (P-D-267).
#![allow(clippy::unwrap_used)]
use super::*;
use serde_json::json;
use time::macros::datetime;
use uuid::uuid;

/// The canonical digest of the quotas package's example snapshot, recomputed from its members.
///
/// Computed independently, from the repository root, with:
///
/// ```text
/// printf 'cf.bss.products.quota_capability_snapshot.v1\n%s' "$(python3 -I -c '
/// import json, sys
/// e = json.load(open("gears/bss/docs/quotas/schemas/quota-capability-discovery.example.json"))
/// o = {k: e[k] for k in ("snapshot_id", "source_revision", "observed_at", "item_count")}
/// o["items"] = sorted(e["items"], key=lambda i: i["id"])
/// sys.stdout.write(json.dumps(o, sort_keys=True, separators=(",", ":"), ensure_ascii=False))
/// ')" | shasum -a 256
/// ```
const PINNED_EXAMPLE_DIGEST: &str =
    "61a950ad006b3fdabfd6b5bceac5662bcfabff24c25c4c2ffe21e62139625c81";

#[test]
fn the_rendering_sorts_keys_at_every_depth_and_keeps_arrays_in_order() {
    let v = json!({"b": [3, {"z": 1, "a": "x"}], "a": null});
    assert_eq!(render(&v), r#"{"a":null,"b":[3,{"a":"x","z":1}]}"#);
}

#[test]
fn the_snapshot_digest_ignores_item_order_and_excludes_cursor_and_digest() {
    let a = json!({"id": "22d9d2b8-9dad-46b3-93af-e6c2b53b41df", "capability_key": "a"});
    let b = json!({"id": "11d9d2b8-9dad-46b3-93af-e6c2b53b41df", "capability_key": "b"});
    let id = uuid!("9144d0b7-3497-4db9-89e7-69c2c5d8f035");
    let at = datetime!(2026-11-13 12:00 UTC);
    let one = snapshot_digest(id, "7", at, 2, &[a.clone(), b.clone()]).unwrap();
    let two = snapshot_digest(id, "7", at, 2, &[b, a]).unwrap();
    assert_eq!(one, two);
    assert_eq!(one.len(), 64);
    assert!(
        one.chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
    );
}

#[test]
fn every_member_of_the_snapshot_identity_moves_the_digest() {
    let item = json!({"id": "22d9d2b8-9dad-46b3-93af-e6c2b53b41df"});
    let id = uuid!("9144d0b7-3497-4db9-89e7-69c2c5d8f035");
    let at = datetime!(2026-11-13 12:00 UTC);
    let base = snapshot_digest(id, "7", at, 1, std::slice::from_ref(&item)).unwrap();
    assert_ne!(
        base,
        snapshot_digest(id, "8", at, 1, std::slice::from_ref(&item)).unwrap()
    );
    assert_ne!(
        base,
        snapshot_digest(id, "7", at, 2, std::slice::from_ref(&item)).unwrap()
    );
    assert_ne!(
        base,
        snapshot_digest(Uuid::nil(), "7", at, 1, std::slice::from_ref(&item)).unwrap()
    );
    assert_ne!(
        base,
        snapshot_digest(
            id,
            "7",
            datetime!(2026-11-13 12:00:01 UTC),
            1,
            std::slice::from_ref(&item)
        )
        .unwrap()
    );
}

#[test]
fn the_kinds_are_domain_separated() {
    let v = json!({"id": "x"});
    let digests = [
        record_digest(&v),
        definition_digest(&v),
        mapping_digest(&v),
        profile_digest(&v),
        content_digest(&v),
    ];
    let unique: std::collections::BTreeSet<_> = digests.iter().collect();
    assert_eq!(unique.len(), digests.len());
}

#[test]
fn the_package_example_snapshot_has_a_pinned_digest() {
    // Contract fixture shared with adapters: the example's own digest field is a placeholder, so
    // the value below is the canonical digest recomputed from the example's members (quotas
    // DESIGN §3.3.2).
    let example: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../docs/quotas/schemas/quota-capability-discovery.example.json"
    ))
    .unwrap();
    let items = example["items"].as_array().unwrap().clone();
    let observed_at = example["observed_at"].as_str().unwrap();
    let at = OffsetDateTime::parse(observed_at, &Rfc3339).unwrap();
    // The example's own text is the form the digest hashes, so an adapter may hash what it sent.
    assert_eq!(at.format(&Rfc3339).unwrap(), observed_at);
    let digest = snapshot_digest(
        Uuid::parse_str(example["snapshot_id"].as_str().unwrap()).unwrap(),
        example["source_revision"].as_str().unwrap(),
        at,
        example["item_count"].as_u64().unwrap(),
        &items,
    )
    .unwrap();
    assert_eq!(digest, PINNED_EXAMPLE_DIGEST);
}

#[test]
fn an_item_without_a_string_id_is_refused() {
    let at = datetime!(2026-11-13 12:00 UTC);
    assert_eq!(
        snapshot_digest(Uuid::nil(), "7", at, 1, &[json!({"id": 7})]),
        Err(DigestError::ItemWithoutId)
    );
}

#[test]
fn an_instant_outside_the_utc_range_is_refused_not_a_panic() {
    // Valid in its own offset, past the last representable instant once moved to UTC.
    let at = datetime!(9999-12-31 23:30 -01:00);
    assert_eq!(
        snapshot_digest(Uuid::nil(), "7", at, 0, &[]),
        Err(DigestError::InstantOutOfRange)
    );
}

#[test]
fn the_observation_time_is_hashed_as_utc_with_a_fraction_only_when_non_zero() {
    let id = uuid!("9144d0b7-3497-4db9-89e7-69c2c5d8f035");
    let expected = |text: &str| {
        let snapshot = json!({
            "snapshot_id": id.to_string(),
            "source_revision": "7",
            "observed_at": text,
            "item_count": 0,
            "items": [],
        });
        sha256_hex(SNAPSHOT_DOMAIN, &render(&snapshot))
    };
    for (at, text) in [
        (datetime!(2026-11-13 12:00 UTC), "2026-11-13T12:00:00Z"),
        (
            datetime!(2026-11-13 13:00:00.250 +01:00),
            "2026-11-13T12:00:00.25Z",
        ),
        (
            datetime!(2026-11-13 12:00:00.000_000_001 UTC),
            "2026-11-13T12:00:00.000000001Z",
        ),
    ] {
        assert_eq!(
            snapshot_digest(id, "7", at, 0, &[]).unwrap(),
            expected(text),
            "{text}"
        );
    }
}
