use toolkit_canonical_errors::CanonicalError;
use toolkit_db::DbError;
use toolkit_macros::domain_model;

#[domain_model]
#[derive(Debug, thiserror::Error)]
pub enum DomainError {
    #[error("Note not found")]
    NotFound,

    #[error("Validation error on field '{field}': {message}")]
    Validation { field: String, message: String },

    #[error("Access forbidden: {0}")]
    Forbidden(String),

    #[error("Service unavailable: {0}")]
    Unavailable(String),

    #[error("Internal error: {0}")]
    Internal(String),

    #[error("Database error: {0}")]
    Database(#[from] DbError),
}

impl DomainError {
    pub fn validation(field: impl Into<String>, message: impl Into<String>) -> Self {
        Self::Validation {
            field: field.into(),
            message: message.into(),
        }
    }

    pub fn forbidden(message: impl Into<String>) -> Self {
        Self::Forbidden(message.into())
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self::Internal(message.into())
    }
}

/// Whether retrying the failed call can help: the error answers with one of the
/// platform's retryable HTTP statuses, 429, 503 or 504 (`is_retryable_status` in
/// `toolkit-contract`). Any other status is not retryable.
pub(crate) fn is_retryable(inner: &CanonicalError) -> bool {
    matches!(inner.status_code(), 429 | 503 | 504)
}

/// The cause of a failed evaluation, for the log and the internal message. The
/// diagnostic of an internal error holds its real cause; it never reaches a caller.
fn describe_failure(inner: &CanonicalError) -> String {
    match inner.diagnostic() {
        Some(diagnostic) => format!("authorization evaluation failed: {inner}: {diagnostic}"),
        None => format!("authorization evaluation failed: {inner}"),
    }
}

/// Converts an enforcer failure into a domain error without logging it: every
/// domain error is logged once, where it is mapped to a response (`api/rest/error.rs`).
///
/// @cpt-dod:cpt-cf-construct-dod-gear-foundation-error-mapping:p1
// TODO(DE1302): `Forbidden`, `Unavailable` and `Internal` only carry Strings, so the
// `EnforcerError` source is lost. Extend the variants to hold a boxed source,
// then remove this allow.
#[allow(unknown_lints, reason = "dylint lint names are unknown to rustc")]
#[allow(
    de1302_error_from_to_string,
    reason = "the source error is flattened to a String until the variants carry a boxed source"
)]
impl From<authz_resolver_sdk::EnforcerError> for DomainError {
    fn from(e: authz_resolver_sdk::EnforcerError) -> Self {
        match e {
            // @cpt-begin:cpt-cf-construct-algo-gear-foundation-scope-note-access:p1:inst-scope-denied
            // @cpt-begin:cpt-cf-construct-algo-gear-foundation-scope-note-access:p1:inst-scope-denied-return
            authz_resolver_sdk::EnforcerError::Denied { .. }
            | authz_resolver_sdk::EnforcerError::CompileFailed(_) => Self::Forbidden(e.to_string()),
            // @cpt-end:cpt-cf-construct-algo-gear-foundation-scope-note-access:p1:inst-scope-denied-return
            // @cpt-end:cpt-cf-construct-algo-gear-foundation-scope-note-access:p1:inst-scope-denied
            // @cpt-begin:cpt-cf-construct-algo-gear-foundation-scope-note-access:p1:inst-scope-failed
            // @cpt-begin:cpt-cf-construct-algo-gear-foundation-scope-note-access:p1:inst-scope-failed-return
            // A failure that the caller may retry is unavailable; any other
            // failure of the policy service is an internal error.
            authz_resolver_sdk::EnforcerError::EvaluationFailed(inner) if is_retryable(&inner) => {
                Self::Unavailable(describe_failure(&inner))
            }
            authz_resolver_sdk::EnforcerError::EvaluationFailed(inner) => {
                Self::Internal(describe_failure(&inner))
            }
        }
        // @cpt-end:cpt-cf-construct-algo-gear-foundation-scope-note-access:p1:inst-scope-failed-return
        // @cpt-end:cpt-cf-construct-algo-gear-foundation-scope-note-access:p1:inst-scope-failed
    }
}
