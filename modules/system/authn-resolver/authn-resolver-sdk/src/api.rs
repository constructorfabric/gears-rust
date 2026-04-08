//! Public API trait for the `AuthN` resolver.
//!
//! This trait defines the interface that consumers use to authenticate
//! bearer tokens. The resolver implements this trait and delegates
//! to the appropriate plugin.

use async_trait::async_trait;

use crate::error::AuthNResolverError;
use crate::models::{AuthenticationResult, ClientCredentialsRequest};

/// Public API trait for the `AuthN` resolver.
///
/// This trait is registered in `ClientHub` by the module and
/// can be consumed by other modules (primarily the API gateway):
///
/// ```ignore
/// let authn = hub.get::<dyn AuthNResolverClient>()?;
///
/// // Authenticate a bearer token
/// let result = authn.authenticate("Bearer xyz...").await?;
/// let ctx = result.security_context;
/// ```
///
/// # Security
///
/// The returned `SecurityContext` includes the original bearer token
/// in the `bearer_token` field for downstream PDP forwarding.
#[async_trait]
pub trait AuthNResolverClient: Send + Sync {
    /// Authenticate a bearer token and return the validated identity.
    ///
    /// # Arguments
    ///
    /// * `bearer_token` - The raw bearer token string (without "Bearer " prefix)
    ///
    /// # Errors
    ///
    /// - `Unauthorized` if the token is invalid, expired, or malformed
    /// - `NoPluginAvailable` if no `AuthN` plugin is registered
    /// - `ServiceUnavailable` if the plugin is not ready
    /// - `Internal` for unexpected errors
    async fn authenticate(
        &self,
        bearer_token: &str,
    ) -> Result<AuthenticationResult, AuthNResolverError>;

    /// Exchange `OAuth2` client credentials for a validated `SecurityContext`.
    ///
    /// Used for service-to-service (S2S) communication when there is no
    /// incoming HTTP request with a bearer token.
    ///
    /// The caller provides its credentials; the `AuthN` plugin knows the
    /// token endpoint / issuer URL from its own configuration.
    ///
    /// # Scopes
    ///
    /// The `scopes` field is passed to the `IdP` as-is in the `OAuth2`
    /// `client_credentials` grant `scope` parameter. The `IdP` determines
    /// the final granted scopes. Plugins that do not interact with an
    /// `IdP` (e.g., static dev plugins) may ignore this field.
    ///
    /// # Errors
    ///
    /// - `TokenAcquisitionFailed` if credentials are invalid or the `IdP` is unreachable
    /// - `NoPluginAvailable` if no `AuthN` plugin is registered
    /// - `ServiceUnavailable` if the plugin is not ready
    /// - `Internal` for unexpected errors
    async fn exchange_client_credentials(
        &self,
        request: &ClientCredentialsRequest,
    ) -> Result<AuthenticationResult, AuthNResolverError>;
}
