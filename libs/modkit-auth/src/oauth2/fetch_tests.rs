use super::*;
use httpmock::prelude::*;
use url::Url;

use super::super::types::ClientAuthMethod;

// -----------------------------------------------------------------------
// Helpers
// -----------------------------------------------------------------------

fn test_config(server: &MockServer) -> OAuthClientConfig {
    OAuthClientConfig {
        token_endpoint: Some(
            Url::parse(&format!("http://localhost:{}/token", server.port())).unwrap(),
        ),
        client_id: "test-client".into(),
        client_secret: SecretString::new("test-secret"),
        http_config: Some(modkit_http::HttpClientConfig::for_testing()),
        jitter_max: Duration::from_millis(0),
        min_refresh_period: Duration::from_millis(100),
        ..Default::default()
    }
}

fn token_json(token: &str, expires_in: u64) -> String {
    format!(r#"{{"access_token":"{token}","expires_in":{expires_in},"token_type":"Bearer"}}"#)
}

// -----------------------------------------------------------------------
// Config validation
// -----------------------------------------------------------------------

#[tokio::test]
async fn config_validated_before_fetch() {
    let cfg = OAuthClientConfig {
        token_endpoint: Some(Url::parse("https://a.example.com/token").unwrap()),
        issuer_url: Some(Url::parse("https://b.example.com").unwrap()),
        client_id: "test-client".into(),
        client_secret: SecretString::new("test-secret"),
        ..Default::default()
    };

    let err = fetch_token(cfg).await.unwrap_err();
    assert!(
        matches!(err, TokenError::ConfigError(ref msg) if msg.contains("mutually exclusive")),
        "expected ConfigError, got: {err}"
    );
}

// -----------------------------------------------------------------------
// OIDC discovery
// -----------------------------------------------------------------------

#[tokio::test]
async fn fetch_with_issuer_url_discovery() {
    let server = MockServer::start();

    let token_ep = format!("http://localhost:{}/oauth/token", server.port());
    let _discovery_mock = server.mock(|when, then| {
        when.method(GET).path("/.well-known/openid-configuration");
        then.status(200)
            .header("content-type", "application/json")
            .body(format!(r#"{{"token_endpoint":"{token_ep}"}}"#));
    });

    let _token_mock = server.mock(|when, then| {
        when.method(POST).path("/oauth/token");
        then.status(200)
            .header("content-type", "application/json")
            .body(token_json("tok-discovered", 1800));
    });

    let cfg = OAuthClientConfig {
        issuer_url: Some(Url::parse(&format!("http://localhost:{}", server.port())).unwrap()),
        client_id: "test-client".into(),
        client_secret: SecretString::new("test-secret"),
        http_config: Some(modkit_http::HttpClientConfig::for_testing()),
        jitter_max: Duration::from_millis(0),
        min_refresh_period: Duration::from_millis(100),
        ..Default::default()
    };

    let fetched = fetch_token(cfg).await.unwrap();
    assert_eq!(fetched.bearer.expose(), "tok-discovered");
    assert_eq!(fetched.expires_in, Duration::from_secs(1800));
}

#[tokio::test]
async fn discovery_failure_returns_error() {
    let server = MockServer::start();

    let _mock = server.mock(|when, then| {
        when.method(GET).path("/.well-known/openid-configuration");
        then.status(500).body("internal server error");
    });

    let cfg = OAuthClientConfig {
        issuer_url: Some(Url::parse(&format!("http://localhost:{}", server.port())).unwrap()),
        client_id: "test-client".into(),
        client_secret: SecretString::new("test-secret"),
        http_config: Some(modkit_http::HttpClientConfig::for_testing()),
        ..Default::default()
    };

    let err = fetch_token(cfg).await.unwrap_err();
    assert!(
        matches!(
            err,
            TokenError::Http(ref msg) if msg.contains("OIDC discovery") && msg.contains("500")
        ),
        "expected Http error with OIDC discovery prefix, got: {err}"
    );
}

// -----------------------------------------------------------------------
// Token fetch
// -----------------------------------------------------------------------

#[tokio::test]
async fn fetch_returns_bearer_and_expires_in() {
    let server = MockServer::start();

    let _mock = server.mock(|when, then| {
        when.method(POST).path("/token");
        then.status(200)
            .header("content-type", "application/json")
            .body(token_json("tok-happy", 3600));
    });

    let fetched = fetch_token(test_config(&server)).await.unwrap();
    assert_eq!(fetched.bearer.expose(), "tok-happy");
    assert_eq!(fetched.expires_in, Duration::from_secs(3600));
}

#[tokio::test]
async fn missing_expires_in_uses_default_ttl() {
    let server = MockServer::start();

    let _mock = server.mock(|when, then| {
        when.method(POST).path("/token");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"access_token":"tok-default"}"#);
    });

    let fetched = fetch_token(test_config(&server)).await.unwrap();
    assert_eq!(fetched.bearer.expose(), "tok-default");
    // default_ttl from OAuthClientConfig::default() is 5 min = 300s
    assert_eq!(fetched.expires_in, Duration::from_secs(300));
}

#[tokio::test]
async fn expires_in_zero_returns_zero_duration() {
    let server = MockServer::start();

    let _mock = server.mock(|when, then| {
        when.method(POST).path("/token");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"access_token":"tok-zero","expires_in":0}"#);
    });

    let fetched = fetch_token(test_config(&server)).await.unwrap();
    assert_eq!(fetched.expires_in, Duration::ZERO);
}

#[tokio::test]
async fn http_error_returns_token_error() {
    let server = MockServer::start();

    let _mock = server.mock(|when, then| {
        when.method(POST).path("/token");
        then.status(500).body("internal server error");
    });

    let err = fetch_token(test_config(&server)).await.unwrap_err();
    assert!(
        matches!(
            err,
            TokenError::Http(ref msg) if msg.contains("OAuth2 token") && msg.contains("500")
        ),
        "expected Http error, got: {err}"
    );
}

#[tokio::test]
async fn unsupported_token_type_returns_error() {
    let server = MockServer::start();

    let _mock = server.mock(|when, then| {
        when.method(POST).path("/token");
        then.status(200)
            .header("content-type", "application/json")
            .body(r#"{"access_token":"tok","token_type":"mac"}"#);
    });

    let err = fetch_token(test_config(&server)).await.unwrap_err();
    assert!(
        matches!(err, TokenError::UnsupportedTokenType(ref t) if t == "mac"),
        "expected UnsupportedTokenType(\"mac\"), got: {err}"
    );
}

// -----------------------------------------------------------------------
// Security
// -----------------------------------------------------------------------

#[tokio::test]
async fn debug_does_not_reveal_bearer() {
    let server = MockServer::start();

    let _mock = server.mock(|when, then| {
        when.method(POST).path("/token");
        then.status(200)
            .header("content-type", "application/json")
            .body(token_json("super-secret-bearer", 3600));
    });

    let fetched = fetch_token(test_config(&server)).await.unwrap();
    let dbg = format!("{fetched:?}");
    assert!(
        !dbg.contains("super-secret-bearer"),
        "Debug must not reveal bearer value: {dbg}"
    );
    assert!(dbg.contains("[REDACTED]"), "Debug must contain [REDACTED]");
}

// -----------------------------------------------------------------------
// Auth methods
// -----------------------------------------------------------------------

#[tokio::test]
async fn form_auth_sends_credentials_in_body() {
    let server = MockServer::start();

    let mock = server.mock(|when, then| {
        when.method(POST)
            .path("/token")
            .body_includes("client_id=test-client")
            .body_includes("client_secret=test-secret");
        then.status(200)
            .header("content-type", "application/json")
            .body(token_json("tok-form", 3600));
    });

    let mut cfg = test_config(&server);
    cfg.auth_method = ClientAuthMethod::Form;
    fetch_token(cfg).await.unwrap();
    mock.assert();
}

#[tokio::test]
async fn basic_auth_sends_credentials_in_header() {
    let server = MockServer::start();

    // base64("test-client:test-secret") = "dGVzdC1jbGllbnQ6dGVzdC1zZWNyZXQ="
    let mock = server.mock(|when, then| {
        when.method(POST)
            .path("/token")
            .header("authorization", "Basic dGVzdC1jbGllbnQ6dGVzdC1zZWNyZXQ=");
        then.status(200)
            .header("content-type", "application/json")
            .body(token_json("tok-basic", 3600));
    });

    let cfg = test_config(&server);
    // Default auth_method is Basic.
    fetch_token(cfg).await.unwrap();
    mock.assert();
}
