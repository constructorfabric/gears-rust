//! Public API trait for the `AuthN` resolver.
//!
//! This trait defines the interface that consumers use to authenticate
//! bearer tokens. The resolver implements this trait and delegates
//! to the appropriate plugin.

use async_trait::async_trait;

use crate::error::AuthNResolverError;
use crate::models::{AuthenticationRequest, AuthenticationResult, ClientCredentialsRequest};

/// Public API trait for the `AuthN` resolver.
///
/// This trait is registered in `ClientHub` by the gear and
/// can be consumed by other gears (primarily the API gateway):
///
/// ```no_run
/// # use std::sync::Arc;
/// # use authn_resolver_sdk::{AuthNResolverClient, AuthenticationRequest};
/// # use secrecy::SecretString;
/// # async fn example(authn: Arc<dyn AuthNResolverClient>) -> Result<(), Box<dyn std::error::Error>> {
/// // `authn` is `hub.get::<dyn AuthNResolverClient>()?`.
/// // Pass the raw token, without the `Bearer ` prefix; `new` always revalidates.
/// let request = AuthenticationRequest::new(SecretString::from("eyJhbGciOiJSUzI1NiJ9..."));
/// let result = authn.authenticate(&request).await?;
/// let ctx = result.security_context;
/// # let _ = ctx;
/// # Ok(())
/// # }
/// ```
///
/// # Security
///
/// The returned `SecurityContext` includes the original bearer token
/// in the `bearer_token` field for downstream PDP forwarding.
#[async_trait]
pub trait AuthNResolverClient: Send + Sync {
    /// Authenticate `request.bearer_token` (without the `Bearer ` prefix) and
    /// return the validated identity.
    ///
    /// `request.cache_control` governs plugin-cache reuse and is forwarded to
    /// the plugin unchanged: [`AuthenticationRequest::new`] requires a fresh
    /// validation; a caller that accepts a plugin-cached result sets
    /// [`crate::RequestCacheControl::AllowCached`]. The result's
    /// `cache_control` governs caller reuse (default no-store);
    /// [`crate::CachingAuthenticator`] uses both.
    ///
    /// # Errors
    ///
    /// - `Unauthorized` if the token is invalid, expired, or malformed
    /// - `NoPluginAvailable` if no `AuthN` plugin is registered
    /// - `ServiceUnavailable` if the plugin is not ready
    /// - `Internal` for unexpected errors
    async fn authenticate(
        &self,
        request: &AuthenticationRequest,
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
    /// # Caching
    ///
    /// [`crate::RequestCacheControl`] governs plugin-cache reuse; the result's
    /// `cache_control` governs caller reuse (default no-store).
    /// Use [`crate::ClientCredentialsProvider`] for repeated acquisitions.
    ///
    /// # Errors
    ///
    /// - `TokenAcquisitionFailed` if credentials are rejected or the obtained token is unusable
    /// - `NoPluginAvailable` if no `AuthN` plugin is registered
    /// - `ServiceUnavailable` if the plugin is not ready or the `IdP` is
    ///   unreachable (transient)
    /// - `Internal` for unexpected errors
    async fn exchange_client_credentials(
        &self,
        request: &ClientCredentialsRequest,
    ) -> Result<AuthenticationResult, AuthNResolverError>;
}
