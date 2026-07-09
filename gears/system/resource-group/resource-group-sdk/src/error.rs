// Created: 2026-04-16 by Constructor Tech
// @cpt-dod:cpt-cf-resource-group-dod-sdk-foundation-sdk-errors:p1
//! Resource Group SDK error surface — typed projection of
//! [`CanonicalError`].
//!
//! # Opt-in convenience, not the contract
//!
//! Per [ADR 0005][adr] the [`ResourceGroupClient`] /
//! [`ResourceGroupReadHierarchy`] trait boundaries are
//! `Result<_, CanonicalError>`. [`ResourceGroupError`] is an **opt-in**
//! typed view over that envelope, shipped for consumers that want flat
//! dispatch on the categories resource-group emits. It is *not* part of
//! the trait contract: adding a variant is non-breaking, and the single
//! authoritative AIP-193 classification lives in the impl crate's one
//! `From<DomainError> for CanonicalError` ladder (`api::rest::error`) —
//! this projection only reads the finished `CanonicalError`.
//!
//! The conversion is infallible (`From<CanonicalError>`). Canonical
//! categories resource-group does not emit fall through to
//! [`ResourceGroupError::Other`], which preserves the full
//! [`CanonicalError`] for inspection / forward-compatible dispatch on the
//! inner variant.
//!
//! **Malformed envelopes also fall through to [`ResourceGroupError::Other`].**
//! The in-process builder's type-state guarantees a well-formed envelope
//! (a resource-scoped category always carries `resource_type`; a
//! `FailedPrecondition` always carries ≥1 violation), but the documented
//! wire path (`Problem JSON → CanonicalError`) does not re-validate. So a
//! `NotFound`/`AlreadyExists` missing `resource_type`, or a
//! `FailedPrecondition` with an empty violation list, projects to `Other`
//! rather than to a typed variant with empty fields — a missing dispatch
//! key surfaces as "unmodeled" instead of silently mis-dispatching.
//!
//! # What resource-group emits — consumer dispatch reference
//!
//! | Disposition | Match arm | HTTP |
//! |---|---|---|
//! | type / group / membership missing | [`ResourceGroupError::NotFound`] | 404 |
//! | duplicate-on-create (type, membership, tenant root) | [`ResourceGroupError::AlreadyExists`] | 409 |
//! | request-shape validation (inspect `reason` against [`field`]) | [`ResourceGroupError::InvalidArgument`] | 400 |
//! | state precondition — inspect [`precondition::Subject`] | [`ResourceGroupError::FailedPrecondition`] | 400 |
//! | generic concurrency / state conflict — inspect `reason` ([`reason::aborted`]) | [`ResourceGroupError::Aborted`] | 409 |
//! | PDP denial — inspect `reason` ([`reason::permission`]) | [`ResourceGroupError::PermissionDenied`] | 403 |
//! | internal error (DB / infra) | [`ResourceGroupError::Internal`] | 500 |
//! | anything else (forward-compat) | [`ResourceGroupError::Other`] | — |
//!
//! Resource-scoped variants ([`ResourceGroupError::NotFound`] /
//! [`ResourceGroupError::AlreadyExists`]) carry the raw `resource_type`;
//! match it against [`crate::gts::GROUP_RESOURCE_TYPE`].
//!
//! [`FailedPrecondition`](ResourceGroupError::FailedPrecondition) flattens
//! several domain families that share the canonical category; the
//! discriminator is the typed [`precondition::Subject`] (e.g.
//! `Subject::Hierarchy` ⇒ a detected cycle, `Subject::Limit` ⇒ a
//! configured-limit violation), **not** the wire `type` (uniformly
//! [`precondition::STATE_TYPE`]).
//!
//! # Consumer integration — three patterns
//!
//! **Pattern 1 — pure propagation (no projection):**
//!
//! ```ignore
//! let group = rg_client.get_group(&ctx, id).await?; // ? propagates CanonicalError
//! ```
//!
//! **Pattern 2 — explicit projection at the call site:**
//!
//! ```ignore
//! use resource_group_sdk::{ResourceGroupError, precondition::Subject};
//!
//! let res = rg_client.create_group(&ctx, req).await
//!     .map_err(ResourceGroupError::from);
//! match res {
//!     Err(ResourceGroupError::FailedPrecondition { subject: Subject::Hierarchy, .. }) =>
//!         /* would create a cycle */,
//!     Err(ResourceGroupError::NotFound { .. }) => /* parent gone */,
//!     _ => /* … */,
//! }
//! ```
//!
//! **Pattern 3 — transparent chaining via `From<CanonicalError> for OwnError`:**
//!
//! ```ignore
//! impl From<CanonicalError> for OwnConsumerError {
//!     fn from(err: CanonicalError) -> Self {
//!         ResourceGroupError::from(err).into() // route through the typed view
//!     }
//! }
//! // then every call site stays plain `?`.
//! ```
//!
//! Out-of-process consumers reconstruct the canonical error from the wire
//! via `TryFrom<Problem> for CanonicalError` first, then project:
//! `Problem JSON → Problem → CanonicalError → ResourceGroupError`.
//!
//! [`ResourceGroupClient`]: crate::ResourceGroupClient
//! [`ResourceGroupReadHierarchy`]: crate::ResourceGroupReadHierarchy
//! [adr]: https://github.com/constructorfabric/gears-rust/blob/main/docs/arch/errors/ADR/0005-cpt-cf-adr-sdk-canonical-projection.md

use thiserror::Error;
use toolkit_canonical_errors::{CanonicalError, InvalidArgument};

use crate::precondition::Subject;

/// Typed projection of [`CanonicalError`] for Resource Group consumers.
///
/// The impl crate's `From<DomainError> for CanonicalError` is the single
/// authoritative AIP-193 mapping; this enum is a forward-compatible, flat
/// view over the seven categories resource-group emits, plus the
/// mandatory catch-all [`Self::Other`]. See the [gear docs](self) for
/// the dispatch table and consumer patterns.
#[derive(Error, Debug, Clone)]
pub enum ResourceGroupError {
    /// Resource not found (type, group, or membership). `resource_type`
    /// is the canonical GTS type — match it against
    /// [`crate::gts::GROUP_RESOURCE_TYPE`]; `name` is the raw identifier
    /// the caller supplied.
    #[error("not found [{resource_type}]: {name}")]
    NotFound {
        resource_type: String,
        name: String,
        detail: String,
    },

    /// Duplicate-on-create conflict (type code clash, duplicate
    /// membership, second tenant-type root).
    #[error("already exists [{resource_type}]: {name}")]
    AlreadyExists {
        resource_type: String,
        name: String,
        detail: String,
    },

    /// Request-shape validation failure. `reason` is the wire reason code
    /// (match against [`field::INVALID_PARENT_TYPE`](crate::field::INVALID_PARENT_TYPE);
    /// empty for the generic `Format` validation shape); `field` is the
    /// attributed request field, if any.
    #[error("invalid argument [{field}/{reason}]: {detail}")]
    InvalidArgument {
        field: String,
        reason: String,
        detail: String,
    },

    /// State precondition violation. `subject` is the typed
    /// [`precondition::Subject`](crate::precondition::Subject) consumers
    /// dispatch on (e.g. `Subject::Hierarchy` ⇒ cycle, `Subject::Limit` ⇒
    /// limit-violation); `type_` is the uniform wire token
    /// ([`precondition::STATE_TYPE`](crate::precondition::STATE_TYPE)).
    #[error("failed precondition [{subject}/{type_}]: {detail}")]
    FailedPrecondition {
        subject: Subject,
        type_: String,
        detail: String,
    },

    /// Generic concurrency / state conflict (HTTP 409). `reason` is the
    /// [`reason::aborted::CONFLICT`](crate::reason::aborted::CONFLICT)
    /// constant.
    #[error("aborted [{reason}]: {detail}")]
    Aborted { reason: String, detail: String },

    /// Authorization denial (HTTP 403). `reason` is the
    /// [`reason::permission::ACCESS_DENIED`](crate::reason::permission::ACCESS_DENIED)
    /// constant.
    #[error("permission denied [{reason}]: {detail}")]
    PermissionDenied { reason: String, detail: String },

    /// Unclassified internal failure (HTTP 500). `detail` is already
    /// redacted at the canonical boundary — it never carries the
    /// server-side diagnostic.
    #[error("internal error: {detail}")]
    Internal { detail: String },

    /// Catch-all for canonical categories resource-group does not model —
    /// preserves the full [`CanonicalError`] so consumers stay
    /// forward-compatible if the impl crate ever emits a new category.
    #[error("[{}] {}", canonical.gts_type(), canonical.detail())]
    Other { canonical: CanonicalError },
}

// ─────────────────────────────────────────────────────────────────────
// CanonicalError → ResourceGroupError projection.
//
// The typed sub-enum (`precondition::Subject`) lives next to its
// wire-string constants in `crate::precondition`; the single-valued
// reasons stay plain consts in `crate::field` / `crate::reason`. This
// file owns only the top-level enum and the dispatch from
// `CanonicalError`.
// ─────────────────────────────────────────────────────────────────────

impl From<CanonicalError> for ResourceGroupError {
    fn from(err: CanonicalError) -> Self {
        // Borrow the canonical detail before consuming `err`; the borrow
        // ends here so each arm below can move its fields out (no clones).
        let detail = err.detail().to_owned();
        match err {
            // A resource-scoped category MUST carry its `resource_type`
            // (the dispatch key consumers match against
            // `gts::GROUP_RESOURCE_TYPE`). Resource-group always sets it;
            // a `None` here means a malformed / foreign envelope reached us
            // via the wire path, so we let it fall through to `Other`
            // rather than forge a `NotFound { resource_type: "" }` that
            // would silently mis-dispatch.
            CanonicalError::NotFound {
                resource_type: Some(resource_type),
                resource_name,
                ..
            } => Self::NotFound {
                resource_type,
                name: resource_name.unwrap_or_default(),
                detail,
            },

            CanonicalError::AlreadyExists {
                resource_type: Some(resource_type),
                resource_name,
                ..
            } => Self::AlreadyExists {
                resource_type,
                name: resource_name.unwrap_or_default(),
                detail,
            },

            CanonicalError::InvalidArgument { ctx, .. } => project_invalid_argument(ctx, detail),

            // Resource-group emits exactly one PreconditionViolation per
            // FailedPrecondition error; the meaningful message lives in
            // the violation `description`, so surface it as `detail`. An
            // empty violation list is a malformed / foreign envelope (the
            // builder's type-state forbids it in-process) — the guard lets
            // it fall through to `Other` rather than forge a
            // `FailedPrecondition` with empty `subject`/`type_` that would
            // look like a real domain precondition.
            CanonicalError::FailedPrecondition { ctx, .. } if !ctx.violations.is_empty() => {
                ctx.violations.into_iter().next().map_or_else(
                    // Unreachable: the guard rejects empty violation lists.
                    // Kept as a total, non-panicking fallback so the
                    // conversion stays infallible.
                    || Self::Internal { detail },
                    |v| Self::FailedPrecondition {
                        subject: Subject::from_wire(&v.subject),
                        type_: v.type_,
                        detail: v.description,
                    },
                )
            }

            CanonicalError::Aborted { ctx, .. } => Self::Aborted {
                reason: ctx.reason,
                detail,
            },

            CanonicalError::PermissionDenied { ctx, .. } => Self::PermissionDenied {
                reason: ctx.reason,
                detail,
            },

            CanonicalError::Internal { .. } => Self::Internal { detail },

            other => Self::Other { canonical: other },
        }
    }
}

fn project_invalid_argument(ctx: InvalidArgument, detail: String) -> ResourceGroupError {
    // Resource-group emits two `InvalidArgument` shapes today: a single
    // `FieldViolations` (parent-type rejects) and a `Format` (generic
    // validation). `Constraint` carries a constraint identifier rather than
    // a field, so it projects with an empty `field` and the constraint name
    // in `reason`; the empty case falls back to the canonical detail.
    match ctx {
        InvalidArgument::FieldViolations { field_violations } => {
            field_violations.into_iter().next().map_or_else(
                || ResourceGroupError::InvalidArgument {
                    field: String::new(),
                    reason: String::new(),
                    detail,
                },
                |v| ResourceGroupError::InvalidArgument {
                    field: v.field,
                    reason: v.reason,
                    detail: v.description,
                },
            )
        }
        InvalidArgument::Constraint { constraint } => ResourceGroupError::InvalidArgument {
            field: String::new(),
            reason: constraint,
            detail,
        },
        InvalidArgument::Format { .. } => ResourceGroupError::InvalidArgument {
            field: String::new(),
            reason: String::new(),
            detail,
        },
    }
}

#[cfg(test)]
#[path = "error_tests.rs"]
mod error_tests;
