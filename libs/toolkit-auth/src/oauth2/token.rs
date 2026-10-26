use std::fmt;
use std::sync::Arc;

use super::config::OAuthClientConfig;
use super::error::TokenError;
use super::source::OAuthTokenSource;
use super::token_watcher::{TokenWatcher, WatcherConfig};
use toolkit_utils::SecretString;

/// Shared provider for acquisition, caching, refresh and invalidation of bearer tokens.
/// Clones share one worker. Dropping the final handle cancels it; use
/// [`Self::shutdown`] to also wait for completion.
#[derive(Clone)]
pub struct TokenProvider {
    inner: Arc<TokenWatcher>,
}

impl fmt::Debug for TokenProvider {
    /// Omit credentials and cached bearer values.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TokenProvider").finish_non_exhaustive()
    }
}

impl TokenProvider {
    /// Validate configuration and create an idle provider without network requests.
    /// Call [`Self::get`] during startup when an initial token is required.
    ///
    /// # Errors
    /// Returns configuration/client construction errors, including `ConfigError`
    /// when there is no active Tokio runtime.
    #[allow(clippy::needless_pass_by_value)] // Consume the credential configuration.
    pub fn new(config: OAuthClientConfig) -> Result<Self, TokenError> {
        tokio::runtime::Handle::try_current().map_err(|_| {
            TokenError::ConfigError("TokenProvider::new requires an active Tokio runtime".into())
        })?;
        config.validate()?;
        let policy = WatcherConfig::new(config.jitter_max, config.min_refresh_period)?;
        let source = OAuthTokenSource::new(&config)?;
        Ok(Self {
            inner: Arc::new(TokenWatcher::new(source, policy)),
        })
    }

    /// Return a valid cached token or await one shared acquisition attempt.
    /// Initial acquisition starts on first use. During backoff, callers await
    /// the next permitted attempt. Cancelling a caller only cancels its wait;
    /// shared acquisition continues. Callers may apply their own timeout.
    ///
    /// A success invalidated or expired before delivery waits for replacement.
    ///
    /// # Errors
    /// Returns the observed attempt's error or [`TokenError::Shutdown`] once shutdown begins.
    pub async fn get(&self) -> Result<SecretString, TokenError> {
        self.inner.get().await
    }

    /// Discard the cached token and wait for its replacement.
    /// Concurrent invalidations and gets share acquisition. A failed replacement
    /// does not restore the discarded token. An already pending attempt keeps
    /// its retry schedule; invalidation does not bypass backoff on an empty cache.
    ///
    /// # Errors
    /// Returns the replacement attempt's error or [`TokenError::Shutdown`].
    pub async fn invalidate(&self) -> Result<(), TokenError> {
        self.inner.invalidate().await
    }

    /// Permanently stop this provider and all clones, waking waiters with
    /// [`TokenError::Shutdown`], and wait for acquisition/refresh to finish stopping.
    /// Repeated/concurrent calls are safe; cancelling one shutdown wait does not
    /// undo cancellation or prevent another caller from joining the task.
    pub async fn shutdown(&self) {
        self.inner.shutdown().await;
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use httpmock::prelude::*;
    use std::time::Duration;
    use url::Url;

    /// Build a test config pointing at the given mock server.
    fn test_config(server: &MockServer) -> OAuthClientConfig {
        OAuthClientConfig {
            token_endpoint: Some(
                Url::parse(&format!("http://localhost:{}/token", server.port())).unwrap(),
            ),
            client_id: "test-client".into(),
            client_secret: SecretString::new("test-secret"),
            http_config: Some(toolkit_http::HttpClientConfig::for_testing()),
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
        assert_traits::<TokenProvider>();
    }

    // -- new ------------------------------------------------------------------

    /// Valid configuration creates an idle provider without an initial exchange.
    #[tokio::test]
    async fn new_with_valid_config() {
        let server = MockServer::start();

        let mock = server.mock(|when, then| {
            when.method(POST).path("/token");
            then.status(200)
                .header("content-type", "application/json")
                .body(token_json("tok-new", 3600));
        });

        let token = TokenProvider::new(test_config(&server));
        assert!(
            token.is_ok(),
            "TokenProvider::new() should succeed: {:?}",
            token.as_ref().err()
        );
        tokio::task::yield_now().await;
        mock.assert_calls(0);
    }

    /// Invalid configuration fails synchronously before acquisition starts.
    #[tokio::test]
    async fn new_validates_config() {
        let cfg = OAuthClientConfig {
            token_endpoint: Some(Url::parse("https://a.example.com/token").unwrap()),
            issuer_url: Some(Url::parse("https://b.example.com").unwrap()),
            client_id: "test-client".into(),
            client_secret: SecretString::new("test-secret"),
            ..Default::default()
        };
        let err = TokenProvider::new(cfg).unwrap_err();
        assert!(
            matches!(err, TokenError::ConfigError(ref msg) if msg.contains("mutually exclusive")),
            "expected ConfigError, got: {err}"
        );
    }

    // -- get ------------------------------------------------------------------

    /// Async acquisition returns an explicitly exposed secret value.
    #[tokio::test]
    async fn get_returns_secret_string() {
        let server = MockServer::start();

        let _mock = server.mock(|when, then| {
            when.method(POST).path("/token");
            then.status(200)
                .header("content-type", "application/json")
                .body(token_json("tok-get-test", 3600));
        });

        let token = TokenProvider::new(test_config(&server)).unwrap();
        token.get().await.unwrap();
        let secret = token.get().await.unwrap();

        assert_eq!(secret.expose(), "tok-get-test");
    }

    // -- invalidate -----------------------------------------------------------

    /// Invalidation fetches a replacement through the shared worker.
    #[tokio::test]
    async fn invalidate_fetches_replacement() {
        let server = MockServer::start();

        let mock = server.mock(|when, then| {
            when.method(POST).path("/token");
            then.status(200)
                .header("content-type", "application/json")
                .body(token_json("tok-inv", 3600));
        });

        let token = TokenProvider::new(test_config(&server)).unwrap();
        token.get().await.unwrap();
        assert_eq!(mock.calls(), 1, "initial fetch");

        token.invalidate().await.unwrap();

        // invalidate uses the shared worker to fetch a fresh token
        assert_eq!(mock.calls(), 2, "after invalidate");
    }

    // -- concurrency ----------------------------------------------------------

    /// Cloned providers can read cached credentials concurrently.
    #[tokio::test]
    async fn concurrent_get_no_deadlock() {
        let server = MockServer::start();

        let _mock = server.mock(|when, then| {
            when.method(POST).path("/token");
            then.status(200)
                .header("content-type", "application/json")
                .body(token_json("tok-conc", 3600));
        });

        let token = TokenProvider::new(test_config(&server)).unwrap();
        token.get().await.unwrap();

        let t1 = {
            let token = token.clone();
            tokio::spawn(async move { token.get().await })
        };
        let t2 = {
            let token = token.clone();
            tokio::spawn(async move { token.get().await })
        };

        let (r1, r2) = tokio::join!(t1, t2);
        assert!(r1.unwrap().is_ok());
        assert!(r2.unwrap().is_ok());
    }

    // -- OIDC discovery -------------------------------------------------------

    /// First acquisition resolves the token endpoint from the issuer.
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
            http_config: Some(toolkit_http::HttpClientConfig::for_testing()),
            jitter_max: Duration::from_millis(0),
            min_refresh_period: Duration::from_millis(100),
            ..Default::default()
        };

        let token = TokenProvider::new(cfg).unwrap();
        token.get().await.unwrap();
        let secret = token.get().await.unwrap();
        assert_eq!(secret.expose(), "tok-discovered");
    }

    /// Replacing credentials reuses the successful discovery result.
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
            http_config: Some(toolkit_http::HttpClientConfig::for_testing()),
            jitter_max: Duration::from_millis(0),
            min_refresh_period: Duration::from_millis(100),
            ..Default::default()
        };

        let token = TokenProvider::new(cfg).unwrap();
        token.get().await.unwrap();
        assert_eq!(discovery_mock.calls(), 1, "discovery: initial");
        assert_eq!(token_mock.calls(), 1, "token: initial");

        // Invalidate should re-fetch the token but NOT re-run discovery.
        token.invalidate().await.unwrap();

        assert_eq!(
            discovery_mock.calls(),
            1,
            "discovery must NOT be repeated on invalidate"
        );
        assert_eq!(token_mock.calls(), 2, "token: after invalidate");
    }

    // -- debug safety ---------------------------------------------------------

    /// Debug output omits credentials and bearer values.
    #[tokio::test]
    async fn debug_does_not_reveal_tokens() {
        let server = MockServer::start();

        let _mock = server.mock(|when, then| {
            when.method(POST).path("/token");
            then.status(200)
                .header("content-type", "application/json")
                .body(token_json("super-secret-tok", 3600));
        });

        let token = TokenProvider::new(test_config(&server)).unwrap();
        token.get().await.unwrap();
        let dbg = format!("{token:?}");
        assert!(
            !dbg.contains("super-secret-tok"),
            "Debug must not reveal token value: {dbg}"
        );
    }
}
