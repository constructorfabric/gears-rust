//! Refusals the inbox itself produces. A source's door error is returned unchanged.
//!
//! The problem marker names the error envelope. The inbox has no authorization resource of its own.

use toolkit_canonical_errors::{CanonicalError, resource_error};

#[resource_error(toolkit_canonical_errors::gts_id!("cf.bss.approvals.inbox.v1~"))]
struct InboxProblem;

/// The caller cannot read any configured source. The body names no gear.
#[must_use]
pub fn forbidden() -> CanonicalError {
    InboxProblem::permission_denied()
        .with_reason("FORBIDDEN")
        .create()
}

/// No configured source holds the unit.
#[must_use]
pub fn not_found() -> CanonicalError {
    InboxProblem::not_found("approval unit not found")
        .with_resource("approval-unit")
        .create()
}

/// A configured source did not answer. `sources` are the names that were down or unregistered.
#[must_use]
pub fn source_unavailable(sources: &[String]) -> CanonicalError {
    CanonicalError::service_unavailable()
        .with_detail(format!("SOURCE_UNAVAILABLE: {}", sources.join(", ")))
        .create()
}

/// Two sources both returned the unit. The detail names every one of them.
///
/// `data_loss` is the 500 category whose detail reaches the wire. `internal` keeps its description
/// off the body, so it cannot name the sources.
#[must_use]
pub fn ambiguous(sources: &[String]) -> CanonicalError {
    InboxProblem::data_loss(format!(
        "{} all hold this approval unit",
        sources.join(" and ")
    ))
    .with_resource("approval-unit")
    .create()
}

/// The caller sent no authenticated subject.
#[must_use]
pub fn unauthenticated() -> CanonicalError {
    CanonicalError::unauthenticated()
        .with_reason("AUTHENTICATION_REQUIRED")
        .create()
}
