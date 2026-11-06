//! Error types for the `AuthN` resolver gear

use thiserror::Error;

/// Errors that can occur when using the `AuthN` resolver API.
#[derive(Debug, Error)]
pub enum AuthNResolverError {
    /// The token is invalid, expired, or malformed.
    #[error("unauthorized: {0}")]
    Unauthorized(String),

    /// No `AuthN` plugin is available to handle the request.
    #[error("no plugin available")]
    NoPluginAvailable,

    /// Transient dependency failure: plugin not ready, unreachable `IdP`,
    /// exhausted retries or open circuit breaker. Allows retry
    /// and fallback to a still-usable cached result.
    #[error("service unavailable: {0}")]
    ServiceUnavailable(String),

    /// Non-transient exchange failure: rejected credentials, non-retryable
    /// endpoint status, malformed response or unusable token.
    /// Unreachable `IdP` dependencies use [`Self::ServiceUnavailable`].
    #[error("token acquisition failed: {0}")]
    TokenAcquisitionFailed(String),

    /// An internal error occurred.
    #[error("internal error: {0}")]
    Internal(String),
}
