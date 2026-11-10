// Created: 2026-10-03 by Constructor Tech
// Updated: 2026-10-06 by Constructor Tech
//! `reqwest`-backed implementation of [`VaultTransport`].
//!
//! All `reqwest` imports of the plugin are confined to this file. It owns the
//! HTTP client (timeout; TLS is `reqwest`'s default) and attaches the
//! `X-Vault-Token` header, and `X-Vault-Namespace` when configured, to every
//! outbound request. The token lives in a [`TokenStore`] (never `Debug`
//! printed or logged; see [`crate::config::VaultToken`] for the config-side
//! redaction).
//!
//! When Vault answers `403` the transport re-reads the token source once and,
//! if the token changed (a Vault Agent sidecar rotated the file), sends the
//! same request once more with the new token. A second `403`, or an unchanged
//! token, is returned to the caller as an ordinary response.
use std::time::Duration;

use async_trait::async_trait;
use reqwest::header::{CONTENT_TYPE, HeaderMap, HeaderValue, RETRY_AFTER};
use reqwest::{Client, Method};
use tracing::warn;

use super::token::{TokenSource, TokenStore};
use crate::config::VaultCredStorePluginConfig;
use crate::domain::transport::{
    HttpMethod, TransportError, VaultRequest, VaultResponse, VaultTransport,
};

/// The largest `Retry-After` the plugin passes on, in seconds.
const MAX_RETRY_AFTER_SECS: u64 = 300;

/// HTTP transport to a Vault / `OpenBao` server.
pub struct ReqwestTransport {
    http: Client,
    tokens: TokenStore,
    namespace: Option<String>,
}

impl ReqwestTransport {
    /// Builds a transport from plugin configuration.
    ///
    /// Performs no network I/O. An inline token and a `token_env` variable
    /// are resolved here; a `token_file` is read on the first request.
    ///
    /// # Errors
    /// The token source is not unique, or an inline / environment token is
    /// missing or malformed, or the underlying HTTP client fails to build.
    pub fn from_config(cfg: &VaultCredStorePluginConfig) -> anyhow::Result<Self> {
        let tokens = TokenStore::new(TokenSource::from_config(cfg)?)?;
        let http = Client::builder()
            .timeout(Duration::from_secs(cfg.timeout_secs))
            .build()?;
        Ok(Self {
            http,
            tokens,
            namespace: cfg.namespace.clone(),
        })
    }

    /// Name of the configuration key the token comes from, for logging.
    #[must_use]
    pub fn token_source(&self) -> &'static str {
        self.tokens.source().kind()
    }

    /// Sends `request` once, authenticating with `token`.
    async fn send_once(
        &self,
        request: &VaultRequest,
        token: &str,
    ) -> Result<VaultResponse, TransportError> {
        let method = match request.method {
            HttpMethod::Get => Method::GET,
            HttpMethod::Post => Method::POST,
            HttpMethod::Delete => Method::DELETE,
        };
        // Marked sensitive so `reqwest`'s own `Debug` output hides it.
        let mut token_header =
            HeaderValue::from_str(token).map_err(|_| TransportError::Credentials)?;
        token_header.set_sensitive(true);
        let mut builder = self
            .http
            .request(method, &request.url)
            .header("X-Vault-Token", token_header);
        if let Some(ns) = &self.namespace {
            builder = builder.header("X-Vault-Namespace", ns);
        }
        if let Some(body) = &request.json_body {
            builder = builder
                .header(CONTENT_TYPE, "application/json")
                .body(body.clone());
        }

        let response = builder.send().await.map_err(|e| classify_error(&e))?;
        let status = response.status().as_u16();
        let retry_after = parse_retry_after(response.headers());
        // A body that cannot be read is a failed exchange, not an empty one.
        let body = response.text().await.map_err(|e| classify_error(&e))?;
        Ok(VaultResponse {
            status,
            body,
            retry_after,
        })
    }
}

#[async_trait]
impl VaultTransport for ReqwestTransport {
    async fn send(&self, request: VaultRequest) -> Result<VaultResponse, TransportError> {
        let token = self.tokens.current().await.map_err(|error| {
            warn!(
                source = self.tokens.source().kind(),
                %error,
                "vault credstore plugin: the Vault token is not available"
            );
            TransportError::Credentials
        })?;
        let response = self.send_once(&request, &token).await?;
        if response.status != 403 {
            return Ok(response);
        }
        match self.tokens.refreshed_after_rejection(&token).await {
            Some(fresh) => self.send_once(&request, &fresh).await,
            None => Ok(response),
        }
    }
}

/// Reads a `Retry-After` given in seconds (the HTTP-date form is ignored),
/// capped at five minutes.
fn parse_retry_after(headers: &HeaderMap) -> Option<Duration> {
    let seconds: u64 = headers
        .get(RETRY_AFTER)?
        .to_str()
        .ok()?
        .trim()
        .parse()
        .ok()?;
    Some(Duration::from_secs(seconds.min(MAX_RETRY_AFTER_SECS)))
}

/// Maps a `reqwest` transport-level failure (connect refused, timeout, DNS,
/// TLS, a broken body) to the domain's coarse [`TransportError`]. Deliberately
/// never carries `reqwest::Error`'s `Display` text, which can embed the
/// request URL; the vendor/priority/address are not secret, but there is no
/// value in taking on that leak surface for a diagnostic string.
fn classify_error(err: &reqwest::Error) -> TransportError {
    if err.is_timeout() {
        TransportError::Timeout
    } else if err.is_connect() {
        TransportError::Connect
    } else {
        TransportError::Other
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "http_tests.rs"]
mod http_tests;
