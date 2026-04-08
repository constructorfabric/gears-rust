//! Domain models for the `AuthN` resolver module.

use secrecy::SecretString;

use modkit_security::SecurityContext;

/// Result of a successful authentication.
///
/// Contains the validated `SecurityContext` with identity information
/// populated from the token (`subject_id`, `subject_tenant_id`, `token_scopes`, etc.).
#[derive(Debug, Clone)]
pub struct AuthenticationResult {
    /// The validated security context with identity fields populated.
    ///
    /// Contains:
    /// - `subject_id` — The authenticated user/service ID
    /// - `subject_tenant_id` — The subject's home tenant
    /// - `token_scopes` — Token capability restrictions
    /// - `bearer_token` — Original token for PDP forwarding
    /// - `tenant_id` — Context tenant (may be set by `AuthN` or later by middleware)
    pub security_context: SecurityContext,
}

/// Request to exchange `OAuth2` client credentials for a `SecurityContext`.
///
/// The caller provides its credentials; the `AuthN` plugin knows the token
/// endpoint / issuer URL from its own configuration.
pub struct ClientCredentialsRequest {
    /// `OAuth2` client identifier.
    pub client_id: String,

    /// `OAuth2` client secret.
    pub client_secret: SecretString,

    /// Optional scopes to request from the `IdP`.
    /// Passed as `scope` parameter in the `OAuth2` `client_credentials` grant.
    /// When empty, the `IdP` returns its default scopes.
    pub scopes: Vec<String>,
}
