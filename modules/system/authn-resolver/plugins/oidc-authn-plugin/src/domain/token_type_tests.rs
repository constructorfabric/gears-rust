use super::*;
use base64::Engine as _;

/// Build a base64url-encoded segment from raw JSON at runtime so that
/// no `eyJ...` literals appear in source (avoids secret-scanner false
/// positives).
fn b64(json: &str) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(json)
}

/// Build a three-segment JWT-shaped string from raw JSON header/payload.
fn fake_jwt(header_json: &str, payload_json: &str, sig: &str) -> String {
    format!("{}.{}.{sig}", b64(header_json), b64(payload_json))
}

#[test]
fn test_jwt_detected_three_segments() {
    let token = fake_jwt(r#"{"alg":"RS256"}"#, r#"{"sub":"u"}"#, "signature");
    assert_eq!(detect_token_type(&token), TokenType::Jwt);
}

#[test]
fn test_opaque_no_dots() {
    assert_eq!(detect_token_type("opaque-token-abc123"), TokenType::Opaque);
}

#[test]
fn test_opaque_two_dots() {
    assert_eq!(detect_token_type("a.b"), TokenType::Opaque);
}

#[test]
fn test_opaque_four_dots() {
    assert_eq!(detect_token_type("a.b.c.d"), TokenType::Opaque);
}

#[test]
fn test_jwt_empty_signature_segment() {
    // Unsecured JWT (`alg:none`) uses empty signature segment but still
    // has a valid JOSE header prefix.
    let token = fake_jwt(r#"{"alg":"none"}"#, r#"{"sub":"test"}"#, "");
    assert_eq!(detect_token_type(&token), TokenType::Jwt);
}

#[test]
fn test_opaque_three_segments_without_eyj_prefix() {
    // Three-segment tokens whose header is not a base64url-encoded JSON
    // object are classified as opaque (filters garbage like abc.def.ghi).
    assert_eq!(detect_token_type("abc.def.ghi"), TokenType::Opaque);
}

#[test]
fn test_opaque_empty_string() {
    assert_eq!(detect_token_type(""), TokenType::Opaque);
}

#[test]
fn test_jwt_realistic_token() {
    let token = fake_jwt(r#"{"alg":"RS256"}"#, r#"{"sub":"test"}"#, "AAAA");
    assert_eq!(detect_token_type(&token), TokenType::Jwt);
}

#[test]
fn test_opaque_empty_header_segment() {
    assert_eq!(detect_token_type(".b.c"), TokenType::Opaque);
}

#[test]
fn test_opaque_empty_payload_segment() {
    assert_eq!(detect_token_type("a..c"), TokenType::Opaque);
}
