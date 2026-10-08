//! Account Management SDK error surface — typed projection of
//! [`CanonicalError`].
//!
//! # Opt-in convenience, not the contract
//!
//! Per [ADR 0005][adr] the [`AccountManagementClient`] trait boundary is
//! `Result<_, CanonicalError>`. [`AccountManagementError`] is an
//! **opt-in** typed view over that envelope, shipped for consumers that
//! want flat dispatch on the categories AM emits. It is *not* part of
//! the trait contract: adding a variant is non-breaking, and the single
//! authoritative AIP-193 classification lives in the impl crate's one
//! `From<DomainError> for CanonicalError` ladder — this projection only
//! reads the finished `CanonicalError`.
//!
//! The conversion is infallible (`From<CanonicalError>`). Canonical
//! categories AM does not emit fall through to [`AccountManagementError::Other`],
//! which preserves the full [`CanonicalError`] for inspection /
//! forward-compatible dispatch on the inner variant.
//!
//! # What AM emits — consumer dispatch reference
//!
//! | Disposition | Match arm | HTTP |
//! |---|---|---|
//! | resource missing (tenant / user / conversion / metadata) | [`AccountManagementError::NotFound`] | 404 |
//! | duplicate-on-create | [`AccountManagementError::AlreadyExists`] | 409 |
//! | request-shape validation — inspect [`field::ValidationReason`] | [`AccountManagementError::InvalidArgument`] | 400 |
//! | state precondition — inspect [`precondition::Subject`] | [`AccountManagementError::FailedPrecondition`] | 400 |
//! | concurrency / version conflict — inspect `reason` ([`reason::aborted`]) | [`AccountManagementError::Aborted`] | 409 |
//! | cross-tenant denied — inspect `reason` ([`reason::permission`]) | [`AccountManagementError::PermissionDenied`] | 403 |
//! | `IdP` operation unsupported | [`AccountManagementError::Unimplemented`] | 501 |
//! | transient outage (infra / `IdP` transport), retry hint | [`AccountManagementError::Unavailable`] | 503 |
//! | integrity-check single-flight gate held, or the tenant service-account limit reached — inspect `subject` ([`quota`]) | [`AccountManagementError::ResourceExhausted`] | 429 |
//! | internal error | [`AccountManagementError::Internal`] | 500 |
//! | anything else (forward-compat) | [`AccountManagementError::Other`] | — |
//!
//! Resource-scoped variants ([`AccountManagementError::NotFound`] /
//! [`AccountManagementError::AlreadyExists`]) carry the raw
//! `resource_type`; match it against the [`gts`] constants
//! (`gts.cf.core.am.{tenant|user|tenant_metadata|conversion_request}.v1~`).
//!
//! # Consumer integration — three patterns
//!
//! **Pattern 1 — pure propagation (no projection):**
//!
//! ```ignore
//! let tenant = am_client.get_tenant(&ctx, id).await?; // ? propagates CanonicalError
//! ```
//!
//! **Pattern 2 — explicit projection at the call site:**
//!
//! ```ignore
//! use account_management_sdk::{AccountManagementError, precondition::Subject};
//!
//! let res = am_client.delete_tenant(&ctx, id).await
//!     .map_err(AccountManagementError::from);
//! match res {
//!     Err(AccountManagementError::FailedPrecondition { subject: Subject::Tenant, .. }) =>
//!         /* has children / owns resources — surface a conflict to the user */,
//!     Err(AccountManagementError::NotFound { .. }) => /* already gone */,
//!     _ => /* … */,
//! }
//! ```
//!
//! **Pattern 3 — transparent chaining via `From<CanonicalError> for OwnError`:**
//!
//! ```ignore
//! impl From<CanonicalError> for OwnConsumerError {
//!     fn from(err: CanonicalError) -> Self {
//!         AccountManagementError::from(err).into() // route through the typed view
//!     }
//! }
//! // then every call site stays plain `?`.
//! ```
//!
//! Out-of-process consumers reconstruct the canonical error from the
//! wire via `TryFrom<Problem> for CanonicalError` first, then project:
//! `Problem JSON → Problem → CanonicalError → AccountManagementError`.
//!
//! [`AccountManagementClient`]: crate::AccountManagementClient
//! [adr]: https://github.com/constructorfabric/gears-rust/blob/main/docs/arch/errors/ADR/0005-cpt-cf-adr-sdk-canonical-projection.md

use thiserror::Error;
use toolkit_canonical_errors::{CanonicalError, InvalidArgument};

use crate::field::ValidationReason;
use crate::precondition::Subject;

/// Typed projection of [`CanonicalError`] for Account Management
/// consumers.
///
/// The impl crate's `From<DomainError> for CanonicalError` is the single
/// authoritative AIP-193 mapping; this enum is a forward-compatible,
/// flat view over the ten categories AM emits, plus the mandatory
/// catch-all [`Self::Other`]. See the [gear docs](self) for the
/// dispatch table and consumer patterns.
#[derive(Debug, Clone, Error)]
#[non_exhaustive]
pub enum AccountManagementError {
    /// Resource not found (tenant, user, conversion request, or metadata
    /// entry). `resource_type` is the canonical GTS type — match it
    /// against the [`crate::gts`] constants; `name` is the raw
    /// identifier the caller supplied.
    #[error("not found [{resource_type}]: {name}")]
    NotFound {
        resource_type: String,
        name: String,
        detail: String,
    },

    /// Duplicate-on-create conflict (tenant slug clash, pending
    /// conversion already exists).
    #[error("already exists [{resource_type}]: {name}")]
    AlreadyExists {
        resource_type: String,
        name: String,
        detail: String,
    },

    /// Request-shape validation failure. `reason` is the typed
    /// [`field::ValidationReason`](crate::field::ValidationReason);
    /// `field` is the attributed request field.
    #[error("invalid argument [{field}/{reason}]: {detail}")]
    InvalidArgument {
        field: String,
        reason: ValidationReason,
        detail: String,
    },

    /// State precondition violation. `subject` is the typed
    /// [`precondition::Subject`](crate::precondition::Subject) consumers
    /// dispatch on (e.g. `Subject::Tenant` ⇒ has children / owns
    /// resources); `type_` is the finer wire token.
    #[error("failed precondition [{subject}/{type_}]: {detail}")]
    FailedPrecondition {
        subject: Subject,
        type_: String,
        detail: String,
    },

    /// Concurrency / version conflict (HTTP 409). `reason` is one of the
    /// [`reason::aborted`](crate::reason::aborted) constants
    /// (`METADATA_VERSION_MISMATCH`, `SERIALIZATION_CONFLICT`).
    #[error("aborted [{reason}]: {detail}")]
    Aborted { reason: String, detail: String },

    /// Authorization denial (HTTP 403). `reason` is one of the
    /// [`reason::permission`](crate::reason::permission) constants
    /// (`CROSS_TENANT_DENIED`).
    #[error("permission denied [{reason}]: {detail}")]
    PermissionDenied { reason: String, detail: String },

    /// The configured `IdP` plugin declared the operation unsupported in
    /// its current profile (HTTP 501).
    #[error("unimplemented: {detail}")]
    Unimplemented { detail: String },

    /// Transient infrastructure / `IdP` transport outage (HTTP 503).
    /// `retry_after_seconds` carries the backoff hint when one is
    /// available.
    #[error("service unavailable: {detail}")]
    Unavailable {
        retry_after_seconds: Option<u64>,
        detail: String,
    },

    /// A bounded resource is exhausted (HTTP 429): the hierarchy-integrity
    /// single-flight gate, or the tenant service-account limit. `subject`
    /// is one of the [`quota`](crate::quota) constants
    /// (`integrity_check`, `service_accounts`).
    #[error("resource exhausted [{subject}]: {detail}")]
    ResourceExhausted { subject: String, detail: String },

    /// Unclassified internal failure (HTTP 500). `detail` is already
    /// redacted at the canonical boundary — it never carries the
    /// server-side diagnostic.
    #[error("internal error: {detail}")]
    Internal { detail: String },

    /// Catch-all for canonical categories AM does not model — preserves
    /// the full [`CanonicalError`] so consumers stay forward-compatible
    /// if the impl crate ever emits a new category. Reaching `Other`
    /// indicates a category outside AM's documented emission set.
    #[error("[{}] {}", canonical.gts_type(), canonical.detail())]
    Other { canonical: CanonicalError },
}

// ─────────────────────────────────────────────────────────────────────
// CanonicalError → AccountManagementError projection.
//
// The typed sub-enums (`field::ValidationReason`, `precondition::Subject`)
// live next to their wire-string constants in `crate::field` /
// `crate::precondition`; the single-valued reasons stay plain consts in
// `crate::reason` / `crate::quota`. This file owns only the top-level
// enum and the dispatch from `CanonicalError`.
// ─────────────────────────────────────────────────────────────────────

impl From<CanonicalError> for AccountManagementError {
    fn from(err: CanonicalError) -> Self {
        // Borrow the canonical detail before consuming `err`; the borrow
        // ends here so each arm below can move its fields out (no clones).
        let detail = err.detail().to_owned();
        match err {
            CanonicalError::NotFound {
                resource_type,
                resource_name,
                ..
            } => Self::NotFound {
                resource_type: resource_type.unwrap_or_default(),
                name: resource_name.unwrap_or_default(),
                detail,
            },

            CanonicalError::AlreadyExists {
                resource_type,
                resource_name,
                ..
            } => Self::AlreadyExists {
                resource_type: resource_type.unwrap_or_default(),
                name: resource_name.unwrap_or_default(),
                detail,
            },

            CanonicalError::InvalidArgument { ctx, .. } => project_invalid_argument(ctx, detail),

            // AM emits exactly one PreconditionViolation per
            // FailedPrecondition error; the meaningful message lives in
            // the violation `description`, so surface it as `detail`.
            CanonicalError::FailedPrecondition { ctx, .. } => {
                ctx.violations.into_iter().next().map_or_else(
                    || Self::FailedPrecondition {
                        subject: Subject::Unknown(String::new()),
                        type_: String::new(),
                        detail,
                    },
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

            CanonicalError::Unimplemented { .. } => Self::Unimplemented { detail },

            CanonicalError::ServiceUnavailable { ctx, .. } => Self::Unavailable {
                retry_after_seconds: ctx.retry_after_seconds,
                detail,
            },

            CanonicalError::ResourceExhausted { ctx, .. } => Self::ResourceExhausted {
                subject: ctx
                    .violations
                    .into_iter()
                    .next()
                    .map(|v| v.subject)
                    .unwrap_or_default(),
                detail,
            },

            CanonicalError::Internal { .. } => Self::Internal { detail },

            other => Self::Other { canonical: other },
        }
    }
}

fn project_invalid_argument(ctx: InvalidArgument, detail: String) -> AccountManagementError {
    // AM only ever emits the `FieldViolations` shape with a single
    // violation; the `Format` / `Constraint` shapes and the empty case
    // fall back to the canonical detail with an unknown reason.
    let first_violation = match ctx {
        InvalidArgument::FieldViolations { field_violations } => {
            field_violations.into_iter().next()
        }
        InvalidArgument::Format { .. } | InvalidArgument::Constraint { .. } => None,
    };

    first_violation.map_or(
        AccountManagementError::InvalidArgument {
            field: String::new(),
            reason: ValidationReason::Unknown(String::new()),
            detail,
        },
        |v| AccountManagementError::InvalidArgument {
            field: v.field,
            reason: ValidationReason::from_wire(&v.reason),
            detail: v.description,
        },
    )
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "error_tests.rs"]
mod error_tests;
