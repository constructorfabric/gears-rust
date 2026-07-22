// Updated: 2026-04-07 by Constructor Tech
//! Service implementation for the static `AuthN` resolver plugin.

use std::collections::HashMap;
use std::fmt;

use secrecy::{ExposeSecret, SecretString};
use toolkit_macros::domain_model;
use toolkit_security::SecurityContext;

use crate::config::{AuthNMode, IdentityConfig, StaticAuthNPluginConfig};
use authn_resolver_sdk::{AuthenticationResult, ClientCredentialsRequest};

/// Wraps a `SecretString` bearer token so it can be a `HashMap` key.
///
/// `secrecy::SecretString` deliberately has no `Hash`/`Eq` — and being a
/// foreign type, we can't add them directly here either (orphan rule). This
/// newtype supplies both by delegating to the exposed value, and delegates
/// `Debug` to `SecretString`'s own redacting impl so a Debug dump of the map
/// never prints the raw token.
struct TokenKey(SecretString);

impl PartialEq for TokenKey {
    fn eq(&self, other: &Self) -> bool {
        self.0.expose_secret() == other.0.expose_secret()
    }
}
impl Eq for TokenKey {}

impl std::hash::Hash for TokenKey {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.0.expose_secret().hash(state);
    }
}

impl std::borrow::Borrow<str> for TokenKey {
    fn borrow(&self) -> &str {
        self.0.expose_secret()
    }
}

impl fmt::Debug for TokenKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&self.0, f)
    }
}

/// Static `AuthN` resolver service.
///
/// Provides token-to-identity mapping based on configuration mode:
/// - `accept_all`: Any non-empty token maps to the default identity
/// - `static_tokens`: Specific tokens map to specific identities
#[domain_model]
pub struct Service {
    mode: AuthNMode,
    default_identity: IdentityConfig,
    token_map: HashMap<TokenKey, IdentityConfig>,
    s2s_credentials: HashMap<String, S2sEntry>,
}

/// Internal entry for S2S credential lookup.
#[domain_model]
struct S2sEntry {
    client_secret: SecretString,
    identity: IdentityConfig,
    bearer_token: Option<SecretString>,
}

impl Service {
    /// Create a service from plugin configuration.
    #[must_use]
    pub fn from_config(cfg: &StaticAuthNPluginConfig) -> Self {
        let token_map = cfg
            .tokens
            .iter()
            .map(|m| (TokenKey(m.token.clone()), m.identity.clone()))
            .collect();

        let s2s_credentials: HashMap<String, S2sEntry> = cfg
            .s2s_credentials
            .iter()
            .map(|m| {
                (
                    m.client_id.clone(),
                    S2sEntry {
                        client_secret: m.client_secret.clone(),
                        identity: m.identity.clone(),
                        bearer_token: m.bearer_token.clone(),
                    },
                )
            })
            .collect();

        Self {
            mode: cfg.mode.clone(),
            default_identity: cfg.default_identity.clone(),
            token_map,
            s2s_credentials,
        }
    }

    /// Authenticate a bearer token and return the identity.
    ///
    /// Returns `None` if the token is not recognized (in `static_tokens` mode)
    /// or empty.
    #[must_use]
    pub fn authenticate(&self, bearer_token: &str) -> Option<AuthenticationResult> {
        if bearer_token.is_empty() {
            return None;
        }

        let identity = match &self.mode {
            AuthNMode::AcceptAll => &self.default_identity,
            AuthNMode::StaticTokens => self.token_map.get(bearer_token)?,
        };

        build_result(identity, Some(SecretString::from(bearer_token)))
    }

    /// Exchange client credentials for a `SecurityContext`.
    ///
    /// Looks up `client_id` in the configured S2S credentials and verifies
    /// the `client_secret`. Returns `None` if credentials are not found or
    /// do not match.
    #[must_use]
    pub fn exchange_client_credentials(
        &self,
        request: &ClientCredentialsRequest,
    ) -> Option<AuthenticationResult> {
        let entry = self.s2s_credentials.get(&request.client_id)?;
        if entry.client_secret.expose_secret() != request.client_secret.expose_secret() {
            return None;
        }
        build_result(&entry.identity, entry.bearer_token.clone())
    }
}

fn build_result(
    identity: &IdentityConfig,
    bearer_token: Option<SecretString>,
) -> Option<AuthenticationResult> {
    let mut builder = SecurityContext::builder()
        .subject_id(identity.subject_id)
        .subject_tenant_id(identity.subject_tenant_id)
        .token_scopes(identity.token_scopes.clone());

    if let Some(st) = &identity.subject_type {
        builder = builder.subject_type(st);
    }
    if let Some(token) = bearer_token {
        builder = builder.bearer_token(token);
    }

    let ctx = builder
        .build()
        .map_err(|e| tracing::error!("Failed to build SecurityContext from config: {e}"))
        .ok()?;

    Some(AuthenticationResult {
        security_context: ctx,
    })
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "service_tests.rs"]
mod service_tests;
