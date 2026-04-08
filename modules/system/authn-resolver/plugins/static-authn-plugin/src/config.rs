//! Configuration for the static `AuthN` resolver plugin.

use secrecy::SecretString;
use serde::Deserialize;
use uuid::Uuid;

use modkit_security::constants::{DEFAULT_SUBJECT_ID, DEFAULT_TENANT_ID};

/// Plugin configuration.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct StaticAuthNPluginConfig {
    /// Vendor name for GTS instance registration.
    pub vendor: String,

    /// Plugin priority (lower = higher priority).
    pub priority: i16,

    /// Authentication mode.
    pub mode: AuthNMode,

    /// Default identity returned in `accept_all` mode.
    pub default_identity: IdentityConfig,

    /// Static token-to-identity mappings for `static_tokens` mode.
    pub tokens: Vec<TokenMapping>,

    /// S2S credential-to-identity mappings for `exchange_client_credentials`.
    pub s2s_credentials: Vec<S2sCredentialMapping>,
}

impl Default for StaticAuthNPluginConfig {
    fn default() -> Self {
        Self {
            vendor: "hyperspot".to_owned(),
            priority: 100,
            mode: AuthNMode::AcceptAll,
            default_identity: IdentityConfig::default(),
            tokens: Vec::new(),
            s2s_credentials: Vec::new(),
        }
    }
}

/// Authentication mode.
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum AuthNMode {
    /// Accept any non-empty token and return the default identity.
    #[default]
    AcceptAll,
    /// Map specific tokens to specific identities.
    StaticTokens,
}

/// Identity configuration for a subject.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct IdentityConfig {
    /// Subject ID (user/service).
    pub subject_id: Uuid,

    /// Subject's home tenant.
    pub subject_tenant_id: Uuid,

    /// Token scopes. `["*"]` means first-party / unrestricted.
    pub token_scopes: Vec<String>,

    /// Subject type — opaque metadata passed through to PDP via `EvaluationRequest.Subject`.
    /// Recommended format: GTS type identifier (e.g. `"gts.x.core.security.subject_user.v1~"`).
    /// The platform does not interpret this value; PDP policies may use it for role/permission mapping.
    pub subject_type: Option<String>,
}

impl Default for IdentityConfig {
    fn default() -> Self {
        Self {
            subject_id: DEFAULT_SUBJECT_ID,
            subject_tenant_id: DEFAULT_TENANT_ID,
            token_scopes: vec!["*".to_owned()],
            subject_type: None,
        }
    }
}

/// Maps a static token to a specific identity.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TokenMapping {
    /// The bearer token value to match.
    pub token: String,
    /// The identity to return when this token is presented.
    pub identity: IdentityConfig,
}

/// Maps S2S client credentials to a specific identity.
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct S2sCredentialMapping {
    /// `OAuth2` client identifier.
    pub client_id: String,
    /// Client secret (redacted in `Debug` output).
    pub client_secret: SecretString,
    /// The identity to return when these credentials are presented.
    /// When omitted, uses the default identity (`DEFAULT_SUBJECT_ID` / `DEFAULT_TENANT_ID`).
    #[serde(default)]
    pub identity: IdentityConfig,
}

impl std::fmt::Debug for S2sCredentialMapping {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("S2sCredentialMapping")
            .field("client_id", &self.client_id)
            .field("client_secret", &"[REDACTED]")
            .field("identity", &self.identity)
            .finish()
    }
}
