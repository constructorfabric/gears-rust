//! One-shot `OAuth2` client credentials token fetch.
//!
//! Use [`fetch_token`] when you need a single token exchange without spawning a
//! background refresh watcher.  This is the right choice for callers that manage
//! their own cache (e.g. an auth plugin with a TTL-based token cache).
//!
//! For long-lived service singletons that benefit from automatic background
//! refresh, use [`Token`](super::Token) instead.

use std::fmt;
use std::time::Duration;

use aliri_tokens::sources::AsyncTokenSource;

use super::config::OAuthClientConfig;
use super::error::TokenError;
use super::source::OAuthTokenSource;
use modkit_utils::SecretString;

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
/// Returns [`TokenError::Http`] if the token (or discovery) request fails.
/// Returns [`TokenError::UnsupportedTokenType`] if the server returns a
/// non-Bearer token type.
pub async fn fetch_token(mut config: OAuthClientConfig) -> Result<FetchedToken, TokenError> {
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

    let mut source = OAuthTokenSource::new(&config)?;
    let token = source.request_token().await?;

    Ok(FetchedToken {
        bearer: SecretString::new(token.access_token().as_str()),
        expires_in: Duration::from_secs(token.lifetime().0),
    })
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "fetch_tests.rs"]
mod tests;
