//! A subject's profile in graph storage: one root node per subject, one fact
//! node per value, and a has-fact edge from the root to each fact.
//!
//! The root carries the profile version, a counter that changes on every
//! write. Construct is the root's only writer, so the counter always equals
//! graph storage's stored node version and is the compare-and-set of the whole
//! profile. Removed facts are listed on the root in the same write, so the
//! removal is committed with the rest of the plan; their soft delete runs
//! after the write, and readers skip listed facts until it has.

use std::future::Future;
use std::time::Duration;

use toolkit_canonical_errors::CanonicalError;

use crate::domain::error::{DomainError, is_retryable};

mod context;
pub mod fact;
pub mod reader;
pub mod root;
pub mod types;
pub mod writer;

pub(crate) use context::{ContextSubject, subject_in_context};

#[cfg(test)]
mod fact_test;
#[cfg(test)]
mod graph_test;
#[cfg(test)]
mod root_test;

/// The reason graph storage gives a refused compare-and-set, and a
/// registration that lost to another of the same type.
pub(crate) const CAS_CONFLICT: &str = "CAS_CONFLICT";

/// How long one graph storage call may take before it counts as unanswered.
/// Graph storage checks its own budget only when it admits a call, and cannot
/// cut off a statement once it is sent, so the bound has to be the caller's.
pub const DEFAULT_CALL_TIMEOUT: Duration = Duration::from_secs(10);

/// A failed graph storage call as a domain error: unavailable when a retry may
/// cure it, internal otherwise. A refusal of Construct's own permission is
/// internal too: it is the deployment's grant that is missing, not the
/// caller's right, so it is no `Forbidden`, which means the caller was denied.
/// `what` names the call.
#[allow(unknown_lints, reason = "dylint lint names are unknown to rustc")]
#[allow(
    de1302_error_from_to_string,
    reason = "DomainError carries a String until its variants hold a boxed source"
)]
pub(crate) fn graph_error(what: &str, err: &CanonicalError) -> DomainError {
    match err {
        CanonicalError::PermissionDenied { .. } | CanonicalError::Unauthenticated { .. } => {
            DomainError::internal(format!(
                "graph storage refused Construct's own permission for {what}: {err}"
            ))
        }
        _ if is_retryable(err) => DomainError::Unavailable(format!("graph storage {what}: {err}")),
        _ => DomainError::internal(format!("graph storage {what}: {err}")),
    }
}

/// Await one graph storage call for at most `timeout`.
///
/// # Errors
///
/// [`DomainError::Unavailable`] when the call does not answer in time; a
/// failed call as [`graph_error`] maps it.
pub(crate) async fn bounded<T>(
    timeout: Duration,
    what: &str,
    call: impl Future<Output = Result<T, CanonicalError>>,
) -> Result<T, DomainError> {
    tokio::time::timeout(timeout, call)
        .await
        .map_err(|_| unanswered(what, timeout))?
        .map_err(|err| graph_error(what, &err))
}

/// A graph storage call that did not answer within `timeout`.
pub(crate) fn unanswered(what: &str, timeout: Duration) -> DomainError {
    DomainError::Unavailable(format!(
        "graph storage {what}: no answer within {timeout:?}"
    ))
}
