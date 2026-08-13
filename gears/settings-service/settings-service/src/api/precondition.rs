// Created: 2026-08-12 by Constructor Tech
//! `If-Match` evaluation for conditional writes.
//!
//! Every mutating request against a versioned resource passes through here
//! before its handler runs. Two settings administrators editing the same
//! declaration is the ordinary case, not the exceptional one, and without a
//! precondition the second write silently discards the first.
//!
//! # Why absent and stale are different answers
//!
//! A **stale** `If-Match` means the caller read, someone else wrote, and the
//! caller's edit is now based on something that no longer exists — it should
//! re-read and decide. An **absent** `If-Match` means the caller never opted
//! into the check at all; retrying is pointless, the client itself must change.
//! Collapsing the two would tell a broken client to retry forever.

use crate::domain::error::DomainError;

/// An entity tag over a resource's persisted representation.
///
/// Compared verbatim as an opaque token: this type never parses or orders tags,
/// so how one is derived can change without any caller noticing.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ETag(String);

impl ETag {
    /// Wrap a computed tag.
    #[must_use]
    pub fn new(tag: impl Into<String>) -> Self {
        Self(tag.into())
    }

    /// The tag as it travels in `ETag` and `If-Match` headers.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for ETag {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// The verdict of an `If-Match` evaluation.
///
/// Carries the current tag forward on success so the handler can echo a
/// refreshed `ETag` without recomputing it — and, more importantly, so the
/// value the check passed against is the same one the response reports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Proceed {
    /// The current tag, verified to match what the caller supplied.
    pub current: ETag,
}

/// Evaluate a conditional write.
///
/// `supplied` is the raw `If-Match` header value, absent when the client sent
/// none. `current` is the tag computed from the target's persisted state.
///
/// # Errors
///
/// [`DomainError::PreconditionRequired`] when no `If-Match` was supplied — the
/// client must change, so this is a request fault rather than a lost race.
/// [`DomainError::PreconditionFailed`] when the supplied tag is stale.
pub fn evaluate(supplied: Option<&str>, current: &ETag) -> Result<Proceed, DomainError> {
    // @cpt-begin:cpt-cf-settings-service-algo-gear-foundation-precondition:p1:inst-gf-precond-1
    let Some(supplied) = supplied else {
        return Err(DomainError::PreconditionRequired {
            detail: "a conditional write requires an If-Match header".to_owned(),
        });
    };
    // @cpt-end:cpt-cf-settings-service-algo-gear-foundation-precondition:p1:inst-gf-precond-1

    // @cpt-begin:cpt-cf-settings-service-algo-gear-foundation-precondition:p1:inst-gf-precond-3
    // Compared verbatim. A tag is opaque, so trimming or unquoting here would
    // make two spellings of one tag and admit a write the caller never based on
    // the state it thinks it read.
    if supplied != current.as_str() {
        return Err(DomainError::PreconditionFailed {
            detail: "the resource changed since it was read".to_owned(),
        });
    }
    // @cpt-end:cpt-cf-settings-service-algo-gear-foundation-precondition:p1:inst-gf-precond-3

    // @cpt-begin:cpt-cf-settings-service-algo-gear-foundation-precondition:p1:inst-gf-precond-4
    Ok(Proceed {
        current: current.clone(),
    })
    // @cpt-end:cpt-cf-settings-service-algo-gear-foundation-precondition:p1:inst-gf-precond-4
}

/// The header this check reads, named once so the violation and the docs agree.
pub const IF_MATCH_HEADER: &str = "If-Match";

#[cfg(test)]
#[path = "precondition_tests.rs"]
mod precondition_tests;
