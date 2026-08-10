//! Request/response models for the service-principal SPI.

use secrecy::SecretString;
use uuid::Uuid;

pub use tenant_resolver_sdk::TenantId;

/// Request to create a confidential `client_credentials` service client.
#[derive(Debug, Clone)]
pub struct CreateServicePrincipalRequest {
    /// Tenant that owns the principal; lands in the token's `tenant_id`.
    pub tenant_id: TenantId,
    /// Short caller-chosen suffix (lowercase alnum + '-', max 40 chars); the
    /// adapter builds the final client id as `svc-<tenant_id>-<name>`.
    pub name: String,
    /// Client scopes to attach; validated against the adapter allowlist.
    pub scopes: Vec<String>,
}

/// Live credentials. The secret is returned ONLY by create/rotate —
/// persist it immediately; recovery from loss is `rotate_secret`.
///
/// `client_secret` is a [`secrecy::SecretString`] (workspace MUST-wrap rule
/// for secret-shaped strings): redacted `Debug`, zeroize-on-drop, no Serde.
#[derive(Debug)]
pub struct ServicePrincipalCredentials {
    pub client_id: String,
    pub client_secret: SecretString,
    /// OAuth token endpoint for the `client_credentials` grant.
    pub token_url: String,
    /// The principal's subject id (`sub` claim of issued tokens — the
    /// IdP/service-account user UUID). Use it for RBAC bindings.
    pub subject_id: Uuid,
}

/// Listing entry — no secrets.
#[derive(Debug, Clone)]
pub struct ServicePrincipalSummary {
    pub client_id: String,
    pub enabled: bool,
    /// Attached client scopes as reported by the `IdP` — includes realm-default scopes, not only consumer-requested ones.
    pub scopes: Vec<String>,
}

#[cfg(test)]
mod tests {
    use secrecy::ExposeSecret as _;

    use super::*;

    #[test]
    fn credentials_debug_hides_secret() {
        let creds = ServicePrincipalCredentials {
            client_id: "svc-abc".into(),
            client_secret: SecretString::from("super-secret".to_owned()),
            token_url: "https://example.com/token".into(),
            subject_id: Uuid::nil(),
        };
        assert!(!format!("{creds:?}").contains("super-secret"));
        assert_eq!(creds.client_secret.expose_secret(), "super-secret");
    }
}
