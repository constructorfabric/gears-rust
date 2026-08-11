//! Domain error model for the token-issuer.

use thiserror::Error;
use token_issuer_sdk::TokenIssuerError;
use toolkit_macros::domain_model;

use crate::domain::downscope::DownscopeError;

/// Errors raised by the token-issuer domain layer.
#[domain_model]
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum DomainError {
    /// The mint request was malformed (bad audience, key reference, etc.).
    #[error("invalid request: {detail}")]
    InvalidRequest {
        /// Human-readable reason.
        detail: String,
    },
    /// Signing failed at the backing signing port.
    #[error("signing failed: {detail}")]
    Signing {
        /// Human-readable reason.
        detail: String,
    },
    /// The issuer has not finished warming up (keys/JWKS not yet available).
    #[error("token issuer not ready")]
    NotReady,
    /// The presented capability token failed provenance verification (bad
    /// signature, wrong issuer/type/alg/kid, or expired).
    #[error("capability token invalid: {detail}")]
    CapInvalid {
        /// Which check failed (not surfaced to clients verbatim).
        detail: String,
    },
    /// The calling peer could not be verified (no mTLS client certificate).
    #[error("peer not verified")]
    PeerUnverified,
    /// The verified peer is not a known adapter in the registry.
    #[error("peer not a known adapter")]
    PeerUnknown,
    /// The capability token's audience does not match the calling peer.
    #[error("capability audience does not match peer")]
    PeerMismatch,
    /// The target adapter is not in an active state.
    #[error("adapter not active")]
    AdapterInactive,
    /// The adapter has no operator grant for OBO callbacks.
    #[error("adapter not granted OBO callbacks")]
    OboNotGranted,
    /// The presented bearer is itself an OBO token (re-entry refused).
    #[error("OBO re-entry refused")]
    LoopGuard,
    /// OBO issuance is disabled by configuration.
    #[error("OBO issuance disabled")]
    OboDisabled,
    /// An unexpected internal error.
    #[error("internal error")]
    Internal {
        /// Internal diagnostic (not surfaced to clients verbatim).
        diagnostic: String,
    },
}

impl DomainError {
    /// Convenience constructor for [`DomainError::Internal`].
    #[must_use]
    pub fn internal(diagnostic: impl Into<String>) -> Self {
        Self::Internal {
            diagnostic: diagnostic.into(),
        }
    }

    /// Convenience constructor for [`DomainError::CapInvalid`].
    #[must_use]
    pub fn cap_invalid(detail: impl Into<String>) -> Self {
        Self::CapInvalid {
            detail: detail.into(),
        }
    }
}

/// Maps a Gate-2 down-scope failure to a domain error. Both an empty
/// intersection and an over-broad request are authorization failures (403) from
/// the caller's perspective, so they surface as [`DomainError::OboNotGranted`].
impl From<DownscopeError> for DomainError {
    fn from(err: DownscopeError) -> Self {
        match err {
            DownscopeError::EmptyIntersection | DownscopeError::NotSubset => Self::OboNotGranted,
        }
    }
}

/// Maps an SDK signing/serialization error (from the OBO mint closure) into the
/// domain error model. Signing failures are transient (503); the rest are
/// internal.
impl From<TokenIssuerError> for DomainError {
    fn from(err: TokenIssuerError) -> Self {
        match err {
            TokenIssuerError::InvalidRequest { reason } => Self::InvalidRequest { detail: reason },
            TokenIssuerError::Signing(e) => Self::Signing {
                detail: e.to_string(),
            },
            TokenIssuerError::Internal(diagnostic) => Self::Internal { diagnostic },
        }
    }
}
