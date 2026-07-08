//! `AuthN` Resolver bearer adapter.
//!
//! Bridges the gear-facing [`AuthNResolverClient`] to the transport-agnostic
//! [`BearerAuthenticator`] consumed by
//! `toolkit_http_middleware::security_context_middleware`, keeping the shared
//! middleware free of any dependency on the `AuthN` Resolver SDK.

use std::sync::Arc;

use authn_resolver_sdk::{AuthNResolverClient, AuthNResolverError};
use toolkit_security::{AuthNError, BearerAuthenticator, SecurityContext};

/// Adapts the gear-facing [`AuthNResolverClient`] to the transport-agnostic
/// [`BearerAuthenticator`] consumed by
/// [`toolkit_http_middleware::security_context_middleware`].
///
/// This keeps the shared middleware free of any dependency on the `AuthN`
/// Resolver SDK: the gateway owns the mapping from `AuthNResolverError` onto the
/// neutral [`AuthNError`].
pub struct AuthNResolverBearerAdapter {
    client: Arc<dyn AuthNResolverClient>,
}

impl AuthNResolverBearerAdapter {
    /// Wrap an [`AuthNResolverClient`] as a [`BearerAuthenticator`].
    #[must_use]
    pub fn new(client: Arc<dyn AuthNResolverClient>) -> Self {
        Self { client }
    }
}

impl BearerAuthenticator for AuthNResolverBearerAdapter {
    async fn authenticate(&self, token: &str) -> Result<SecurityContext, AuthNError> {
        match self.client.authenticate(token).await {
            Ok(result) => Ok(result.security_context),
            Err(err) => {
                log_authn_error(&err);
                Err(map_authn_error(&err))
            }
        }
    }
}

/// Map an `AuthNResolverError` onto the neutral [`AuthNError`] consumed by the
/// shared middleware, which renders the canonical `problem+json` response and
/// RFC 6750 challenge. No provider-specific detail is surfaced on the wire.
fn map_authn_error(err: &AuthNResolverError) -> AuthNError {
    match err {
        AuthNResolverError::Unauthorized(_) => AuthNError::InvalidToken,
        AuthNResolverError::NoPluginAvailable | AuthNResolverError::ServiceUnavailable(_) => {
            AuthNError::Unavailable
        }
        AuthNResolverError::TokenAcquisitionFailed(msg) | AuthNResolverError::Internal(msg) => {
            AuthNError::Other(msg.clone())
        }
    }
}

/// Log authentication errors at appropriate levels.
///
/// Cognitive complexity is inflated by tracing macro expansion.
#[allow(clippy::cognitive_complexity)]
fn log_authn_error(err: &AuthNResolverError) {
    match err {
        AuthNResolverError::Unauthorized(msg) => tracing::debug!("AuthN rejected: {msg}"),
        AuthNResolverError::NoPluginAvailable => tracing::error!("No AuthN plugin available"),
        AuthNResolverError::ServiceUnavailable(msg) => {
            tracing::error!("AuthN service unavailable: {msg}");
        }
        AuthNResolverError::TokenAcquisitionFailed(msg) => {
            tracing::error!("AuthN token acquisition failed: {msg}");
        }
        AuthNResolverError::Internal(msg) => tracing::error!("AuthN internal error: {msg}"),
    }
}
