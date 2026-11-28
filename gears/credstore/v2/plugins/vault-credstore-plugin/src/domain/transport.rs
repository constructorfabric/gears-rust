// Created: 2026-10-03 by Constructor Tech
// Updated: 2026-10-06 by Constructor Tech
//! Port for the raw HTTP exchange with the Vault / `OpenBao` REST API.
//!
//! The domain [`Service`](super::Service) builds fully-resolved requests (URL,
//! verb, JSON body) and classifies the plain status/body pairs it gets back;
//! it never names an HTTP client. The `reqwest`-backed implementation lives in
//! `crate::infra::http` and is injected at gear initialisation, which keeps
//! this layer free of infrastructure crates and lets the service be driven by
//! any other transport.
//!
//! The transport owns everything that is per-connection rather than
//! per-request: timeouts, TLS, and the authentication / namespace headers
//! (`X-Vault-Token`, `X-Vault-Namespace`), including re-reading a rotated
//! token after a `403`. The token therefore never reaches the domain layer.
use std::time::Duration;

use async_trait::async_trait;
use credstore_sdk::CredStorePluginError;
use toolkit_macros::domain_model;

/// HTTP verb of a KV v2 call.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HttpMethod {
    /// `GET` (value read, metadata read).
    Get,
    /// `POST` (value write, key settings, destroy).
    Post,
    /// `DELETE` (metadata removal).
    Delete,
}

/// A fully-resolved request to the backend.
#[domain_model]
#[derive(Debug, Clone)]
pub struct VaultRequest {
    /// HTTP verb.
    pub method: HttpMethod,
    /// Absolute request URL (address + `/v1/...` path + query).
    pub url: String,
    /// JSON request body, already serialized; `None` for bodiless calls. The
    /// transport sends it with `Content-Type: application/json`.
    pub json_body: Option<String>,
}

/// What came back from the backend: the status code, the body text and the
/// server's retry hint.
#[domain_model]
#[derive(Debug, Clone)]
pub struct VaultResponse {
    /// Numeric HTTP status code.
    pub status: u16,
    /// Response body as text.
    pub body: String,
    /// The `Retry-After` header as a delay, when the server sent one in
    /// seconds.
    pub retry_after: Option<Duration>,
}

/// Why no HTTP response was obtained (a transport-level failure).
///
/// Deliberately coarse: it carries no message, because the underlying client
/// error's `Display` text can embed the request URL.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransportError {
    /// The request timed out.
    Timeout,
    /// The connection could not be established.
    Connect,
    /// The authentication token could not be obtained (its file or variable
    /// is missing, empty or malformed). No request was sent.
    Credentials,
    /// Any other failure while sending the request or receiving the
    /// response.
    Other,
}

impl TransportError {
    /// Maps the failure to the plugin error's "store unavailable" variant. Never
    /// includes anything but the coarse kind — see the type docs.
    #[must_use]
    pub fn into_sdk_error(self) -> CredStorePluginError {
        let kind = match self {
            Self::Timeout => "timeout",
            Self::Connect => "connection failed",
            Self::Credentials => "token unavailable",
            Self::Other => "request failed",
        };
        CredStorePluginError::unavailable(format!("vault credstore plugin: {kind}"), None)
    }
}

/// Sends requests to the Vault / `OpenBao` backend.
///
/// # Contract
///
/// * `Ok(VaultResponse)` whenever an HTTP exchange completed, whatever the
///   status code (a `404` or `5xx` is a response, not an error).
/// * `Err(TransportError)` only when no complete response was obtained
///   (connect failure, timeout, DNS, TLS, a body that could not be read, a
///   token that could not be loaded).
/// * A `403` that survived the transport's own token re-read is returned as
///   a plain response; the domain maps it.
#[async_trait]
pub trait VaultTransport: Send + Sync {
    /// Executes `request` against the backend.
    ///
    /// # Errors
    /// [`TransportError`] when no HTTP response was obtained.
    async fn send(&self, request: VaultRequest) -> Result<VaultResponse, TransportError>;
}
