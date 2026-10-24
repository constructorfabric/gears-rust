#![allow(clippy::expect_used, clippy::unwrap_used)]

use serde_json::json;

use super::*;

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
