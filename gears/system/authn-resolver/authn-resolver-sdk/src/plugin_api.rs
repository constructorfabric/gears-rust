//! Plugin API trait for `AuthN` resolver implementations.
//!
//! Plugins implement this trait to provide token validation.
//! The gateway discovers plugins via GTS types-registry and delegates
//! API calls to the selected plugin.

use async_trait::async_trait;

use crate::error::AuthNResolverError;
use crate::models::{AuthenticationRequest, AuthenticationResult, ClientCredentialsRequest};

/// Plugin API trait for `AuthN` resolver implementations.
///
/// Each plugin registers this trait with a scoped `ClientHub` entry
/// using its GTS instance ID as the scope.
///
/// The gateway delegates to this method. Cross-cutting concerns (logging,
/// metrics) may be added at the gateway level in the future.
#[async_trait]
pub trait AuthNResolverPluginClient: Send + Sync {
    /// Validate `request.bearer_token` and return the validated identity.
    ///
    /// # Caching
    ///
    /// A plugin with its own authenticate cache must honour
    /// `request.cache_control` ([`crate::RequestCacheControl`]): `AllowCached`
    /// permits a cached result; `Refresh` without a tag forbids one; `Refresh`
    /// with a tag permits one only if it is provably newer than that tag.
    /// A plugin without such a cache validates every call, which satisfies
    /// every value. Opt into caller reuse via `AuthenticationResult::cache_control`
    /// (default no-store), optionally tagged for targeted refresh; never past
    /// the credential's expiry (`SecurityContext::expires_at`).
    ///
    /// # Errors
    ///
    /// - `Unauthorized` if the token is invalid, expired, or malformed
    /// - `ServiceUnavailable` for transient dependency failures
    /// - `Internal` for unexpected errors
    async fn authenticate(
        &self,
        request: &AuthenticationRequest,
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
    /// # Caching
    ///
    /// Honour `request.cache_control` ([`crate::RequestCacheControl`]), including
    /// refresh requests. Opt into caller reuse via `AuthenticationResult::cache_control`
    /// (default no-store). Set `SecurityContext::expires_at` to the token's
    /// original absolute expiry; cache hits must not extend it.
    ///
    /// # Errors
    ///
    /// - `TokenAcquisitionFailed` if credentials are rejected or the obtained token is unusable
    /// - `ServiceUnavailable` if the `IdP` is unreachable (transient)
    /// - `Internal` for unexpected errors
    async fn exchange_client_credentials(
        &self,
        request: &ClientCredentialsRequest,
    ) -> Result<AuthenticationResult, AuthNResolverError>;
}
