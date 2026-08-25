// Created: 2026-08-12 by Constructor Tech
//! The gear's internal error type.
//!
//! # Where this sits
//!
//! `DomainError` is in-process only. It never crosses a trait boundary and is
//! never serialized: the impl side converts it once, with
//! `.map_err(CanonicalError::from)`, and the platform renders the resulting
//! `CanonicalError` as an RFC-9457 problem document. Keeping it internal is what
//! lets a variant be added here without it becoming a breaking change for any
//! consuming gear.
//!
//! # Why these variants and not the DESIGN catalogue
//!
//! DESIGN.md §4.3 names concrete failures — `CategoryNotEmpty`,
//! `DeclarationKeyConflict`, `ValueTooLarge`. Those belong to the features that
//! raise them. What lives here is the set of **shapes** every feature reuses:
//! the 422 with field-level detail, the 412 that guards a conditional write, the
//! 409 for a state conflict, and the denial that must not disclose existence.
//!
//! A feature adds its own variant, or supplies its own `code` to
//! [`DomainError::Validation`]; it does not restate the mapping.

use crate::field;

/// A failure raised inside the gear.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum DomainError {
    /// A request field failed validation. Renders as `422` with a field-level
    /// entry carrying `field`, `code`, and `message`.
    #[error("validation failed on `{field}`: {message}")]
    Validation {
        /// The offending field, or [`field::REQUEST_FIELD`] when it cannot be
        /// pinned to one.
        field: String,
        /// A stable code from [`crate::field`]; tooling matches on this, never
        /// on `message`.
        code: &'static str,
        /// Human-readable explanation.
        message: String,
    },

    /// A mutating request arrived with no `If-Match` at all. Renders as `428`.
    ///
    /// Distinct from [`DomainError::PreconditionFailed`] on purpose: a stale tag
    /// means re-read and retry, an absent one means the client must change.
    /// Telling a broken client to retry would have it retry forever.
    #[error("precondition required: {detail}")]
    PreconditionRequired {
        /// What the request must carry.
        detail: String,
    },

    /// A conditional write lost its `If-Match` check. Renders as `412`.
    #[error("precondition failed: {detail}")]
    PreconditionFailed {
        /// What no longer holds.
        detail: String,
    },

    /// The request conflicts with current state. Renders as `409`.
    #[error("conflict: {detail}")]
    Conflict {
        /// What conflicts.
        detail: String,
    },

    /// The caller is not entitled to the target. Renders as `403`.
    ///
    /// Deliberately carries **no** identifier of the target and no indication of
    /// whether it exists: a denial that differed between "no such setting" and
    /// "exists but forbidden" would let an unauthorized caller enumerate the
    /// settings tree by reading status codes.
    #[error("not authorized")]
    Unauthorized {
        /// The kind of resource the decision was made against — never the
        /// caller's identifier for it. Two denials for different categories are
        /// still byte-identical; only the resource *type* differs, and the URL
        /// already reveals that.
        resource: &'static str,
    },

    /// No such resource. Renders as `404`.
    #[error("{resource} not found")]
    NotFound {
        /// The kind of thing that was not found — never the caller's identifier
        /// for it, which may itself be sensitive.
        resource: &'static str,
    },

    /// A dependency this service needs is unreachable. Renders as `503`.
    #[error("settings unavailable: {detail}")]
    Unavailable {
        /// What could not be reached.
        detail: String,
    },

    /// Anything unrecognized. Renders as `500` with a generic body.
    ///
    /// `diagnostic` stays in process: the platform keeps it out of the wire
    /// representation, so a stack detail or a connection string put here does
    /// not reach the caller.
    #[error("internal error: {diagnostic}")]
    Internal {
        /// In-process diagnostic. Never serialized.
        diagnostic: String,
    },
}

impl DomainError {
    /// A validation failure that could not be pinned to a single field.
    #[must_use]
    pub fn validation(message: impl Into<String>) -> Self {
        Self::Validation {
            field: field::REQUEST_FIELD.to_owned(),
            code: field::VALIDATION,
            message: message.into(),
        }
    }
}
