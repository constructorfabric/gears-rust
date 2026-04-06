//! Service implementation for the static `AuthN` resolver plugin.

use std::collections::HashMap;

use modkit_macros::domain_model;
use modkit_security::SecurityContext;
use secrecy::{ExposeSecret, SecretString};

use crate::config::{AuthNMode, IdentityConfig, StaticAuthNPluginConfig};
use authn_resolver_sdk::{AuthenticationResult, ClientCredentialsRequest};

/// Static `AuthN` resolver service.
///
/// Provides token-to-identity mapping based on configuration mode:
/// - `accept_all`: Any non-empty token maps to the default identity
/// - `static_tokens`: Specific tokens map to specific identities
#[domain_model]
pub struct Service {
    mode: AuthNMode,
    default_identity: IdentityConfig,
    token_map: HashMap<String, IdentityConfig>,
    s2s_credentials: HashMap<String, S2sEntry>,
}

/// Internal entry for S2S credential lookup.
#[domain_model]
struct S2sEntry {
    client_secret: SecretString,
    identity: IdentityConfig,
}

impl Service {
    /// Create a service from plugin configuration.
    #[must_use]
    pub fn from_config(cfg: &StaticAuthNPluginConfig) -> Self {
        let token_map: HashMap<String, IdentityConfig> = cfg
            .tokens
            .iter()
            .map(|m| (m.token.clone(), m.identity.clone()))
            .collect();

        let s2s_credentials: HashMap<String, S2sEntry> = cfg
            .s2s_credentials
            .iter()
            .map(|m| {
                (
                    m.client_id.clone(),
                    S2sEntry {
                        client_secret: SecretString::from(
                            m.client_secret.expose_secret().to_owned(),
                        ),
                        identity: m.identity.clone(),
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

        build_result(identity, Some(bearer_token))
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
        build_result(&entry.identity, None)
    }
}

fn build_result(
    identity: &IdentityConfig,
    bearer_token: Option<&str>,
) -> Option<AuthenticationResult> {
    let mut builder = SecurityContext::builder()
        .subject_id(identity.subject_id)
        .subject_tenant_id(identity.subject_tenant_id)
        .token_scopes(identity.token_scopes.clone());

    if let Some(st) = &identity.subject_type {
        builder = builder.subject_type(st);
    }
    if let Some(token) = bearer_token {
        builder = builder.bearer_token(token.to_owned());
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
mod tests;
