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
//! (`X-Vault-Token`, `X-Vault-Namespace`). The token therefore never reaches
//! the domain layer.
use async_trait::async_trait;
use credstore_sdk::CredStoreError;
use toolkit_macros::domain_model;

/// HTTP verb of a KV v2 call.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HttpMethod {
    /// `GET` (value read, metadata read).
    Get,
    /// `POST` (value write, destroy).
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

/// What came back from the backend: the status code and the body text.
#[domain_model]
#[derive(Debug, Clone)]
pub struct VaultResponse {
    /// Numeric HTTP status code.
    pub status: u16,
    /// Response body as text; empty when the body could not be read.
    pub body: String,
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
    /// Any other failure while sending the request or receiving the head of
    /// the response.
    Other,
}

impl TransportError {
    /// Maps the failure to the SDK's "backend unavailable" variant. Never
    /// includes anything but the coarse kind — see the type docs.
    #[must_use]
    pub fn into_sdk_error(self) -> CredStoreError {
        let kind = match self {
            Self::Timeout => "timeout",
            Self::Connect => "connection failed",
            Self::Other => "request failed",
        };
        CredStoreError::service_unavailable(format!("vault credstore plugin: {kind}"))
    }
}

/// Sends requests to the Vault / `OpenBao` backend.
///
/// # Contract
///
/// * `Ok(VaultResponse)` whenever an HTTP exchange completed, whatever the
///   status code (a `404` or `5xx` is a response, not an error).
/// * `Err(TransportError)` only when no response was obtained (connect
///   failure, timeout, DNS, TLS).
#[async_trait]
pub trait VaultTransport: Send + Sync {
    /// Executes `request` against the backend.
    ///
    /// # Errors
    /// [`TransportError`] when no HTTP response was obtained.
    async fn send(&self, request: VaultRequest) -> Result<VaultResponse, TransportError>;
}
