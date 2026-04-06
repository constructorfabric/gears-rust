use std::time::Duration;

use aliri_clock::DurationSecs;
use aliri_tokens::sources::AsyncTokenSource;
use aliri_tokens::{AccessToken, IdToken, TokenLifetimeConfig, TokenWithLifetime};
use async_trait::async_trait;
use base64::{Engine as _, engine::general_purpose};
use http::header::AUTHORIZATION;
use url::Url;
use zeroize::Zeroizing;

use super::config::OAuthClientConfig;
use super::error::TokenError;
use super::types::ClientAuthMethod;
use modkit_utils::SecretString;

/// Token source that exchanges client credentials for an access token using
/// `modkit-http::HttpClient`.
///
/// It implements [`aliri_tokens::AsyncTokenSource`] so that
/// `aliri_tokens` can drive refresh scheduling, jitter, and backoff.
pub struct OAuthTokenSource {
    client: modkit_http::HttpClient,
    token_endpoint: Url,
    client_id: String,
    client_secret: SecretString,
    /// Pre-joined scopes (space-separated), or `None` when the scope list is
    /// empty.
    scopes: Option<String>,
    auth_method: ClientAuthMethod,
    extra_headers: Vec<(String, String)>,
    default_ttl: Duration,
    refresh_offset: Duration,
    min_refresh_period: Duration,
}

impl OAuthTokenSource {
    /// Build a new token source from the given configuration.
    ///
    /// # Errors
    ///
    /// Returns [`TokenError::ConfigError`] if `token_endpoint` is `None`.
    ///
    /// Returns [`TokenError::Http`] if the underlying `HttpClient` fails to
    /// build.
    pub fn new(config: &OAuthClientConfig) -> Result<Self, TokenError> {
        let token_endpoint = config
            .token_endpoint
            .clone()
            .ok_or_else(|| TokenError::ConfigError("token_endpoint is required".into()))?;

        let http_config = config
            .http_config
            .clone()
            .unwrap_or_else(modkit_http::HttpClientConfig::token_endpoint);

        let client = modkit_http::HttpClientBuilder::with_config(http_config)
            .build()
            .map_err(|e| {
                TokenError::Http(crate::http_error::format_http_error(&e, "OAuth2 token"))
            })?;

        let scopes = if config.scopes.is_empty() {
            None
        } else {
            Some(config.scopes.join(" "))
        };

        Ok(Self {
            client,
            token_endpoint,
            client_id: config.client_id.clone(),
            client_secret: config.client_secret.clone(),
            scopes,
            auth_method: config.auth_method,
            extra_headers: config.extra_headers.clone(),
            default_ttl: config.default_ttl,
            refresh_offset: config.refresh_offset,
            min_refresh_period: config.min_refresh_period,
        })
    }
}

#[async_trait]
impl AsyncTokenSource for OAuthTokenSource {
    type Error = TokenError;

    async fn request_token(&mut self) -> Result<TokenWithLifetime, Self::Error> {
        // -- build form fields ---------------------------------------------------
        let mut fields: Vec<(&str, &str)> = vec![("grant_type", "client_credentials")];

        if let Some(ref scope) = self.scopes {
            fields.push(("scope", scope));
        }

        // For Form auth, credentials go into the form body.
        // Wrap the temporary copy in `Zeroizing` so it is scrubbed on drop.
        let secret_expose;
        if self.auth_method == ClientAuthMethod::Form {
            secret_expose = Zeroizing::new(self.client_secret.expose().to_owned());
            fields.push(("client_id", &self.client_id));
            fields.push(("client_secret", &secret_expose));
        }

        // -- build request -------------------------------------------------------
        let mut builder = self.client.post(self.token_endpoint.as_str());

        // For Basic auth, credentials go into the Authorization header.
        // Wrap intermediates in `Zeroizing` so the plaintext is scrubbed on drop.
        if self.auth_method == ClientAuthMethod::Basic {
            let credentials = Zeroizing::new(format!(
                "{}:{}",
                self.client_id,
                self.client_secret.expose()
            ));
            let encoded = Zeroizing::new(general_purpose::STANDARD.encode(credentials.as_bytes()));
            let header_value = Zeroizing::new(format!("Basic {}", &*encoded));
            builder = builder.header(AUTHORIZATION.as_str(), &header_value);
        }

        // Apply vendor-specific extra headers.
        for (name, value) in &self.extra_headers {
            builder = builder.header(name, value);
        }

        let response = builder
            .form(fields.as_slice())
            .map_err(|e| {
                TokenError::Http(crate::http_error::format_http_error(&e, "OAuth2 token"))
            })?
            .send()
            .await
            .map_err(|e| {
                TokenError::Http(crate::http_error::format_http_error(&e, "OAuth2 token"))
            })?;

        // -- check status, then parse response ------------------------------------
        let token_resp: super::types::TokenResponse = response
            .error_for_status()
            .map_err(|e| {
                TokenError::Http(crate::http_error::format_http_error(&e, "OAuth2 token"))
            })?
            .json()
            .await
            .map_err(|e| {
                TokenError::Http(crate::http_error::format_http_error(&e, "OAuth2 token"))
            })?;

        // -- validate token_type -------------------------------------------------
        if let Some(ref tt) = token_resp.token_type
            && !tt.eq_ignore_ascii_case("bearer")
        {
            return Err(TokenError::UnsupportedTokenType(tt.clone()));
        }

        // -- compute lifetime ----------------------------------------------------
        let lifetime_secs = token_resp.expires_in.unwrap_or(self.default_ttl.as_secs());

        // Compute per-token refresh parameters so that the stale time
        // never exceeds the expiry time, even for short-lived tokens.
        let (freshness, min_stale) = refresh_params(
            lifetime_secs,
            &self.refresh_offset,
            &self.min_refresh_period,
        );
        let lifetime_config = TokenLifetimeConfig::new(freshness, min_stale);

        let access_token = AccessToken::new(token_resp.access_token);
        let token = lifetime_config.create_token(
            &access_token,
            None::<&IdToken>,
            DurationSecs(lifetime_secs),
        );

        Ok(token)
    }
}

/// Compute refresh parameters for [`TokenLifetimeConfig`].
///
/// Returns `(freshness_period, min_staleness_period)` such that
/// `max(lifetime × freshness_period, min_staleness_period) <= lifetime`,
/// guaranteeing the stale time never exceeds the expiry time.
///
/// `min_refresh_period` is used as a **lower bound on the staleness
/// window** (the minimum time the token spends in the "stale" state
/// before expiry), not as a refresh deadline. It is capped to
/// `desired_delay` so it can never push the stale time past expiry.
/// In `aliri_tokens` terms it maps to `min_staleness_period`.
///
/// - Normal case (`offset < lifetime`): stale `offset` seconds before
///   expiry.
/// - Short-lived token (`offset >= lifetime`): stale at 50% of lifetime.
/// - Zero lifetime: immediately stale.
#[allow(clippy::integer_division, clippy::cast_precision_loss)]
fn refresh_params(
    lifetime_secs: u64,
    refresh_offset: &Duration,
    min_refresh_period: &Duration,
) -> (f64, DurationSecs) {
    if lifetime_secs == 0 {
        return (0.0, DurationSecs(0));
    }

    let offset = refresh_offset.as_secs();
    let desired_delay = if offset < lifetime_secs {
        lifetime_secs - offset
    } else {
        // Fallback: stale at 50% of lifetime (truncation is fine).
        lifetime_secs / 2
    };

    // Precision loss is negligible — token lifetimes are practical
    // values well within f64 mantissa range.
    let freshness = (desired_delay as f64) / (lifetime_secs as f64);
    let min_stale = min_refresh_period.as_secs().min(desired_delay);

    (freshness, DurationSecs(min_stale))
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "source_tests.rs"]
mod tests;
