use toolkit_canonical_errors::CanonicalError;
use toolkit_db::DbError;
use toolkit_macros::domain_model;

use super::pagination::{PageError, QueryError};
use super::profile::FullProfile;
use super::refusal::Refusal;
use super::review_queue::{NotStored, ReviewConflict};
use super::settings_scope::SettingsOutOfScope;

#[domain_model]
#[derive(Debug, thiserror::Error)]
pub enum DomainError {
    #[error("Record refused: {} at '{}': {}", .0.reason.code(), .0.place, .0.rule)]
    Refused(Refusal),

    #[error("Access forbidden: {0}")]
    Forbidden(String),

    /// The caller's policy scope leaves the subject's settings out, on a read
    /// or a write. Answered like `Forbidden`; kept apart so callers and the
    /// log can tell it from a policy denial.
    #[error("Access forbidden: {0}")]
    OutOfScope(SettingsOutOfScope),

    /// The caller may not see the resource. Answered like a resource that
    /// does not exist, so the answer tells nothing about what exists.
    #[error("Not visible: {0}")]
    NotVisible(String),

    #[error("Service unavailable: {0}")]
    Unavailable(String),

    /// A write was sent and its result is unknown: it may have been stored.
    /// Answered as unavailable, with a detail that says so.
    #[error("Write result unknown: {0}")]
    WriteUnknown(String),

    /// The deployment does not configure what the call needs, such as the
    /// processing identity. Not cured by a retry, so it is not `Unavailable`.
    #[error("Not configured: {0}")]
    NotConfigured(String),

    /// The subject has no live fact with the id asked for. Answered like a
    /// subject that does not exist.
    #[error("No such fact: {0}")]
    NoSuchFact(String),

    /// The tenant has no review request with the id asked for.
    #[error("No such review request: {0}")]
    NoSuchReviewRequest(String),

    /// The fact already has an open review request, or the request asked to
    /// resolve is closed.
    #[error("Review conflict: {0}")]
    ReviewConflict(ReviewConflict),

    /// A reviewer's correction is for another property than the fact's
    /// (`property`), or its property's schema does not take the value
    /// (`value`). The rule never repeats the value.
    #[error("Invalid {field}: {rule}")]
    InvalidValue { field: &'static str, rule: String },

    /// An administrator's add of a value the subject holds already, or of a
    /// second value of a one-valued property: the fact that holds it.
    #[error("Fact held: {}", .0.0)]
    FactHeld(super::plan::NodeKey),

    /// A correction the checks or Admission did not let through; nothing was
    /// stored.
    #[error("Not stored: {}", .0.name())]
    NotStored(NotStored),

    /// The caller's `$filter`, `$orderby` or cursor does not fit the list.
    #[error("Invalid query: {0:?}")]
    InvalidQuery(QueryError),

    #[error("Internal error: {0}")]
    Internal(String),

    /// The subject's profile holds as many facts as one read takes: it takes
    /// no new fact, and a profile past that cannot be read whole. Not a
    /// defect, and not cured by a retry.
    #[error("Profile full: {0}")]
    ProfileFull(FullProfile),

    #[error("Database error: {0}")]
    Database(#[from] DbError),
}

impl DomainError {
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
            // @cpt-begin:cpt-cf-construct-algo-gear-foundation-map-errors:p1:inst-map-enforcer-denied
            // @cpt-begin:cpt-cf-construct-algo-gear-foundation-map-errors:p1:inst-map-enforcer-denied-return
            authz_resolver_sdk::EnforcerError::AccessDenied(_)
            | authz_resolver_sdk::EnforcerError::CompileFailed(_) => Self::Forbidden(e.to_string()),
            // @cpt-end:cpt-cf-construct-algo-gear-foundation-map-errors:p1:inst-map-enforcer-denied-return
            // @cpt-end:cpt-cf-construct-algo-gear-foundation-map-errors:p1:inst-map-enforcer-denied
            // @cpt-begin:cpt-cf-construct-algo-gear-foundation-map-errors:p1:inst-map-enforcer-failed
            // @cpt-begin:cpt-cf-construct-algo-gear-foundation-map-errors:p1:inst-map-enforcer-failed-return
            // A failure that the caller may retry is unavailable; any other
            // failure of the policy service is an internal error.
            authz_resolver_sdk::EnforcerError::EvaluationFailed(inner) if is_retryable(&inner) => {
                Self::Unavailable(describe_failure(&inner))
            }
            authz_resolver_sdk::EnforcerError::EvaluationFailed(inner) => {
                Self::Internal(describe_failure(&inner))
            }
            // An unknown outcome class is an internal failure, never a denial.
            unknown => {
                tracing::error!(error = %unknown, "authz failed with an unknown outcome");
                Self::Internal("authorization failed with an unknown outcome".to_owned())
            }
        }
        // @cpt-end:cpt-cf-construct-algo-gear-foundation-map-errors:p1:inst-map-enforcer-failed-return
        // @cpt-end:cpt-cf-construct-algo-gear-foundation-map-errors:p1:inst-map-enforcer-failed
    }
}

impl From<Refusal> for DomainError {
    fn from(refusal: Refusal) -> Self {
        Self::Refused(refusal)
    }
}

impl From<PageError> for DomainError {
    fn from(err: PageError) -> Self {
        match err {
            PageError::Internal(detail) => Self::Internal(detail),
            PageError::Query(query) => Self::InvalidQuery(query),
        }
    }
}
