//! HTTP integration tests for lazy `OAuth2` acquisition.
//! Timing, cancellation and backoff are covered separately with virtual time.
#![allow(clippy::unwrap_used, clippy::expect_used)]
use httpmock::prelude::*;
use std::time::Duration;
use toolkit_auth::oauth2::{
    HttpOperation, OAuthClientConfig, SecretString, TokenError, TokenProvider,
};
use url::Url;

/// Build a mock-server configuration without transport-level retries.
fn config(server: &MockServer) -> OAuthClientConfig {
    OAuthClientConfig {
        token_endpoint: Some(Url::parse(&server.url("/token")).unwrap()),
        client_id: "client".into(),
        client_secret: SecretString::new("secret"),
        http_config: Some(toolkit_http::HttpClientConfig::for_testing()),
        jitter_max: Duration::ZERO,
        ..Default::default()
    }
}

/// Bound one attempt by a generous deadline without polling the cache.
async fn attempt(token: &TokenProvider) -> Result<SecretString, TokenError> {
    tokio::time::timeout(Duration::from_secs(30), token.get())
        .await
        .expect("acquisition attempt did not finish")
}

/// Wait for recovery, observing completed attempts instead of sleeping or polling.
async fn recovered(token: &TokenProvider) -> SecretString {
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            match token.get().await {
                Ok(value) => return value,
                Err(TokenError::HttpStatus { status, .. })
                    if status == http::StatusCode::SERVICE_UNAVAILABLE => {}
                Err(error) => panic!("unexpected acquisition error: {error}"),
            }
        }
    })
    .await
    .expect("acquisition did not recover")
}

/// Clones share the first exchange and expose its success through the async API.
#[tokio::test]
async fn concurrent_first_waits_share_acquisition() {
    let server = MockServer::start_async().await;
    let mock = server
        .mock_async(|when, then| {
            when.method(POST).path("/token");
            then.status(200)
                .json_body(serde_json::json!({"access_token":"shared", "expires_in":3600}));
        })
        .await;
    let token = TokenProvider::new(config(&server)).unwrap();
    mock.assert_calls_async(0).await;
    let mut tasks = Vec::new();
    for _ in 0..16 {
        let clone = token.clone();
        tasks.push(tokio::spawn(async move { attempt(&clone).await.unwrap() }));
    }
    for task in tasks {
        assert_eq!(task.await.unwrap().expose(), "shared");
    }
    mock.assert_calls_async(1).await;
    let clone = token.clone();
    clone.shutdown().await;
    assert!(matches!(token.get().await, Err(TokenError::Shutdown)));
    assert!(matches!(
        token.invalidate().await,
        Err(TokenError::Shutdown)
    ));
}

/// Discovery failures retain their typed operation and recover without reconstruction.
#[tokio::test]
async fn discovery_failure_recovers_and_resolution_survives_invalidation() {
    let server = MockServer::start_async().await;
    let failure = server
        .mock_async(|when, then| {
            when.method(GET).path("/.well-known/openid-configuration");
            then.status(503);
        })
        .await;
    let mut cfg = config(&server);
    cfg.token_endpoint = None;
    cfg.issuer_url = Some(Url::parse(&server.base_url()).unwrap());
    let token = TokenProvider::new(cfg).unwrap();
    assert!(matches!(
        attempt(&token).await,
        Err(TokenError::HttpStatus {
            operation: HttpOperation::Discovery,
            status: http::StatusCode::SERVICE_UNAVAILABLE,
            target_url,
            ..
        }) if target_url == server.url("/.well-known/openid-configuration")
    ));
    failure.delete_async().await;
    let discovery = server
        .mock_async(|when, then| {
            when.method(GET).path("/.well-known/openid-configuration");
            then.status(200)
                .json_body(serde_json::json!({"token_endpoint":server.url("/token")}));
        })
        .await;
    let exchange = server
        .mock_async(|when, then| {
            when.method(POST).path("/token");
            then.status(200)
                .json_body(serde_json::json!({"access_token":"recovered", "expires_in":3600}));
        })
        .await;
    assert_eq!(recovered(&token).await.expose(), "recovered");
    token.invalidate().await.unwrap();
    discovery.assert_calls_async(1).await;
    exchange.assert_calls_async(2).await;
}

/// Repeated unauthorized responses remain distinguishable from pending acquisition.
#[tokio::test]
async fn unauthorized_attempts_are_visible_to_get() {
    let server = MockServer::start_async().await;
    let _failure = server
        .mock_async(|when, then| {
            when.method(POST).path("/token");
            then.status(401).body("sensitive response diagnostics");
        })
        .await;
    let mut cfg = config(&server);
    cfg.token_endpoint
        .as_mut()
        .unwrap()
        .set_query(Some("diagnostic=secret-query"));
    let token = TokenProvider::new(cfg).unwrap();
    for _ in 0..2 {
        let error = attempt(&token).await.unwrap_err();
        assert!(matches!(
            &error,
            TokenError::HttpStatus {
                operation: HttpOperation::TokenRequest,
                status: http::StatusCode::UNAUTHORIZED,
                target_url,
                ..
            } if target_url == &server.url("/token")
        ));
        assert!(!format!("{error:?} {error}").contains("sensitive"));
        assert!(!format!("{error:?} {error}").contains("secret-query"));
    }
}

/// Token retries reuse successful discovery, including pending invalidation.
#[tokio::test]
async fn token_failure_recovers_without_repeating_discovery() {
    let server = MockServer::start_async().await;
    let discovery = server
        .mock_async(|when, then| {
            when.method(GET).path("/.well-known/openid-configuration");
            then.status(200)
                .json_body(serde_json::json!({"token_endpoint":server.url("/token")}));
        })
        .await;
    let failure = server
        .mock_async(|when, then| {
            when.method(POST).path("/token");
            then.status(503);
        })
        .await;
    let mut cfg = config(&server);
    cfg.token_endpoint = None;
    cfg.issuer_url = Some(Url::parse(&server.base_url()).unwrap());
    let token = TokenProvider::new(cfg).unwrap();
    assert_eq!(
        attempt(&token).await.unwrap_err().status(),
        Some(http::StatusCode::SERVICE_UNAVAILABLE)
    );
    assert_eq!(
        token.invalidate().await.unwrap_err().status(),
        Some(http::StatusCode::SERVICE_UNAVAILABLE)
    );
    failure.delete_async().await;
    let _success = server
        .mock_async(|when, then| {
            when.method(POST).path("/token");
            then.status(200)
                .json_body(serde_json::json!({"access_token":"recovered", "expires_in":3600}));
        })
        .await;
    assert_eq!(recovered(&token).await.expose(), "recovered");
    discovery.assert_calls_async(1).await;
}

/// Invalid credentials configuration is rejected before starting a watcher.
#[tokio::test]
async fn lazy_creation_rejects_invalid_configuration() {
    assert!(matches!(
        TokenProvider::new(OAuthClientConfig::default()),
        Err(TokenError::ConfigError(_))
    ));
}

/// A valid configuration outside Tokio returns an error rather than panicking.
#[test]
fn lazy_creation_without_runtime_returns_error() {
    let cfg = OAuthClientConfig {
        token_endpoint: Some(Url::parse("https://example.com/token").unwrap()),
        client_id: "client".into(),
        client_secret: SecretString::new("secret"),
        ..Default::default()
    };
    assert!(
        matches!(TokenProvider::new(cfg), Err(TokenError::ConfigError(message)) if message.contains("Tokio runtime"))
    );
}
