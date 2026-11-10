// Updated: 2026-10-06 by Constructor Tech
//! Internal credential-store error taxonomy.
//!
//! Domain failures retain operational causes while boundary adapters project
//! only curated SDK and canonical HTTP errors.

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
    /// The decisive record resolved and the caller may read its secret, but
    /// the record's `expires_at` has passed: the secret is never served, and
    /// resolution does not continue to an ancestor's value.
    #[error("secret expired")]
    SecretExpired,
    #[error("version precondition failed")]
    VersionConflict,
    #[error("invalid precondition: {detail}")]
    InvalidPrecondition { detail: String },
    /// An update or delete arrived without the mandatory optimistic-concurrency
    /// precondition (REST: no `If-Match` header). Writes must state their
    /// concurrency stance — a version validator or an explicit `*`.
    #[error("precondition required: {detail}")]
    PreconditionRequired { detail: String },
    #[error("unsupported sharing transition: {detail}")]
    UnsupportedTransition { detail: String },
    /// A write violated the secret type's traits, or names a type that
    /// conflicts with the one already in play (`TYPE_IMMUTABLE`,
    /// `TYPE_MISMATCH_WITH_INHERITED`, `TYPE_MISMATCH_WITH_DESCENDANT`). `reason` is the stable
    /// machine-readable code surfaced on the wire (e.g.
    /// `SHARING_NOT_ALLOWED_FOR_TYPE`); `field` names the offending request
    /// field for the canonical field violation.
    #[error("secret type violation ({reason}): {detail}")]
    TypeViolation {
        field: &'static str,
        reason: &'static str,
        detail: String,
    },
    /// A request is malformed independently of any secret type (ADR-0007):
    /// `SECRET_REQUIRED`, `EMPTY_PATCH`, `NULL_NOT_ALLOWED`,
    /// `PRECONDITION_REQUIRED`, `TYPE_REQUIRED`. `reason` is the stable
    /// machine-readable code; `field` names the offending request field.
    #[error("invalid request ({reason}): {detail}")]
    InvalidRequest {
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

/// Maps a plugin SPI failure onto the gear's taxonomy. Plugin errors never
/// reach consumers: the gear answers 503 (with the plugin's retry hint) for an
/// unavailable store, 503 without a hint for a store that refused the
/// plugin's own credentials (an operator fault, logged here), and 500 for
/// everything else.
///
/// The plugin's own text is not curated for the credstore boundary and could
/// in principle carry sensitive material, so neither the wire detail nor the
/// logs ever carry it: only its length is logged. The underlying cause stays
/// in the error chain for in-process diagnostics.
impl From<credstore_sdk::CredStorePluginError> for DomainError {
    fn from(e: credstore_sdk::CredStorePluginError) -> Self {
        use credstore_sdk::CredStorePluginError as P;
        match e {
            P::Unavailable {
                detail,
                retry_after,
                source,
            } => Self::plugin_unavailable(&detail, retry_after, source),
            P::CredentialsRejected { detail, source } => {
                Self::plugin_credentials_rejected(&detail, source)
            }
            P::Internal { detail, source } => Self::plugin_internal(&detail, source),
            // `CredStorePluginError` is `#[non_exhaustive]`: a future variant
            // is a contract the gear does not know yet.
            other => Self::Internal {
                diagnostic: "plugin returned an unknown error variant".to_owned(),
                cause: Some(Box::new(other)),
            },
        }
    }
}

impl DomainError {
    /// The public face of an unavailable value store: a generic detail (the
    /// plugin's own text never reaches the wire or the logs; only its length
    /// is logged), the optional retry hint, and the plugin's cause kept in
    /// the chain.
    fn plugin_unavailable(
        detail: &str,
        retry_after: Option<Duration>,
        cause: Option<BoxError>,
    ) -> Self {
        tracing::warn!(
            detail_len = detail.len(),
            "credstore: storage plugin reported unavailable (detail redacted)"
        );
        Self::ServiceUnavailable {
            detail: "storage backend unavailable".to_owned(),
            retry_after,
            cause,
        }
    }

    /// The store refused the plugin's own credentials: not transient for the
    /// caller (no retry hint), but the operator must act, hence the error log.
    fn plugin_credentials_rejected(detail: &str, cause: Option<BoxError>) -> Self {
        tracing::error!(
            detail_len = detail.len(),
            "credstore: the value store rejected the storage plugin's credentials \
             (detail redacted)"
        );
        Self::ServiceUnavailable {
            detail: "storage backend unavailable".to_owned(),
            retry_after: None,
            cause,
        }
    }

    /// A permanent plugin fault: surfaced at once, text redacted.
    fn plugin_internal(detail: &str, cause: Option<BoxError>) -> Self {
        tracing::error!(
            detail_len = detail.len(),
            "credstore: storage plugin reported an internal error (detail redacted)"
        );
        Self::Internal {
            diagnostic: format!(
                "plugin returned Internal error (detail redacted, {} bytes)",
                detail.len()
            ),
            cause,
        }
    }
}

/// The dependency-metric outcome of a failed plugin call.
#[must_use]
pub fn plugin_error_outcome(
    e: &credstore_sdk::CredStorePluginError,
) -> crate::domain::ports::metrics::Outcome {
    use crate::domain::ports::metrics::Outcome;
    match e {
        credstore_sdk::CredStorePluginError::CredentialsRejected { .. } => {
            Outcome::CredentialsRejected
        }
        _ => Outcome::Error,
    }
}
