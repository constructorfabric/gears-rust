use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use serde_json::json;

use super::*;

/// Builds an unsigned (header.payload.) JWT-shaped string for decode-only tests.
fn unsigned_jwt(header: &serde_json::Value, payload: &serde_json::Value) -> String {
    let h = URL_SAFE_NO_PAD.encode(serde_json::to_vec(header).unwrap());
    let p = URL_SAFE_NO_PAD.encode(serde_json::to_vec(payload).unwrap());
    format!("{h}.{p}.")
}

const OBO: &str = "https://core.example.com/issuers/obo";

#[test]
fn obo_issuer_bearer_is_reentry() {
    let jwt = unsigned_jwt(&json!({"typ": "obo+jwt"}), &json!({"iss": OBO}));
    assert!(is_obo_reentry(Some(&jwt), OBO));
}

#[test]
fn other_issuer_bearer_is_not_reentry() {
    let kc = unsigned_jwt(&json!({}), &json!({"iss": "https://kc/realms/x"}));
    assert!(!is_obo_reentry(Some(&kc), OBO));

    // The cap issuer of the same deployment is not the OBO issuer.
    let cap = unsigned_jwt(
        &json!({"typ": "cap+jwt"}),
        &json!({"iss": "https://core.example.com/issuers/cap"}),
    );
    assert!(!is_obo_reentry(Some(&cap), OBO));
}

#[test]
fn missing_bearer_is_not_reentry() {
    assert!(!is_obo_reentry(None, OBO));
}

#[test]
fn malformed_bearer_is_not_reentry() {
    assert!(!is_obo_reentry(Some("not-a-jwt"), OBO));
    assert!(!is_obo_reentry(Some("only.two"), OBO)); // payload is "two" → not base64/json
    assert!(!is_obo_reentry(Some("a.!!!.c"), OBO)); // payload not valid base64url
    // payload missing `iss`.
    let no_iss = unsigned_jwt(&json!({}), &json!({"sub": "x"}));
    assert!(!is_obo_reentry(Some(&no_iss), OBO));
}
