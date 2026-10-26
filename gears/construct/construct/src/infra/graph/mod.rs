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

/// A failed graph storage call as a domain error: forbidden when graph storage
/// refuses Construct's permission, unavailable when a retry may cure it,
/// internal otherwise. `what` names the call.
#[allow(unknown_lints, reason = "dylint lint names are unknown to rustc")]
#[allow(
    de1302_error_from_to_string,
    reason = "DomainError carries a String until its variants hold a boxed source"
)]
pub(crate) fn graph_error(what: &str, err: &CanonicalError) -> DomainError {
    match err {
        CanonicalError::PermissionDenied { .. } | CanonicalError::Unauthenticated { .. } => {
            DomainError::forbidden(format!("graph storage {what}: {err}"))
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

/// The profile of `subject_id` in the context's tenant. Graph storage reads
/// and writes in the security context's tenant, and the profile's keys name a
/// tenant too, so the two must be the same.
///
/// # Errors
///
/// [`DomainError::Internal`] when `tenant_id` is not the context's tenant: the
/// caller built the context for another tenant, which is a defect.
pub(crate) fn subject_in_context(
    ctx: &SecurityContext,
    tenant_id: uuid::Uuid,
    subject_id: uuid::Uuid,
) -> Result<SubjectKey, DomainError> {
    let context_tenant = ctx.subject_tenant_id();
    if context_tenant != tenant_id {
        return Err(DomainError::internal(format!(
            "the profile's tenant {tenant_id} is not the context's tenant {context_tenant}"
        )));
    }
    Ok(SubjectKey {
        tenant_id,
        subject_id,
    })
}
