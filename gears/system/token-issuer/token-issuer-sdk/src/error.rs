use std::time::Duration;

use thiserror::Error;

/// Errors that can occur during signing operations.
#[derive(Debug, Error)]
pub enum SigningError {
    #[error("invalid key reference: {reason}")]
    InvalidKeyRef { reason: String },
    #[error("signing key not found")]
    NotFound,
    #[error("no signing plugin available")]
    NoPluginAvailable,
    #[error("service unavailable: {detail}")]
    ServiceUnavailable {
        detail: String,
        retry_after: Option<Duration>,
    },
    #[error("internal error: {0}")]
    Internal(String),
}

impl SigningError {
    #[must_use]
    pub fn internal(msg: impl Into<String>) -> Self {
        Self::Internal(msg.into())
    }

    #[must_use]
    pub fn service_unavailable(detail: impl Into<String>) -> Self {
        Self::ServiceUnavailable {
            detail: detail.into(),
            retry_after: None,
        }
    }

    /// `true` if the operation may succeed on a future retry.
    #[must_use]
    pub fn is_retryable(&self) -> bool {
        matches!(
            self,
            Self::ServiceUnavailable { .. } | Self::NoPluginAvailable
        )
    }

    /// `true` for signing key not found.
    #[must_use]
    pub fn is_not_found(&self) -> bool {
        matches!(self, Self::NotFound)
    }
}

/// Errors that can occur during token-issuer operations.
#[derive(Debug, Error)]
pub enum TokenIssuerError {
    #[error("invalid request: {reason}")]
    InvalidRequest { reason: String },
    #[error(transparent)]
    Signing(#[from] SigningError),
    #[error("internal error: {0}")]
    Internal(String),
}
