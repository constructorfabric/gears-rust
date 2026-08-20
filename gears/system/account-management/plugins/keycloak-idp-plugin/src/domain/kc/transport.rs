//! Port trait for Keycloak Admin REST transport.
//!
//! Domain code interacts only with [`KcTransport`]; the reqwest-backed
//! implementation lives in `crate::infra::kc_http` and is injected at module
//! initialisation time. This keeps the domain layer free of any reqwest /
//! infra imports and enables in-process fake transports for unit tests.
//!
//! ## Return-type choice
//!
//! The trait returns `Result<(u16, String), crate::domain::error::PluginError>`
//! rather than a generic `TransportError`. `PluginError` IS the plugin's domain
//! error vocabulary, so a domain-internal trait returning it is correct — the
//! trait is not a public SDK seam. This minimises call-site boilerplate: callers
//! already map errors to `PluginError` variants, and the `KcRest` /
//! `DeprovisionRetryable` / `UserOpUnavailable` error path is uniform across
//! both transport failures and non-2xx status.

use async_trait::async_trait;
use serde_json::Value;

use crate::domain::error::PluginError;

/// Raw HTTP transport for Keycloak Admin REST.
///
/// Constructed once per process (built at module init, shared across the
/// factory and all facades via `Arc`). Domain code must not depend on any
/// concrete implementor — always accept `Arc<dyn KcTransport>`.
///
/// ## Contract
///
/// * Returns `Ok((status, body_text))` whenever an HTTP exchange completed,
///   even for non-2xx responses. The body is always fully buffered as UTF-8.
/// * Returns `Err(PluginError::KcRest)` with `KcStatusKind::Transport` or
///   `KcStatusKind::Timeout` when no HTTP exchange occurred (DNS failure, TCP
///   connection refused, TLS error, timeout before first byte).
/// * [`KcTransport::raw_request`] injects the W3C `traceparent` header from
///   the current tracing span into every outbound request per DESIGN "Component Model".
#[async_trait]
pub trait KcTransport: Send + Sync {
    /// Execute a bearer-authenticated JSON-body HTTP request against Keycloak.
    ///
    /// # Parameters
    ///
    /// * `method`       — HTTP verb string (`"GET"`, `"POST"`, etc.)
    /// * `url`          — Full absolute URL (base URL + path, already encoded).
    /// * `bearer_token` — Value for the `Authorization: Bearer …` header.
    /// * `body`         — Optional JSON request body.
    /// * `path_template` — Template string used only for `PluginError::KcRest`
    ///   attribution (e.g. `"/admin/realms/{realm}/groups"`).
    ///
    /// # Errors
    ///
    /// `PluginError::KcRest` with `KcStatusKind::Transport` or
    /// `KcStatusKind::Timeout` on transport / connectivity failure.
    async fn raw_request(
        &self,
        method: &str,
        url: &str,
        bearer_token: &str,
        body: Option<&Value>,
        path_template: &str,
    ) -> Result<(u16, String), PluginError>;

    /// POST to an OIDC token endpoint using `application/x-www-form-urlencoded`
    /// encoding, without a bearer token (used for `client_credentials` token
    /// exchange).
    ///
    /// `fields` is a slice of `(name, value)` pairs written as form fields.
    ///
    /// # Errors
    ///
    /// `PluginError::KcRest` with `KcStatusKind::Transport` or
    /// `KcStatusKind::Timeout` on transport / connectivity failure.
    async fn form_post(
        &self,
        url: &str,
        fields: &[(&str, &str)],
        path_template: &str,
    ) -> Result<(u16, String), PluginError>;

    /// Unauthenticated GET — used for lightweight health / existence probes.
    ///
    /// # Errors
    ///
    /// `PluginError::KcRest` with `KcStatusKind::Transport` or
    /// `KcStatusKind::Timeout` on transport / connectivity failure.
    async fn get_unauthenticated(
        &self,
        url: &str,
        path_template: &str,
    ) -> Result<(u16, String), PluginError>;
}

/// Truncate a response body to the first 2 KiB. Per DESIGN "API Contracts", oversized
/// bodies get a trailing `…` marker; non-printable bytes are escaped to
/// `\xNN`. Moved here from `infra/kc_http.rs` so domain code can call it
/// without crossing the layer boundary.
#[must_use]
pub fn truncate_2kb(s: &str) -> String {
    const CAP: usize = 2048;
    let escaped: String = s
        .chars()
        .map(|c| {
            if c.is_ascii_graphic() || c == ' ' || c == '\t' || c == '\n' || c == '\r' {
                c.to_string()
            } else if c.is_ascii() {
                format!("\\x{:02X}", c as u8)
            } else {
                // Multi-byte UTF-8 — escape each byte separately to stay ASCII-safe.
                use std::fmt::Write as _;
                let mut buf = [0u8; 4];
                let bytes = c.encode_utf8(&mut buf).as_bytes();
                bytes.iter().fold(String::new(), |mut acc, b| {
                    // Writing into a String cannot fail.
                    let _written = write!(acc, "\\x{b:02X}");
                    acc
                })
            }
        })
        .collect();
    if escaped.len() <= CAP {
        escaped
    } else {
        let mut truncated: String = escaped.chars().take(CAP).collect();
        truncated.push('\u{2026}');
        truncated
    }
}

#[cfg(test)]
#[path = "transport_tests.rs"]
mod transport_tests;
