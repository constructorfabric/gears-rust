//! One-shot `OAuth2` client credentials token fetch.
//!
//! Use [`fetch_token`] when you need a single token exchange without spawning a
//! background refresh watcher.  This is the right choice for callers that manage
//! their own cache (e.g. an auth plugin with a TTL-based token cache).
//!
//! For long-lived service singletons that benefit from automatic background
//! refresh, use [`TokenProvider`](super::TokenProvider) instead.

use std::fmt;
use std::time::Duration;

use super::config::OAuthClientConfig;
use super::error::TokenError;
use super::source::OAuthTokenSource;
use toolkit_utils::SecretString;

/// Result of a one-shot `OAuth2` client credentials token exchange.
///
/// Contains the bearer token and the server-reported lifetime so callers can
/// set per-entry cache TTLs.
///
/// `Debug` is manually implemented to redact [`bearer`](Self::bearer).
pub struct FetchedToken {
    /// The access token, wrapped in [`SecretString`] for safe handling.
    pub bearer: SecretString,

    /// Token lifetime as reported by the authorization server (`expires_in`),
    /// or the configured [`default_ttl`](OAuthClientConfig::default_ttl) when
    /// the server omits it.
    pub expires_in: Duration,
}

/// `Debug` redacts the bearer value to prevent accidental exposure in logs.
impl fmt::Debug for FetchedToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FetchedToken")
            .field("bearer", &"[REDACTED]")
            .field("expires_in", &self.expires_in)
            .finish()
    }
}

/// Perform a single `OAuth2` client credentials token exchange.
///
/// This function validates the configuration, optionally resolves the token
/// endpoint via OIDC discovery, fetches a token, and returns the bearer value
/// alongside `expires_in` — all without spawning background tasks.
///
/// # Errors
///
/// Returns [`TokenError::ConfigError`] if the configuration is invalid.
/// Returns [`TokenError::Http`] or [`TokenError::HttpStatus`] if the token (or discovery) request fails.
/// Returns [`TokenError::UnsupportedTokenType`] if the server returns a
/// non-Bearer token type.
pub async fn fetch_token(config: OAuthClientConfig) -> Result<FetchedToken, TokenError> {
    config.validate()?;

    let mut source = OAuthTokenSource::new(&config)?;
    let token = source.request_token().await?;

    Ok(FetchedToken {
        bearer: token.access_token,
        expires_in: Duration::from_secs(token.lifetime_secs),
    })
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::super::error::HttpOperation;
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
            http_config: Some(toolkit_http::HttpClientConfig::for_testing()),
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
            http_config: Some(toolkit_http::HttpClientConfig::for_testing()),
            jitter_max: Duration::from_millis(0),
            min_refresh_period: Duration::from_millis(100),
            ..Default::default()
        };

        let fetched = fetch_token(cfg).await.unwrap();
        assert_eq!(fetched.bearer.expose(), "tok-discovered");
        assert_eq!(fetched.expires_in, Duration::from_mins(30));
    }

    /// One-shot acquisition propagates discovery failures.
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
            http_config: Some(toolkit_http::HttpClientConfig::for_testing()),
            ..Default::default()
        };

        let err = fetch_token(cfg).await.unwrap_err();
        assert!(
            matches!(
                err,
                TokenError::HttpStatus { operation: HttpOperation::Discovery, .. } if err.status() == Some(http::StatusCode::INTERNAL_SERVER_ERROR)
            ),
            "expected Http error with OIDC discovery prefix, got: {err}"
        );
    }

    // -----------------------------------------------------------------------
    // TokenProvider fetch
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
        assert_eq!(fetched.expires_in, Duration::from_hours(1));
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
        assert_eq!(fetched.expires_in, Duration::from_mins(5));
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

    /// One-shot acquisition exposes the token endpoint response status.
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
                TokenError::HttpStatus { operation: HttpOperation::TokenRequest, .. } if err.status() == Some(http::StatusCode::INTERNAL_SERVER_ERROR)
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
}
