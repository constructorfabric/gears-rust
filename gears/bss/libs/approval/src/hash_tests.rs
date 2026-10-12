#![allow(clippy::expect_used, clippy::unwrap_used)]
use super::snapshot_hash;
use crate::model::ItemRef;
use uuid::Uuid;

fn item(id: Uuid, after: serde_json::Value) -> ItemRef {
    ItemRef {
        item_type: "price".into(),
        item_id: id,
        created_by: Uuid::nil(),
        before: None,
        after,
    }
}

#[test]
fn the_hash_ignores_item_order_and_key_order_and_is_64_hex() {
    let a = Uuid::new_v4();
    let b = Uuid::new_v4();
    let x = vec![
        item(a, serde_json::json!({"amount": 10, "model": "flat"})),
        item(b, serde_json::json!({"model": "flat", "amount": 20})),
    ];
    let y = vec![
        item(b, serde_json::json!({"amount": 20, "model": "flat"})),
        item(a, serde_json::json!({"model": "flat", "amount": 10})),
    ];
    assert_eq!(snapshot_hash(&x, None), snapshot_hash(&y, None));
    assert_eq!(snapshot_hash(&x, None).len(), 64);
    assert!(
        snapshot_hash(&x, None)
            .chars()
            .all(|c| c.is_ascii_hexdigit())
    );
}
#[test]
fn a_changed_amount_or_date_changes_the_hash() {
    let a = Uuid::new_v4();
    let base = vec![item(a, serde_json::json!({"amount": 10}))];
    let changed = vec![item(a, serde_json::json!({"amount": 11}))];
    assert_ne!(snapshot_hash(&base, None), snapshot_hash(&changed, None));
    assert_ne!(
        snapshot_hash(&base, None),
        snapshot_hash(&base, Some(time::macros::date!(2026 - 10 - 01)))
    );
}
#[test]
fn authorship_and_before_are_not_part_of_the_fingerprint() {
    let a = Uuid::new_v4();
    let mut x = item(a, serde_json::json!({"amount": 10}));
    x.created_by = Uuid::new_v4();
    x.before = Some(serde_json::json!({"amount": 9}));
    assert_eq!(
        snapshot_hash(&[x], None),
        snapshot_hash(&[item(a, serde_json::json!({"amount": 10}))], None)
    );
}

/// Keys in any order, nested ones included, give one fingerprint. The crate's dev-dependencies
/// turn on `serde_json`'s `preserve_order`, as the gears' builds do, so a `json!` object keeps the
/// order it is written in and only the canonical form sorts it.
#[test]
fn one_flat_price_fingerprint_is_pinned() {
    let id = Uuid::parse_str("11111111-1111-1111-1111-111111111111").unwrap();
    let hash = snapshot_hash(
        &[item(id, serde_json::json!({"amount": 10, "model": "flat"}))],
        Some(time::macros::date!(2026 - 10 - 01)),
    );
    assert_eq!(
        hash,
        "f93205e840daa8ee2803709594b4b4a70034c85ee2fb87a3d3fbf62087f3abc8"
    );
}

#[test]
fn the_hash_ignores_nested_key_order() {
    let a = Uuid::new_v4();
    let x = item(
        a,
        serde_json::json!({"price": {"rate": "0.10", "tiers": [{"up_to": 10, "rate": "1"}]}, "model": "flat"}),
    );
    let y = item(
        a,
        serde_json::json!({"model": "flat", "price": {"tiers": [{"rate": "1", "up_to": 10}], "rate": "0.10"}}),
    );
    assert_ne!(
        serde_json::to_string(&x.after).unwrap(),
        serde_json::to_string(&y.after).unwrap(),
        "the two objects keep their written key order"
    );
    assert_eq!(snapshot_hash(&[x], None), snapshot_hash(&[y], None));
}
