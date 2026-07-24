use std::time::Duration;

use thiserror::Error;
use toolkit_macros::domain_model;

type BoxError = Box<dyn std::error::Error + Send + Sync + 'static>;

#[domain_model]
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum DomainError {
    #[error("invalid secret reference: {detail}")]
    InvalidSecretRef { detail: String },
    #[error("secret not found")]
    NotFound,
    #[error("secret already exists")]
    Conflict,
    #[error("version precondition failed")]
    VersionConflict,
    #[error("invalid precondition: {detail}")]
    InvalidPrecondition { detail: String },
    #[error("unsupported sharing transition: {detail}")]
    UnsupportedTransition { detail: String },
    /// A write violated the secret type's traits. `reason` is the stable
    /// machine-readable code surfaced on the wire (e.g.
    /// `SHARING_NOT_ALLOWED_FOR_TYPE`); `field` names the offending request
    /// field for the canonical field violation.
    #[error("secret type violation ({reason}): {detail}")]
    TypeViolation {
        field: &'static str,
        reason: &'static str,
        detail: String,
    },
    #[error("access denied")]
    AccessDenied {
        #[source]
        cause: Option<BoxError>,
    },
    #[error("service unavailable: {detail}")]
    ServiceUnavailable {
        detail: String,
        retry_after: Option<Duration>,
        #[source]
        cause: Option<BoxError>,
    },
    #[error("internal error")]
    Internal {
        diagnostic: String,
        #[source]
        cause: Option<BoxError>,
    },
}

impl DomainError {
    #[must_use]
    pub fn internal(diagnostic: impl Into<String>) -> Self {
        Self::Internal {
            diagnostic: diagnostic.into(),
            cause: None,
        }
    }
}
