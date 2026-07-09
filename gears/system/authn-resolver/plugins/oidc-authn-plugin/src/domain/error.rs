//! Error types for the Oidc `AuthN` plugin.
//!
//! Internal errors are typed as [`AuthNError`] for rich diagnostics and metrics.
//! They are mapped to the gateway's `AuthNResolverError` at the plugin boundary.

use authn_resolver_sdk::AuthNResolverError;
use thiserror::Error;
use toolkit_macros::domain_model;
use tracing::debug;

/// Internal, richly-typed error enum for the Oidc `AuthN` plugin.
///
/// All variants are mapped to an appropriate `AuthNResolverError` at the plugin
/// boundary: validation errors become `Unauthorized`; connectivity errors become
/// `ServiceUnavailable`.
#[domain_model]
#[derive(Debug, Error)]
pub enum AuthNError {
    /// JWT signature verification failed.
    #[error("signature invalid")]
    SignatureInvalid,

    /// Bearer token is not a JWT and opaque token introspection is unsupported.
    #[error("unsupported token format")]
    UnsupportedTokenFormat,

    /// JWT `exp` claim is in the past.
    #[error("token expired")]
    TokenExpired,

    /// JWT `iss` claim is not in the trusted issuers list.
    #[error("untrusted issuer")]
    UntrustedIssuer,

    /// A required claim is absent from the token.
    #[error("missing claim: {0}")]
    MissingClaim(String),

    /// The `sub` claim is not a valid UUID.
    #[error("invalid subject id")]
    InvalidSubject,

    /// The JWT's `kid` was not found in the JWKS, even after a forced refresh.
    #[error("kid not found")]
    KidNotFound,

    /// The JWT uses an unsupported or disallowed algorithm (e.g. `alg: none`).
    #[error("unsupported algorithm")]
    UnsupportedAlgorithm,

    /// The JWT `aud` claim does not match the expected audience.
    #[error("invalid audience")]
    InvalidAudience,

    /// The JOSE `typ` header does not match the issuer's required token type.
    #[error("invalid token type")]
    InvalidTokenType,

    /// Oidc (or OIDC Discovery / JWKS endpoint) is unreachable.
    #[error("identity provider unreachable")]
    IdpUnreachable,

    /// S2S token endpoint returned a non-successful response status.
    #[error("token endpoint returned unsuccessful status: {0}")]
    TokenEndpointUnsuccessfulStatus(u16),

    /// S2S token endpoint response could not be parsed.
    #[error("token response parse failed")]
    TokenResponseParseFailed,

    /// S2S token endpoint is not configured.
    #[error("token endpoint not configured")]
    TokenEndpointNotConfigured,
}

impl AuthNError {
    /// Returns true when this error indicates IdP/connectivity degradation.
    #[must_use]
    pub fn is_idp_failure(&self) -> bool {
        matches!(self, Self::IdpUnreachable)
    }
}

impl From<AuthNError> for AuthNResolverError {
    fn from(value: AuthNError) -> Self {
        match value {
            AuthNError::MissingClaim(claim) => {
                log_missing_claim(&claim);
                AuthNResolverError::Unauthorized("missing claim".to_owned())
            }
            AuthNError::SignatureInvalid
            | AuthNError::UnsupportedTokenFormat
            | AuthNError::TokenExpired
            | AuthNError::UntrustedIssuer
            | AuthNError::InvalidSubject
            | AuthNError::KidNotFound
            | AuthNError::UnsupportedAlgorithm
            | AuthNError::InvalidAudience
            | AuthNError::InvalidTokenType => AuthNResolverError::Unauthorized(format!("{value}")),
            AuthNError::IdpUnreachable => {
                AuthNResolverError::ServiceUnavailable(format!("{value}"))
            }
            AuthNError::TokenEndpointUnsuccessfulStatus(_)
            | AuthNError::TokenResponseParseFailed
            | AuthNError::TokenEndpointNotConfigured => {
                AuthNResolverError::TokenAcquisitionFailed(format!("{value}"))
            }
        }
    }
}

fn log_missing_claim(claim: &str) {
    debug!(claim = %claim, "OIDC token missing required claim");
}

#[cfg(test)]
#[path = "error_tests.rs"]
mod error_tests;
