use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use aliri_clock::DurationSecs;
use aliri_tokens::backoff::ErrorBackoffConfig;
use aliri_tokens::jitter::RandomEarlyJitter;
use aliri_tokens::{TokenStatus, TokenWatcher};
use arc_swap::ArcSwap;

use super::config::OAuthClientConfig;
use super::error::TokenError;
use super::source::OAuthTokenSource;
use modkit_utils::SecretString;

/// Internal state holding the live watcher.
///
/// Wrapped in `Arc<ArcSwap<_>>` so that [`Token::invalidate`] can atomically
/// swap in a replacement without blocking concurrent [`Token::get`] calls.
struct TokenInner {
    watcher: TokenWatcher,
}

/// Parameters needed to (re-)spawn a [`TokenWatcher`].
struct WatcherConfig {
    jitter_max: Duration,
    min_refresh_period: Duration,
}

/// Handle for obtaining `OAuth2` bearer tokens.
///
/// Internally drives an `aliri_tokens::TokenWatcher` for background refresh and
/// exposes lock-free reads via `ArcSwap` (same pattern as the JWKS key
/// provider).
///
/// `Token` is [`Clone`] + [`Send`] + [`Sync`] — share freely across tasks.
#[derive(Clone)]
pub struct Token {
    inner: Arc<ArcSwap<TokenInner>>,
    source_factory: Arc<dyn Fn() -> Result<OAuthTokenSource, TokenError> + Send + Sync>,
    watcher_config: Arc<WatcherConfig>,
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
    /// Returns [`TokenError::Http`] if the initial token fetch fails.
    pub async fn new(mut config: OAuthClientConfig) -> Result<Self, TokenError> {
        config.validate()?;

        // Resolve issuer_url → token_endpoint via OIDC discovery (one-time).
        if let Some(issuer_url) = config.issuer_url.take() {
            let http_config = config
                .http_config
                .clone()
                .unwrap_or_else(modkit_http::HttpClientConfig::token_endpoint);
            let client = modkit_http::HttpClientBuilder::with_config(http_config)
                .build()
                .map_err(|e| {
                    TokenError::Http(crate::http_error::format_http_error(&e, "OIDC discovery"))
                })?;
            let resolved = super::discovery::discover_token_endpoint(&client, &issuer_url).await?;
            config.token_endpoint = Some(resolved);
        }

        let watcher_config = Arc::new(WatcherConfig {
            jitter_max: config.jitter_max,
            min_refresh_period: config.min_refresh_period,
        });

        let source = OAuthTokenSource::new(&config)?;
        let watcher = spawn_watcher(source, &watcher_config).await?;

        let source_factory: Arc<dyn Fn() -> Result<OAuthTokenSource, TokenError> + Send + Sync> =
            Arc::new(move || OAuthTokenSource::new(&config));

        Ok(Self {
            inner: Arc::new(ArcSwap::from_pointee(TokenInner { watcher })),
            source_factory,
            watcher_config,
        })
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
    /// Returns [`TokenError::Unavailable`] if the cached token has expired
    /// (the background watcher has not yet refreshed it).
    pub fn get(&self) -> Result<SecretString, TokenError> {
        let guard = self.inner.load();
        let borrowed = guard.watcher.token();
        if matches!(borrowed.token_status(), TokenStatus::Expired) {
            return Err(TokenError::Unavailable(
                "token expired, refresh pending".into(),
            ));
        }
        let raw = borrowed.access_token().as_str();
        Ok(SecretString::new(raw))
    }

    /// Force-replace the internal watcher with a freshly-spawned one.
    ///
    /// Use this after receiving a 401 from a downstream service to immediately
    /// discard a potentially revoked token.
    ///
    /// If recreating the source or the initial token fetch fails, a warning is
    /// logged and the existing watcher is left in place.
    pub async fn invalidate(&self) {
        let source = match (self.source_factory)() {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!("OAuth2 token invalidation: failed to create source: {e}");
                return;
            }
        };

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
    let jitter = RandomEarlyJitter::new(DurationSecs(config.jitter_max.as_secs()));
    let backoff =
        ErrorBackoffConfig::new(config.min_refresh_period, config.min_refresh_period * 30, 2);

    TokenWatcher::spawn_from_token_source(source, jitter, backoff).await
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "token_tests.rs"]
mod tests;
