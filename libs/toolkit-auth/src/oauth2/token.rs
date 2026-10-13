use std::fmt;
use std::sync::Arc;

use arc_swap::ArcSwap;

use super::config::OAuthClientConfig;
use super::error::TokenError;
use super::source::OAuthTokenSource;
use super::token_watcher::{TokenWatcher, WatcherConfig};
use toolkit_utils::SecretString;

/// Internal state holding the live watcher.
///
/// Wrapped in `Arc<ArcSwap<_>>` so that [`Token::invalidate`] can atomically
/// swap in a replacement without blocking concurrent [`Token::get`] calls.
struct TokenInner {
    watcher: TokenWatcher,
}

/// Handle for obtaining `OAuth2` bearer tokens.
///
/// Internally drives a `TokenWatcher` for background refresh
/// and exposes lock-free reads via `ArcSwap` (same pattern as the JWKS key
/// provider).
///
/// `Token` is [`Clone`] + [`Send`] + [`Sync`] — share freely across tasks.
#[derive(Clone)]
pub struct Token {
    inner: Arc<ArcSwap<TokenInner>>,
    source: Arc<OAuthTokenSource>,
    watcher_config: WatcherConfig,
}

impl fmt::Debug for Token {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Token").finish_non_exhaustive()
    }
}

impl Token {
    /// Create a new token handle and start background refresh.
    ///
    /// This performs an initial token fetch — if the token endpoint is
    /// unreachable or returns an error, `new` will fail immediately.
    ///
    /// # Errors
    ///
    /// Returns [`TokenError::ConfigError`] if the config is invalid.
    /// Returns [`TokenError::Http`] or [`TokenError::HttpStatus`] if acquisition fails.
    pub async fn new(config: OAuthClientConfig) -> Result<Self, TokenError> {
        let (source, watcher_config) = Self::prepare(&config)?;
        let watcher = spawn_watcher(source.clone(), &watcher_config).await?;
        Ok(Self::with_watcher(source, watcher_config, watcher))
    }

    /// Create a handle without making discovery or token requests.
    ///
    /// The first [`Self::get`] starts acquisition and returns
    /// [`TokenError::Unavailable`] before the first attempt completes, then the
    /// latest acquisition error until a token is cached. The watcher retries
    /// failures with exponential backoff, then refreshes normally after success.
    /// Clones share one watcher. Dropping the last handle cancels pending work.
    /// Requires an active Tokio runtime, just like [`Self::new`].
    ///
    /// # Errors
    /// Returns configuration or HTTP client construction errors immediately,
    /// including [`TokenError::ConfigError`] when no Tokio runtime is active.
    #[allow(clippy::needless_pass_by_value)] // Match new() and consume the credential config.
    pub fn new_lazy(config: OAuthClientConfig) -> Result<Self, TokenError> {
        tokio::runtime::Handle::try_current().map_err(|_| {
            TokenError::ConfigError("Token::new_lazy requires an active Tokio runtime".into())
        })?;
        let (source, watcher_config) = Self::prepare(&config)?;
        let watcher = TokenWatcher::spawn_lazy(source.clone(), watcher_config);
        Ok(Self::with_watcher(source, watcher_config, watcher))
    }

    /// Validate configuration and build a token source without network I/O.
    fn prepare(
        config: &OAuthClientConfig,
    ) -> Result<(OAuthTokenSource, WatcherConfig), TokenError> {
        config.validate()?;
        let watcher_config = WatcherConfig::new(config.jitter_max, config.min_refresh_period)?;
        Ok((OAuthTokenSource::new(config)?, watcher_config))
    }

    /// Assemble a clonable handle sharing the watcher and discovery state.
    fn with_watcher(
        source: OAuthTokenSource,
        config: WatcherConfig,
        watcher: TokenWatcher,
    ) -> Self {
        Self {
            inner: Arc::new(ArcSwap::from_pointee(TokenInner { watcher })),
            source: Arc::new(source),
            watcher_config: config,
        }
    }

    /// Get the current bearer token.
    ///
    /// This is a lock-free read from the `ArcSwap`-cached watcher — it never
    /// blocks on a network call.  The underlying watcher refreshes the token in
    /// the background before it expires.
    ///
    /// The returned [`SecretString`] wraps the raw access-token value so it is
    /// not accidentally logged.
    ///
    /// # Errors
    ///
    /// Returns [`TokenError::Unavailable`] while initial acquisition is pending
    /// or if the cached token has expired. After failed initial acquisition,
    /// returns the last acquisition error instead. For a lazy handle, this call starts
    /// the watcher without waiting for network I/O.
    pub fn get(&self) -> Result<SecretString, TokenError> {
        let guard = self.inner.load();
        guard.watcher.valid_token()
    }

    /// Wait for a usable token or the result of one shared acquisition attempt.
    ///
    /// Returns a valid cached token immediately. Otherwise starts lazy acquisition
    /// and waits for the current or next scheduled attempt, including its backoff.
    /// Each waiter receives that attempt's result even if another retry completes
    /// before the waiter is polled again. Cancelling this future does not cancel
    /// the shared watcher; callers may impose their own timeout.
    ///
    /// # Errors
    /// Returns the acquisition error of the observed attempt, or
    /// [`TokenError::Unavailable`] if the watcher stops or its token has expired.
    pub async fn wait_ready(&self) -> Result<SecretString, TokenError> {
        let inner = self.inner.load_full();
        inner.watcher.wait_ready().await
    }

    /// Force-replace the internal watcher with a freshly-spawned one.
    ///
    /// Use this after receiving a 401 from a downstream service to immediately
    /// discard a potentially revoked token.
    ///
    /// If fetching a replacement token fails, a warning is
    /// logged and the existing watcher is left in place.
    pub async fn invalidate(&self) {
        // Share pending acquisition rather than starting a competing fetch.
        if self.inner.load().watcher.start_if_pending() {
            return;
        }
        let source = self.source.as_ref().clone();

        let watcher = match spawn_watcher(source, &self.watcher_config).await {
            Ok(w) => w,
            Err(e) => {
                tracing::warn!("OAuth2 token invalidation: initial fetch failed: {e}");
                return;
            }
        };

        self.inner.store(Arc::new(TokenInner { watcher }));
    }
}

/// Spawn a [`TokenWatcher`] from the given source and config.
async fn spawn_watcher(
    source: OAuthTokenSource,
    config: &WatcherConfig,
) -> Result<TokenWatcher, TokenError> {
    TokenWatcher::spawn(source, *config).await
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
            http_config: Some(toolkit_http::HttpClientConfig::for_testing()),
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
            http_config: Some(toolkit_http::HttpClientConfig::for_testing()),
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
}
