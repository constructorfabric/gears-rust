// Created: 2026-09-23 by Virtuozzo International GmbH
//! The unverified payload reader: the compact shape and the size come before
//! any decoding, and anything else is simply not a JWT.

use base64::Engine as _;
use serde_json::{Value, json};

use super::{MAX_PAYLOAD_BYTES, MAX_TOKEN_BYTES, unverified_payload};

fn segment(claims: &Value) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(serde_json::to_vec(claims).expect("json"))
}

#[test]
fn a_compact_jwt_yields_its_payload() {
    let token = format!("h.{}.s", segment(&json!({ "sub": "me" })));
    let sub = unverified_payload(&token).and_then(|p| p["sub"].as_str().map(str::to_owned));
    assert_eq!(sub, Some("me".to_owned()));
}

#[test]
fn anything_but_three_segments_is_not_a_jwt() {
    let payload = segment(&json!({ "sub": "me" }));
    assert!(
        unverified_payload(&format!("h.{payload}")).is_none(),
        "two segments"
    );
    assert!(
        unverified_payload(&format!("h.{payload}.s.extra")).is_none(),
        "four segments"
    );
    assert!(unverified_payload(&payload).is_none(), "no dots at all");
}

#[test]
fn an_oversized_token_or_payload_is_not_decoded() {
    let padded = segment(&json!({ "pad": "x".repeat(MAX_PAYLOAD_BYTES) }));
    assert!(padded.len() > MAX_PAYLOAD_BYTES);
    assert!(
        unverified_payload(&format!("h.{padded}.s")).is_none(),
        "a payload over the cap"
    );

    let payload = segment(&json!({ "sub": "me" }));
    let long_signature = "s".repeat(MAX_TOKEN_BYTES);
    assert!(
        unverified_payload(&format!("h.{payload}.{long_signature}")).is_none(),
        "a token over the cap, whatever segment carries the bulk"
    );
}
