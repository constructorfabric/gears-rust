//! Plugin API trait for `AuthN` resolver implementations.
//!
//! Plugins implement this trait to provide token validation.
//! The gateway discovers plugins via GTS types-registry and delegates
//! API calls to the selected plugin.

use async_trait::async_trait;

use crate::error::AuthNResolverError;
use crate::models::{AuthenticationResult, ClientCredentialsRequest};

/// Plugin API trait for `AuthN` resolver implementations.
///
/// Each plugin registers this trait with a scoped `ClientHub` entry
/// using its GTS instance ID as the scope.
///
/// The gateway delegates to this method. Cross-cutting concerns (logging,
/// metrics) may be added at the gateway level in the future.
#[async_trait]
pub trait AuthNResolverPluginClient: Send + Sync {
    /// Authenticate a bearer token and return the validated identity.
    ///
    /// # Arguments
    ///
    /// * `bearer_token` - The raw bearer token string
    ///
    /// # Errors
    ///
    /// - `Unauthorized` if the token is invalid, expired, or malformed
    /// - `Internal` for unexpected errors
    async fn authenticate(
        &self,
        bearer_token: &str,
    ) -> Result<AuthenticationResult, AuthNResolverError>;

    /// Exchange client credentials for an `AuthenticationResult`.
    ///
    /// The plugin performs the actual `OAuth2` `client_credentials` flow
    /// (or static credential lookup) and returns an `AuthenticationResult`
    /// containing the validated `SecurityContext`.
    ///
    /// # Scopes
    ///
    /// Production plugins forward `scopes` to the `IdP` as-is in the
    /// `OAuth2` `scope` parameter. Plugins that do not interact with an
    /// `IdP` (e.g., static dev plugins) may ignore this field.
    ///
    /// # Errors
    ///
    /// - `TokenAcquisitionFailed` if credentials are invalid or `IdP` is unreachable
    /// - `Internal` for unexpected errors
    async fn exchange_client_credentials(
        &self,
        request: &ClientCredentialsRequest,
    ) -> Result<AuthenticationResult, AuthNResolverError>;
}
