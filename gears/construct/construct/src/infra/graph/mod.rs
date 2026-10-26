//! A subject's profile in graph storage: one root node per subject, one fact
//! node per value, and a has-fact edge from the root to each fact.
//!
//! The root carries the profile version, a counter that changes on every
//! write. Construct is the root's only writer, so the counter always equals
//! graph storage's stored node version and is the compare-and-set of the whole
//! profile. Removed facts are listed on the root in the same write, so the
//! removal is committed with the rest of the plan; their soft delete runs
//! after the write, and readers skip listed facts until it has.

use toolkit_canonical_errors::CanonicalError;

use crate::domain::error::{DomainError, is_retryable};

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

/// A failed graph storage call as a domain error: unavailable when a retry may
/// cure it, internal otherwise. `what` names the call for the log line where
/// the error is mapped.
#[allow(unknown_lints, reason = "dylint lint names are unknown to rustc")]
#[allow(
    de1302_error_from_to_string,
    reason = "DomainError carries a String until its variants hold a boxed source"
)]
pub(crate) fn graph_error(what: &str, err: &CanonicalError) -> DomainError {
    if is_retryable(err) {
        DomainError::Unavailable(format!("graph storage {what}: {err}"))
    } else {
        DomainError::internal(format!("graph storage {what}: {err}"))
    }
}
