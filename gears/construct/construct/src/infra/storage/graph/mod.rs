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
use toolkit_security::SecurityContext;

use crate::domain::error::{DomainError, is_retryable};
use crate::domain::subject_settings::SubjectKey;

pub mod fact;
pub mod reader;
pub mod root;
pub mod types;
pub mod writer;

#[cfg(test)]
mod fact_test;
#[cfg(test)]
mod graph_test;
#[cfg(test)]
mod root_test;

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

/// Await one graph storage call for at most `timeout`. A call with no answer
/// in time is unavailable.
///
/// # Errors
///
/// [`DomainError::Unavailable`] when the call does not answer in time; the
/// call's own result is returned as it is.
pub(crate) async fn bounded<T>(
    timeout: Duration,
    what: &str,
    call: impl Future<Output = Result<T, CanonicalError>>,
) -> Result<Result<T, CanonicalError>, DomainError> {
    tokio::time::timeout(timeout, call).await.map_err(|_| {
        DomainError::Unavailable(format!(
            "graph storage {what}: no answer within {timeout:?}"
        ))
    })
}

/// `subject` when it is in the context's tenant. Graph storage reads and
/// writes in the security context's tenant, and the profile's keys name a
/// tenant too, so the two must be the same.
///
/// # Errors
///
/// [`DomainError::Internal`] when the subject's tenant is not the context's:
/// the caller built the context for another tenant, which is a defect.
pub(crate) fn subject_in_context(
    ctx: &SecurityContext,
    subject: SubjectKey,
) -> Result<SubjectKey, DomainError> {
    let context_tenant = ctx.subject_tenant_id();
    if context_tenant != subject.tenant_id {
        return Err(DomainError::internal(format!(
            "the profile's tenant {} is not the context's tenant {context_tenant}",
            subject.tenant_id
        )));
    }
    Ok(subject)
}
