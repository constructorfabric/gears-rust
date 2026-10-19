#![allow(clippy::expect_used, clippy::unwrap_used)]

use proptest::prelude::*;
use serde_json::{Value, json};

use super::{Sha256Hex, canonical_json, push_json_string};

#[test]
fn the_digest_is_lowercase_hex_sha256() {
    // SHA-256("abc"), FIPS 180-2 appendix B.1.
    assert_eq!(
        Sha256Hex::of_bytes(b"abc").as_str(),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
}

#[test]
fn member_order_does_not_change_the_digest() {
    let a = json!({"b": 1, "a": {"y": [1, 2], "x": "s"}});
    let b = json!({"a": {"x": "s", "y": [1, 2]}, "b": 1});
    assert_eq!(Sha256Hex::of_json(&a), Sha256Hex::of_json(&b));
    let c = json!({"a": {"x": "s", "y": [2, 1]}, "b": 1});
    assert_ne!(Sha256Hex::of_json(&a), Sha256Hex::of_json(&c));
}

#[test]
fn the_canonical_text_and_its_digest_are_pinned() {
    // A redelivered fact is a duplicate only if this text never changes: members sorted by key
    // bytes at every depth, arrays in order, serde_json's escaping and number text.
    let payload = json!({
        "b": [1, -2, 1.5, true, null, "x\ny"],
        "a\"q": {"\u{fc}": "\u{e9}", "z": {}},
        "A": 0
    });
    let mut text = String::new();
    canonical_json(&payload, &mut text);
    assert_eq!(
        text,
        "{\"A\":0,\"a\\\"q\":{\"z\":{},\"\u{fc}\":\"\u{e9}\"},\"b\":[1,-2,1.5,true,null,\"x\\ny\"]}"
    );
    assert_eq!(
        Sha256Hex::of_json(&payload).as_str(),
        "bdb6add51829eec23108a234c5f763e584737f91bddbe20dab0795d8d100b940"
    );
}

#[test]
fn only_lowercase_hex_of_sixty_four_parses() {
    let digest = Sha256Hex::of_bytes(b"x");
    assert_eq!(Sha256Hex::parse(digest.as_str()).unwrap(), digest);
    assert!(Sha256Hex::parse(&digest.as_str().to_uppercase()).is_err());
    assert!(Sha256Hex::parse("abc").is_err());
    assert!(Sha256Hex::parse(&"g".repeat(64)).is_err());
}

/// Any string, control characters and non-ASCII included.
fn any_text() -> impl Strategy<Value = String> {
    proptest::collection::vec(any::<char>(), 0..24).prop_map(String::from_iter)
}

/// Any JSON value: every scalar form (floats short enough to read back exactly), nested arrays
/// and objects with arbitrary keys.
fn any_json() -> impl Strategy<Value = Value> {
    let leaf = prop_oneof![
        Just(Value::Null),
        any::<bool>().prop_map(Value::from),
        any::<i64>().prop_map(Value::from),
        any::<u64>().prop_map(Value::from),
        (any::<i16>(), 0_u32..8)
            .prop_map(|(n, k)| Value::from(f64::from(n) / f64::from(1_u16 << k))),
        any_text().prop_map(Value::from),
    ];
    leaf.prop_recursive(4, 48, 6, |inner| {
        prop_oneof![
            proptest::collection::vec(inner.clone(), 0..6).prop_map(Value::Array),
            proptest::collection::vec((any_text(), inner), 0..6)
                .prop_map(|members| Value::Object(members.into_iter().collect())),
        ]
    })
}

/// `value` with the members of every object, at every depth, in reverse order.
fn reversed(value: &Value) -> Value {
    match value {
        Value::Object(map) => Value::Object(
            map.iter()
                .rev()
                .map(|(key, member)| (key.clone(), reversed(member)))
                .collect(),
        ),
        Value::Array(items) => Value::Array(items.iter().map(reversed).collect()),
        scalar => scalar.clone(),
    }
}

fn canonical(value: &Value) -> String {
    let mut text = String::new();
    canonical_json(value, &mut text);
    text
}

proptest! {
    #[test]
    fn a_string_is_escaped_as_serde_json_escapes_it(text in any_text()) {
        let mut escaped = String::new();
        push_json_string(&text, &mut escaped);
        prop_assert_eq!(escaped, Value::String(text).to_string());
    }

    #[test]
    fn member_order_never_changes_the_canonical_text(value in any_json()) {
        prop_assert_eq!(canonical(&value), canonical(&reversed(&value)));
    }

    #[test]
    fn the_canonical_text_reads_back_as_the_same_value(value in any_json()) {
        // Two values with one canonical text would read back as one value: the text, and so the
        // digest, tells every pair of different payloads apart.
        let read: Value = serde_json::from_str(&canonical(&value)).unwrap();
        prop_assert_eq!(read, value);
    }
}
