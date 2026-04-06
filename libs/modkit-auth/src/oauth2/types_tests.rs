use super::*;

#[test]
fn default_auth_method_is_basic() {
    assert_eq!(ClientAuthMethod::default(), ClientAuthMethod::Basic);
}

#[test]
fn deserialize_full_response() {
    let json = r#"{"access_token":"tok","expires_in":3600,"token_type":"Bearer"}"#;
    let r: TokenResponse = serde_json::from_str(json).unwrap();
    assert_eq!(r.access_token, "tok");
    assert_eq!(r.expires_in, Some(3600));
    assert_eq!(r.token_type.as_deref(), Some("Bearer"));
}

#[test]
fn deserialize_minimal_response() {
    let json = r#"{"access_token":"tok"}"#;
    let r: TokenResponse = serde_json::from_str(json).unwrap();
    assert_eq!(r.access_token, "tok");
    assert!(r.expires_in.is_none());
    assert!(r.token_type.is_none());
}

#[test]
fn deserialize_ignores_unknown_fields() {
    let json = r#"{"access_token":"tok","scope":"read","refresh_token":"rt"}"#;
    let r: TokenResponse = serde_json::from_str(json).unwrap();
    assert_eq!(r.access_token, "tok");
}
