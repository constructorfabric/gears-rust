//! `reqwest`-backed implementation of [`VaultTransport`].
//!
//! All `reqwest` imports of the plugin are confined to this file. It owns the
//! HTTP client (timeout, TLS) and attaches the `X-Vault-Token` header — and
//! `X-Vault-Namespace` when configured — to every outbound request. The token
//! is kept as a plain `String` here (never `Debug`/logged — see
//! [`crate::config::VaultToken`] for the config-side redaction).
use std::time::Duration;

use async_trait::async_trait;
use reqwest::header::CONTENT_TYPE;
use reqwest::{Client, Method};

use crate::config::VaultCredStorePluginConfig;
use crate::domain::transport::{
    HttpMethod, TransportError, VaultRequest, VaultResponse, VaultTransport,
};

/// HTTP transport to a Vault / `OpenBao` server.
pub struct ReqwestTransport {
    http: Client,
    token: String,
    namespace: Option<String>,
}

impl ReqwestTransport {
    /// Builds a transport from plugin configuration.
    ///
    /// # Errors
    /// Returns an error if the underlying HTTP client fails to build (e.g.
    /// an invalid TLS configuration) — this constructor performs no network
    /// I/O itself.
    pub fn from_config(cfg: &VaultCredStorePluginConfig) -> anyhow::Result<Self> {
        let http = Client::builder()
            .timeout(Duration::from_secs(cfg.timeout_secs.max(1)))
            .build()?;
        Ok(Self {
            http,
            token: cfg.token.expose().to_owned(),
            namespace: cfg.namespace.clone(),
        })
    }
}

#[async_trait]
impl VaultTransport for ReqwestTransport {
    async fn send(&self, request: VaultRequest) -> Result<VaultResponse, TransportError> {
        let method = match request.method {
            HttpMethod::Get => Method::GET,
            HttpMethod::Post => Method::POST,
            HttpMethod::Delete => Method::DELETE,
        };
        let mut builder = self
            .http
            .request(method, &request.url)
            .header("X-Vault-Token", &self.token);
        if let Some(ns) = &self.namespace {
            builder = builder.header("X-Vault-Namespace", ns);
        }
        if let Some(body) = request.json_body {
            builder = builder.header(CONTENT_TYPE, "application/json").body(body);
        }

        let response = builder.send().await.map_err(|e| classify_error(&e))?;
        let status = response.status().as_u16();
        let body = response.text().await.unwrap_or_default();
        Ok(VaultResponse { status, body })
    }
}

/// Maps a `reqwest` transport-level failure (connect refused, timeout, DNS,
/// TLS) to the domain's coarse [`TransportError`]. Deliberately never carries
/// `reqwest::Error`'s `Display` text, which can embed the request URL; the
/// vendor/priority/address are not secret, but there is no value in taking on
/// that leak surface for a diagnostic string.
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
