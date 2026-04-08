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
mod tests {
    use secrecy::{ExposeSecret, SecretString};

    use super::*;
    use crate::config::{S2sCredentialMapping, TokenMapping};
    use uuid::Uuid;

    fn default_config() -> StaticAuthNPluginConfig {
        StaticAuthNPluginConfig::default()
    }

    #[test]
    fn accept_all_mode_returns_default_identity() {
        let service = Service::from_config(&default_config());

        let result = service.authenticate("any-token-value");
        assert!(result.is_some());

        let auth = result.unwrap();
        let ctx = &auth.security_context;
        assert_eq!(
            ctx.subject_id(),
            modkit_security::constants::DEFAULT_SUBJECT_ID
        );
        assert_eq!(
            ctx.subject_tenant_id(),
            modkit_security::constants::DEFAULT_TENANT_ID
        );
        assert_eq!(ctx.token_scopes(), &["*"]);
        assert_eq!(
            ctx.bearer_token().map(ExposeSecret::expose_secret),
            Some("any-token-value"),
        );
    }

    #[test]
    fn accept_all_mode_rejects_empty_token() {
        let service = Service::from_config(&default_config());

        let result = service.authenticate("");
        assert!(result.is_none());
    }

    #[test]
    fn static_tokens_mode_returns_mapped_identity() {
        let user_a_id = Uuid::parse_str("aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa").unwrap();
        let tenant_a = Uuid::parse_str("bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb").unwrap();

        let cfg = StaticAuthNPluginConfig {
            mode: AuthNMode::StaticTokens,
            tokens: vec![TokenMapping {
                token: "token-user-a".to_owned(),
                identity: IdentityConfig {
                    subject_id: user_a_id,
                    subject_tenant_id: tenant_a,
                    token_scopes: vec!["read:data".to_owned()],
                    subject_type: None,
                },
            }],
            ..default_config()
        };

        let service = Service::from_config(&cfg);

        let result = service.authenticate("token-user-a");
        assert!(result.is_some());

        let auth = result.unwrap();
        let ctx = &auth.security_context;
        assert_eq!(ctx.subject_id(), user_a_id);
        assert_eq!(ctx.subject_tenant_id(), tenant_a);
        assert_eq!(ctx.token_scopes(), &["read:data"]);
        assert_eq!(
            ctx.bearer_token().map(ExposeSecret::expose_secret),
            Some("token-user-a"),
        );
    }

    #[test]
    fn static_tokens_mode_rejects_unknown_token() {
        let cfg = StaticAuthNPluginConfig {
            mode: AuthNMode::StaticTokens,
            tokens: vec![TokenMapping {
                token: "known-token".to_owned(),
                identity: IdentityConfig::default(),
            }],
            ..default_config()
        };

        let service = Service::from_config(&cfg);

        let result = service.authenticate("unknown-token");
        assert!(result.is_none());
    }

    #[test]
    fn static_tokens_mode_rejects_empty_token() {
        let cfg = StaticAuthNPluginConfig {
            mode: AuthNMode::StaticTokens,
            tokens: vec![],
            ..default_config()
        };

        let service = Service::from_config(&cfg);

        let result = service.authenticate("");
        assert!(result.is_none());
    }

    #[test]
    fn subject_type_propagated_in_security_context() {
        let cfg = StaticAuthNPluginConfig {
            default_identity: IdentityConfig {
                subject_type: Some("user".to_owned()),
                ..IdentityConfig::default()
            },
            ..default_config()
        };

        let service = Service::from_config(&cfg);
        let result = service.authenticate("any-token").unwrap();
        assert_eq!(result.security_context.subject_type(), Some("user"));
    }

    #[test]
    fn subject_type_none_when_not_configured() {
        let service = Service::from_config(&default_config());
        let result = service.authenticate("any-token").unwrap();
        assert_eq!(result.security_context.subject_type(), None);
    }

    fn s2s_config() -> StaticAuthNPluginConfig {
        let svc_id = Uuid::parse_str("cccccccc-cccc-cccc-cccc-cccccccccccc").unwrap();
        let svc_tenant = Uuid::parse_str("dddddddd-dddd-dddd-dddd-dddddddddddd").unwrap();

        StaticAuthNPluginConfig {
            s2s_credentials: vec![S2sCredentialMapping {
                client_id: "my-service".to_owned(),
                client_secret: SecretString::from("my-secret"),
                identity: IdentityConfig {
                    subject_id: svc_id,
                    subject_tenant_id: svc_tenant,
                    token_scopes: vec!["platform.internal".to_owned()],
                    subject_type: Some("service".to_owned()),
                },
            }],
            ..default_config()
        }
    }

    #[test]
    fn s2s_exchange_returns_identity_for_valid_credentials() {
        let service = Service::from_config(&s2s_config());

        let request = ClientCredentialsRequest {
            client_id: "my-service".to_owned(),
            client_secret: SecretString::from("my-secret"),
            scopes: vec![],
        };

        let result = service.exchange_client_credentials(&request);
        assert!(result.is_some());

        let auth = result.unwrap();
        let ctx = &auth.security_context;
        assert_eq!(
            ctx.subject_id(),
            Uuid::parse_str("cccccccc-cccc-cccc-cccc-cccccccccccc").unwrap()
        );
        assert_eq!(
            ctx.subject_tenant_id(),
            Uuid::parse_str("dddddddd-dddd-dddd-dddd-dddddddddddd").unwrap()
        );
        assert_eq!(ctx.token_scopes(), &["platform.internal"]);
        assert_eq!(ctx.subject_type(), Some("service"));
        // S2S exchange does not set bearer_token (no real token issued)
        assert!(ctx.bearer_token().is_none());
    }

    #[test]
    fn s2s_exchange_rejects_wrong_secret() {
        let service = Service::from_config(&s2s_config());

        let request = ClientCredentialsRequest {
            client_id: "my-service".to_owned(),
            client_secret: SecretString::from("wrong-secret"),
            scopes: vec![],
        };

        let result = service.exchange_client_credentials(&request);
        assert!(result.is_none());
    }

    #[test]
    fn s2s_exchange_rejects_unknown_client_id() {
        let service = Service::from_config(&s2s_config());

        let request = ClientCredentialsRequest {
            client_id: "unknown-service".to_owned(),
            client_secret: SecretString::from("my-secret"),
            scopes: vec![],
        };

        let result = service.exchange_client_credentials(&request);
        assert!(result.is_none());
    }

    #[test]
    fn s2s_exchange_returns_none_with_no_credentials_configured() {
        let service = Service::from_config(&default_config());

        let request = ClientCredentialsRequest {
            client_id: "any-service".to_owned(),
            client_secret: SecretString::from("any-secret"),
            scopes: vec![],
        };

        let result = service.exchange_client_credentials(&request);
        assert!(result.is_none());
    }
}
