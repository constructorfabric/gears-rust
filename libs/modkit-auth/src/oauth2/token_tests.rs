use super::*;
use httpmock::prelude::*;
use url::Url;

/// Build a test config pointing at the given mock server.
fn test_config(server: &MockServer) -> OAuthClientConfig {
    OAuthClientConfig {
        token_endpoint: Some(
            Url::parse(&format!("http://localhost:{}/token", server.port())).unwrap(),
        ),
        client_id: "test-client".into(),
        client_secret: SecretString::new("test-secret"),
        http_config: Some(modkit_http::HttpClientConfig::for_testing()),
        // Use short durations for tests.
        jitter_max: Duration::from_millis(0),
        min_refresh_period: Duration::from_millis(100),
        ..Default::default()
    }
}

fn token_json(token: &str, expires_in: u64) -> String {
    format!(r#"{{"access_token":"{token}","expires_in":{expires_in},"token_type":"Bearer"}}"#)
}

// -- trait assertions -----------------------------------------------------

#[test]
fn token_is_send_sync_clone() {
    fn assert_traits<T: Send + Sync + Clone>() {}
    assert_traits::<Token>();
}

// -- new ------------------------------------------------------------------

#[tokio::test]
async fn new_with_valid_config() {
    let server = MockServer::start();

    let _mock = server.mock(|when, then| {
        when.method(POST).path("/token");
        then.status(200)
            .header("content-type", "application/json")
            .body(token_json("tok-new", 3600));
    });

    let token = Token::new(test_config(&server)).await;
    assert!(
        token.is_ok(),
        "Token::new() should succeed: {:?}",
        token.err()
    );
}

#[tokio::test]
async fn new_validates_config() {
    let cfg = OAuthClientConfig {
        token_endpoint: Some(Url::parse("https://a.example.com/token").unwrap()),
        issuer_url: Some(Url::parse("https://b.example.com").unwrap()),
        client_id: "test-client".into(),
        client_secret: SecretString::new("test-secret"),
        ..Default::default()
    };
    let err = Token::new(cfg).await.unwrap_err();
    assert!(
        matches!(err, TokenError::ConfigError(ref msg) if msg.contains("mutually exclusive")),
        "expected ConfigError, got: {err}"
    );
}

// -- get ------------------------------------------------------------------

#[tokio::test]
async fn get_returns_secret_string() {
    let server = MockServer::start();

    let _mock = server.mock(|when, then| {
        when.method(POST).path("/token");
        then.status(200)
            .header("content-type", "application/json")
            .body(token_json("tok-get-test", 3600));
    });

    let token = Token::new(test_config(&server)).await.unwrap();
    let secret = token.get().unwrap();

    assert_eq!(secret.expose(), "tok-get-test");
}

// -- invalidate -----------------------------------------------------------

#[tokio::test]
async fn invalidate_creates_new_watcher() {
    let server = MockServer::start();

    let mock = server.mock(|when, then| {
        when.method(POST).path("/token");
        then.status(200)
            .header("content-type", "application/json")
            .body(token_json("tok-inv", 3600));
    });

    let token = Token::new(test_config(&server)).await.unwrap();
    assert_eq!(mock.calls(), 1, "initial fetch");

    token.invalidate().await;

    // invalidate spawns a new watcher which fetches a fresh token
    assert_eq!(mock.calls(), 2, "after invalidate");
}

// -- concurrency ----------------------------------------------------------

#[tokio::test]
async fn concurrent_get_no_deadlock() {
    let server = MockServer::start();

    let _mock = server.mock(|when, then| {
        when.method(POST).path("/token");
        then.status(200)
            .header("content-type", "application/json")
            .body(token_json("tok-conc", 3600));
    });

    let token = Token::new(test_config(&server)).await.unwrap();

    let t1 = {
        let token = token.clone();
        tokio::spawn(async move { token.get() })
    };
    let t2 = {
        let token = token.clone();
        tokio::spawn(async move { token.get() })
    };

    let (r1, r2) = tokio::join!(t1, t2);
    assert!(r1.unwrap().is_ok());
    assert!(r2.unwrap().is_ok());
}

// -- OIDC discovery -------------------------------------------------------

#[tokio::test]
async fn new_with_issuer_url_discovery() {
    let server = MockServer::start();

    // Mock the OIDC discovery endpoint.
    let token_ep = format!("http://localhost:{}/oauth/token", server.port());
    let _discovery_mock = server.mock(|when, then| {
        when.method(GET).path("/.well-known/openid-configuration");
        then.status(200)
            .header("content-type", "application/json")
            .body(format!(r#"{{"token_endpoint":"{token_ep}"}}"#));
    });

    // Mock the resolved token endpoint.
    let _token_mock = server.mock(|when, then| {
        when.method(POST).path("/oauth/token");
        then.status(200)
            .header("content-type", "application/json")
            .body(token_json("tok-discovered", 3600));
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

    let token = Token::new(cfg).await.unwrap();
    let secret = token.get().unwrap();
    assert_eq!(secret.expose(), "tok-discovered");
}

#[tokio::test]
async fn discovery_not_repeated_on_invalidate() {
    let server = MockServer::start();

    // Mock the OIDC discovery endpoint.
    let token_ep = format!("http://localhost:{}/oauth/token", server.port());
    let discovery_mock = server.mock(|when, then| {
        when.method(GET).path("/.well-known/openid-configuration");
        then.status(200)
            .header("content-type", "application/json")
            .body(format!(r#"{{"token_endpoint":"{token_ep}"}}"#));
    });

    // Mock the resolved token endpoint.
    let token_mock = server.mock(|when, then| {
        when.method(POST).path("/oauth/token");
        then.status(200)
            .header("content-type", "application/json")
            .body(token_json("tok-disc-inv", 3600));
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

    let token = Token::new(cfg).await.unwrap();
    assert_eq!(discovery_mock.calls(), 1, "discovery: initial");
    assert_eq!(token_mock.calls(), 1, "token: initial");

    // Invalidate should re-fetch the token but NOT re-run discovery.
    token.invalidate().await;

    assert_eq!(
        discovery_mock.calls(),
        1,
        "discovery must NOT be repeated on invalidate"
    );
    assert_eq!(token_mock.calls(), 2, "token: after invalidate");
}

// -- debug safety ---------------------------------------------------------

#[tokio::test]
async fn debug_does_not_reveal_tokens() {
    let server = MockServer::start();

    let _mock = server.mock(|when, then| {
        when.method(POST).path("/token");
        then.status(200)
            .header("content-type", "application/json")
            .body(token_json("super-secret-tok", 3600));
    });

    let token = Token::new(test_config(&server)).await.unwrap();
    let dbg = format!("{token:?}");
    assert!(
        !dbg.contains("super-secret-tok"),
        "Debug must not reveal token value: {dbg}"
    );
}
