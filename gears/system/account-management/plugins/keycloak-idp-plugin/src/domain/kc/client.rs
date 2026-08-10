//! Per-call Keycloak admin client handle.
//!
//! Returned by `KeycloakAdminClientFactory::bootstrap_client()` /
//! `realm_client(...)` (Phase 7). Wraps an `Arc<dyn KcTransport>` and an
//! `Arc<CachedToken>`; cloneable, cheap.

use std::sync::Arc;

use serde_json::Value;
use toolkit_macros::domain_model;

use crate::domain::error::PluginError;
use crate::domain::kc::token_cache::CachedToken;
use crate::domain::kc::transport::KcTransport;

#[domain_model]
#[derive(Clone)]
pub struct KcAdminClient {
    transport: Arc<dyn KcTransport>,
    token: Arc<CachedToken>,
}

impl KcAdminClient {
    #[must_use]
    pub fn new(transport: Arc<dyn KcTransport>, token: Arc<CachedToken>) -> Self {
        Self { transport, token }
    }

    /// `{base_url}/admin/realms/{realm}` — the prefix this token authorises.
    #[must_use]
    pub fn realm_url(&self) -> &str {
        &self.token.realm_url
    }

    /// Execute a raw HTTP request using this client's bearer token.
    ///
    /// Delegates to the underlying [`KcTransport`] so domain code never
    /// imports reqwest or infra modules directly.
    ///
    /// # Errors
    ///
    /// `PluginError::KcRest` on transport / timeout failure.
    pub async fn raw_request(
        &self,
        method: &str,
        url: &str,
        body: Option<&Value>,
        path_template: &str,
    ) -> Result<(u16, String), PluginError> {
        self.transport
            .raw_request(method, url, self.access_token(), body, path_template)
            .await
    }

    /// Bearer token string. Borrow stays inside the handle; do NOT clone
    /// or log. `pub(crate)` so the only caller is `raw_request` above
    /// (which threads it straight to the transport) — type-system
    /// enforcement matches the docstring contract.
    #[must_use]
    pub(crate) fn access_token(&self) -> &str {
        use secrecy::ExposeSecret as _;
        self.token.access_token.expose_secret()
    }
}

impl std::fmt::Debug for KcAdminClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("KcAdminClient")
            .field("realm_url", &self.token.realm_url)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::error::PluginError;
    use crate::domain::kc::token_cache::CachedToken;
    use crate::domain::kc::transport::KcTransport;
    use async_trait::async_trait;
    use secrecy::SecretString;
    use serde_json::Value;
    use std::sync::Arc;
    use std::time::Duration;
    use std::time::Instant;

    /// No-op transport for unit tests that only exercise non-HTTP paths.
    #[domain_model]
    struct StubTransport;

    #[async_trait]
    impl KcTransport for StubTransport {
        async fn raw_request(
            &self,
            _method: &str,
            _url: &str,
            _bearer_token: &str,
            _body: Option<&Value>,
            _path_template: &str,
        ) -> Result<(u16, String), PluginError> {
            Ok((200, String::new()))
        }

        async fn form_post(
            &self,
            _url: &str,
            _fields: &[(&str, &str)],
            _path_template: &str,
        ) -> Result<(u16, String), PluginError> {
            Ok((200, String::new()))
        }

        async fn get_unauthenticated(
            &self,
            _url: &str,
            _path_template: &str,
        ) -> Result<(u16, String), PluginError> {
            Ok((200, String::new()))
        }
    }

    fn make_client(token: &str) -> KcAdminClient {
        KcAdminClient::new(
            Arc::new(StubTransport),
            Arc::new(CachedToken {
                access_token: SecretString::from(String::from(token)),
                expires_at: Instant::now() + Duration::from_mins(1),
                realm_url: Arc::from("https://kc/admin/realms/master"),
            }),
        )
    }

    #[test]
    fn debug_does_not_leak_token() {
        let c = make_client("sek");
        let dbg = format!("{c:?}");
        assert!(!dbg.contains("sek"), "Debug must not leak token: {dbg}");
    }

    #[test]
    fn realm_url_exposes_token_url() {
        let c = make_client("x");
        assert_eq!(c.realm_url(), "https://kc/admin/realms/master");
    }
}
