use super::*;
use httpmock::prelude::*;

/// Build a minimal valid config pointing at the mock server.
fn test_config(server: &MockServer) -> OAuthClientConfig {
    OAuthClientConfig {
        token_endpoint: Some(
            Url::parse(&format!("http://localhost:{}/token", server.port())).unwrap(),
        ),
        client_id: "test-client".into(),
        client_secret: SecretString::new("test-secret"),
        http_config: Some(modkit_http::HttpClientConfig::for_testing()),
        ..Default::default()
    }
}

#[tokio::test]
async fn request_token_basic_auth_success() {
    let server = MockServer::start();

    let mock = server.mock(|when, then| {
        when.method(POST)
            .path("/token")
            .header_exists("authorization")
            .body_includes("grant_type=client_credentials");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"access_token":"tok-123","expires_in":3600,"token_type":"Bearer"}"#);
    });

    let mut source = OAuthTokenSource::new(&test_config(&server)).unwrap();
    let token = source.request_token().await.unwrap();

    assert_eq!(token.access_token().as_str(), "tok-123");
    assert_eq!(token.lifetime(), DurationSecs(3600));
    mock.assert();
}

#[tokio::test]
async fn missing_expires_in_uses_default_ttl() {
    let server = MockServer::start();

    let mock = server.mock(|when, then| {
        when.method(POST).path("/token");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"access_token":"tok-456"}"#);
    });

    let mut source = OAuthTokenSource::new(&test_config(&server)).unwrap();
    let token = source.request_token().await.unwrap();

    // default_ttl from OAuthClientConfig::default() is 5 min = 300s
    assert_eq!(token.lifetime(), DurationSecs(300));
    mock.assert();
}

#[tokio::test]
async fn expires_in_zero_honoured() {
    let server = MockServer::start();

    let mock = server.mock(|when, then| {
        when.method(POST).path("/token");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"access_token":"tok-zero","expires_in":0}"#);
    });

    let mut source = OAuthTokenSource::new(&test_config(&server)).unwrap();
    let token = source.request_token().await.unwrap();

    // Server-provided expires_in is honoured as-is; aliri handles refresh scheduling.
    assert_eq!(token.lifetime(), DurationSecs(0));
    mock.assert();
}

#[tokio::test]
async fn unsupported_token_type_returns_error() {
    let server = MockServer::start();

    let mock = server.mock(|when, then| {
        when.method(POST).path("/token");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"access_token":"tok","token_type":"mac"}"#);
    });

    let mut source = OAuthTokenSource::new(&test_config(&server)).unwrap();
    let err = source.request_token().await.unwrap_err();

    assert!(
        matches!(err, TokenError::UnsupportedTokenType(ref t) if t == "mac"),
        "expected UnsupportedTokenType(\"mac\"), got: {err}"
    );
    mock.assert();
}

#[tokio::test]
async fn empty_scopes_omits_scope_param() {
    let server = MockServer::start();

    let mock = server.mock(|when, then| {
        when.method(POST)
            .path("/token")
            .body_includes("grant_type=client_credentials")
            .body_excludes("scope");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"access_token":"tok"}"#);
    });

    let mut source = OAuthTokenSource::new(&test_config(&server)).unwrap();
    source.request_token().await.unwrap();
    mock.assert();
}

#[tokio::test]
async fn scopes_are_space_joined() {
    let server = MockServer::start();

    let mock = server.mock(|when, then| {
        when.method(POST)
            .path("/token")
            .body_includes("scope=read+write");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"access_token":"tok"}"#);
    });

    let mut cfg = test_config(&server);
    cfg.scopes = vec!["read".into(), "write".into()];
    let mut source = OAuthTokenSource::new(&cfg).unwrap();
    source.request_token().await.unwrap();
    mock.assert();
}

#[tokio::test]
async fn basic_auth_header_present() {
    let server = MockServer::start();

    let expected = format!(
        "Basic {}",
        general_purpose::STANDARD.encode("test-client:test-secret")
    );

    let mock = server.mock(|when, then| {
        when.method(POST)
            .path("/token")
            .header("authorization", &expected);
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"access_token":"tok"}"#);
    });

    let mut source = OAuthTokenSource::new(&test_config(&server)).unwrap();
    source.request_token().await.unwrap();
    mock.assert();
}

#[tokio::test]
async fn form_auth_sends_credentials_in_body() {
    let server = MockServer::start();

    let mock = server.mock(|when, then| {
        when.method(POST)
            .path("/token")
            .body_includes("client_id=test-client")
            .body_includes("client_secret=test-secret")
            .body_includes("grant_type=client_credentials");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"access_token":"tok"}"#);
    });

    let mut cfg = test_config(&server);
    cfg.auth_method = ClientAuthMethod::Form;
    let mut source = OAuthTokenSource::new(&cfg).unwrap();
    source.request_token().await.unwrap();
    mock.assert();
}

#[tokio::test]
async fn form_auth_does_not_send_basic_header() {
    let server = MockServer::start();

    // Mock that REQUIRES a Basic auth header — should NOT be hit.
    let basic_mock = server.mock(|when, then| {
        when.method(POST)
            .path("/token")
            .header_exists("authorization");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"access_token":"tok"}"#);
    });

    // Catch-all mock for the POST.
    let fallback_mock = server.mock(|when, then| {
        when.method(POST).path("/token");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"access_token":"tok"}"#);
    });

    let mut cfg = test_config(&server);
    cfg.auth_method = ClientAuthMethod::Form;
    let mut source = OAuthTokenSource::new(&cfg).unwrap();
    source.request_token().await.unwrap();

    assert_eq!(
        basic_mock.calls(),
        0,
        "Form auth must not send Authorization header"
    );
    fallback_mock.assert();
}

#[tokio::test]
async fn extra_headers_are_applied() {
    let server = MockServer::start();

    let mock = server.mock(|when, then| {
        when.method(POST)
            .path("/token")
            .header("x-vendor-key", "vendor-value");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"access_token":"tok"}"#);
    });

    let mut cfg = test_config(&server);
    cfg.extra_headers = vec![("x-vendor-key".into(), "vendor-value".into())];
    let mut source = OAuthTokenSource::new(&cfg).unwrap();
    source.request_token().await.unwrap();
    mock.assert();
}

#[tokio::test]
async fn http_error_mapped_via_format_http_error() {
    let server = MockServer::start();

    let mock = server.mock(|when, then| {
        when.method(POST).path("/token");
        then.status(401)
            .header("content-type", "application/json")
            .body(r#"{"error":"invalid_client"}"#);
    });

    let mut source = OAuthTokenSource::new(&test_config(&server)).unwrap();
    let err = source.request_token().await.unwrap_err();

    assert!(
        matches!(
            err,
            TokenError::Http(ref msg)
                if msg.contains("OAuth2 token")
                    && msg.contains("401")
        ),
        "expected Http error with OAuth2 token prefix and 401, got: {err}"
    );
    mock.assert();
}

#[test]
fn config_error_when_token_endpoint_missing() {
    let cfg = OAuthClientConfig::default();
    let result = OAuthTokenSource::new(&cfg);
    let Err(err) = result else {
        panic!("expected ConfigError, got Ok");
    };
    assert!(
        matches!(err, TokenError::ConfigError(ref msg) if msg.contains("token_endpoint")),
        "expected ConfigError about token_endpoint, got: {err}"
    );
}

#[tokio::test]
async fn bearer_case_insensitive() {
    let server = MockServer::start();

    let mock = server.mock(|when, then| {
        when.method(POST).path("/token");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"access_token":"tok","token_type":"bEaReR"}"#);
    });

    let mut source = OAuthTokenSource::new(&test_config(&server)).unwrap();
    let token = source.request_token().await.unwrap();

    assert_eq!(token.access_token().as_str(), "tok");
    mock.assert();
}

// -- refresh_params -------------------------------------------------------

/// Helper: verify that the aliri formula
/// `max(lifetime * freshness, min_stale) <= lifetime`
/// holds for the given params.
#[allow(clippy::cast_precision_loss)]
fn assert_stale_before_expiry(lifetime: u64, freshness: f64, min_stale: DurationSecs) {
    let delay_a = (lifetime as f64) * freshness;
    let delay_b = min_stale.0 as f64;
    let delay = delay_a.max(delay_b);
    assert!(
        delay <= lifetime as f64,
        "stale ({delay}) must not exceed lifetime ({lifetime})"
    );
}

#[test]
fn refresh_normal_token() {
    // 1-hour token, 30-min offset → stale at 50%
    let (r, ms) = refresh_params(
        3600,
        &Duration::from_secs(30 * 60),
        &Duration::from_secs(10),
    );
    assert!((r - 0.5).abs() < f64::EPSILON);
    assert_eq!(ms, DurationSecs(10));
    assert_stale_before_expiry(3600, r, ms);
}

#[test]
fn refresh_short_lived_token() {
    // 20-min token, 30-min offset → fallback 0.5
    let (r, ms) = refresh_params(
        1200,
        &Duration::from_secs(30 * 60),
        &Duration::from_secs(10),
    );
    assert!((r - 0.5).abs() < f64::EPSILON);
    assert_eq!(ms, DurationSecs(10));
    assert_stale_before_expiry(1200, r, ms);
}

#[test]
fn refresh_equal_lifetime_and_offset() {
    // 30-min token, 30-min offset → fallback 0.5
    let (r, ms) = refresh_params(
        1800,
        &Duration::from_secs(30 * 60),
        &Duration::from_secs(10),
    );
    assert!((r - 0.5).abs() < f64::EPSILON);
    assert_stale_before_expiry(1800, r, ms);
}

#[test]
fn refresh_zero_lifetime() {
    // Both values must be zero so stale == expiry.
    let (r, ms) = refresh_params(0, &Duration::from_secs(30 * 60), &Duration::from_secs(10));
    assert!((r - 0.0).abs() < f64::EPSILON);
    assert_eq!(ms, DurationSecs(0));
}

#[test]
fn refresh_small_offset() {
    // 5-min token, 1-min offset → stale at 80%
    let (r, ms) = refresh_params(300, &Duration::from_secs(60), &Duration::from_secs(10));
    assert!((r - 0.8).abs() < f64::EPSILON);
    assert_eq!(ms, DurationSecs(10));
    assert_stale_before_expiry(300, r, ms);
}

#[test]
fn refresh_zero_offset() {
    // No offset → stale at 100% (only stale when expired)
    let (r, ms) = refresh_params(3600, &Duration::from_secs(0), &Duration::from_secs(10));
    assert!((r - 1.0).abs() < f64::EPSILON);
    assert_eq!(ms, DurationSecs(10));
    assert_stale_before_expiry(3600, r, ms);
}

#[test]
fn refresh_min_period_exceeds_lifetime() {
    // min_refresh_period (600s) > lifetime (300s) — must be capped
    let (r, ms) = refresh_params(300, &Duration::from_secs(60), &Duration::from_secs(600));
    // desired_delay = 300 - 60 = 240
    assert!((r - 0.8).abs() < f64::EPSILON);
    // min_stale capped to desired_delay, not 600
    assert_eq!(ms, DurationSecs(240));
    assert_stale_before_expiry(300, r, ms);
}

#[test]
fn refresh_zero_lifetime_nonzero_min_period() {
    // expires_in=0 with min_refresh_period=10 — both must be zero
    let (r, ms) = refresh_params(0, &Duration::from_secs(30 * 60), &Duration::from_secs(10));
    assert!((r - 0.0).abs() < f64::EPSILON);
    assert_eq!(ms, DurationSecs(0));
}
