use super::*;

fn test_url(s: &str) -> Url {
    Url::parse(s).unwrap()
}

// ---- validate -----------------------------------------------------------

/// Returns a minimal valid config (credentials + one endpoint).
fn valid_base() -> OAuthClientConfig {
    OAuthClientConfig {
        client_id: "my-client".into(),
        client_secret: SecretString::new("my-secret"),
        ..Default::default()
    }
}

#[test]
fn validate_ok_with_token_endpoint_only() {
    let cfg = OAuthClientConfig {
        token_endpoint: Some(test_url("https://auth.example.com/token")),
        ..valid_base()
    };
    assert!(cfg.validate().is_ok());
}

#[test]
fn validate_ok_with_issuer_url_only() {
    let cfg = OAuthClientConfig {
        issuer_url: Some(test_url("https://auth.example.com")),
        ..valid_base()
    };
    assert!(cfg.validate().is_ok());
}

#[test]
fn validate_err_when_both_set() {
    let cfg = OAuthClientConfig {
        token_endpoint: Some(test_url("https://a.example.com/token")),
        issuer_url: Some(test_url("https://b.example.com")),
        ..valid_base()
    };
    let err = cfg.validate().unwrap_err();
    assert!(
        err.to_string().contains("mutually exclusive"),
        "unexpected error: {err}"
    );
}

#[test]
fn validate_err_when_neither_set() {
    let cfg = valid_base();
    let err = cfg.validate().unwrap_err();
    assert!(
        err.to_string().contains("must be set"),
        "unexpected error: {err}"
    );
}

#[test]
fn validate_err_when_client_id_empty() {
    let cfg = OAuthClientConfig {
        token_endpoint: Some(test_url("https://auth.example.com/token")),
        client_id: String::new(),
        client_secret: SecretString::new("my-secret"),
        ..Default::default()
    };
    let err = cfg.validate().unwrap_err();
    assert!(
        err.to_string().contains("client_id"),
        "unexpected error: {err}"
    );
}

#[test]
fn validate_err_when_client_id_whitespace() {
    let cfg = OAuthClientConfig {
        token_endpoint: Some(test_url("https://auth.example.com/token")),
        client_id: "   ".into(),
        client_secret: SecretString::new("my-secret"),
        ..Default::default()
    };
    let err = cfg.validate().unwrap_err();
    assert!(
        err.to_string().contains("client_id"),
        "unexpected error: {err}"
    );
}

#[test]
fn validate_err_when_client_secret_empty() {
    let cfg = OAuthClientConfig {
        token_endpoint: Some(test_url("https://auth.example.com/token")),
        client_id: "my-client".into(),
        client_secret: SecretString::new(""),
        ..Default::default()
    };
    let err = cfg.validate().unwrap_err();
    assert!(
        err.to_string().contains("client_secret"),
        "unexpected error: {err}"
    );
}

// ---- Debug redaction ----------------------------------------------------

#[test]
fn debug_redacts_client_secret() {
    let cfg = OAuthClientConfig {
        token_endpoint: Some(test_url("https://auth.example.com/token")),
        client_id: "my-client".into(),
        client_secret: SecretString::new("super-secret"),
        ..Default::default()
    };
    let dbg = format!("{cfg:?}");
    assert!(dbg.contains("[REDACTED]"), "Debug must contain [REDACTED]");
    assert!(
        !dbg.contains("super-secret"),
        "Debug must not contain the raw secret"
    );
    assert!(dbg.contains("my-client"), "Debug should contain client_id");
}

#[test]
fn debug_redacts_extra_header_values() {
    let cfg = OAuthClientConfig {
        token_endpoint: Some(test_url("https://auth.example.com/token")),
        client_id: "my-client".into(),
        client_secret: SecretString::new("s"),
        extra_headers: vec![("x-api-key".into(), "secret-api-key-value".into())],
        ..Default::default()
    };
    let dbg = format!("{cfg:?}");
    assert!(
        dbg.contains("x-api-key"),
        "Debug should contain header name"
    );
    assert!(
        !dbg.contains("secret-api-key-value"),
        "Debug must not contain header value"
    );
}

// ---- Default ------------------------------------------------------------

#[test]
fn default_durations() {
    let cfg = OAuthClientConfig::default();
    assert_eq!(cfg.refresh_offset, Duration::from_secs(30 * 60));
    assert_eq!(cfg.jitter_max, Duration::from_secs(5 * 60));
    assert_eq!(cfg.min_refresh_period, Duration::from_secs(10));
    assert_eq!(cfg.default_ttl, Duration::from_secs(5 * 60));
    assert_eq!(cfg.auth_method, ClientAuthMethod::Basic);
}
