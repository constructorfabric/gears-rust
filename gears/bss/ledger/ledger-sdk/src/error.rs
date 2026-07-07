//! `LedgerError` — typed projection of [`CanonicalError`] for BSS Ledger
//! consumers (ADR-0005). The gear's single `From<DomainError> for
//! CanonicalError` ladder is the authoritative AIP-193 classification; this is
//! the forward-compatible typed *view* a consumer matches on. Match the
//! category for coarse handling, `code` for the exact ledger condition (the old
//! `LedgerErrorCode` `SCREAMING_SNAKE` names), and `resource_type`/`resource_name`
//! for the entry / fiscal-period / account it concerns.
//!
//! Consumers: `?` propagates the `CanonicalError`; opt into the typed view with
//! `.map_err(LedgerError::from)` at the call site.

use thiserror::Error;
use toolkit_canonical_errors::{CanonicalError, InvalidArgument};

/// Typed projection of [`CanonicalError`] for ledger consumers.
#[derive(Debug, Clone, Error)]
#[non_exhaustive]
pub enum LedgerError {
    /// Bad request shape/value. `field` is the attributed request field;
    /// `code` is the machine-readable reason.
    #[error("invalid argument [{field}/{code}]: {detail}")]
    InvalidArgument {
        field: String,
        code: String,
        detail: String,
    },
    /// Resource state forbids the operation. `subject` is the violated
    /// resource (`fiscal_period`/`account`/`account_balance`/…); `code` is the
    /// exact precondition.
    #[error("failed precondition [{subject}/{code}]: {detail}")]
    FailedPrecondition {
        subject: String,
        code: String,
        detail: String,
    },
    /// Conflict; the caller may retry. `code` is the abort reason
    /// (`IDEMPOTENCY_PAYLOAD_CONFLICT`/`CURRENCY_SCALE_LOCKED`).
    #[error("aborted [{code}]: {detail}")]
    Aborted { code: String, detail: String },
    /// The named resource does not exist.
    #[error("not found [{resource_type}]: {resource_name}")]
    NotFound {
        resource_type: String,
        resource_name: String,
        detail: String,
    },
    /// A bounded resource is exhausted (backpressure). `code` is the quota
    /// subject (`TENANT_POSTING_LOCKED`).
    #[error("resource exhausted [{code}]: {detail}")]
    ResourceExhausted { code: String, detail: String },
    /// Authorization denial. `reason` is the deny reason.
    #[error("permission denied [{reason}]: {detail}")]
    PermissionDenied { reason: String, detail: String },
    /// The request carried no authenticated `SecurityContext`.
    #[error("unauthenticated: {detail}")]
    Unauthenticated { detail: String },
    /// Transient outage; retry later.
    #[error("service unavailable: {detail}")]
    Unavailable { detail: String },
    /// Unclassified internal failure — `detail` is already redacted at the
    /// canonical boundary (no server-side diagnostic).
    #[error("internal: {detail}")]
    Internal { detail: String },
    /// Catch-all for canonical categories the ledger does not model — preserves
    /// the full [`CanonicalError`] so consumers stay forward-compatible.
    #[error("{canonical}")]
    Other { canonical: CanonicalError },
}

impl From<CanonicalError> for LedgerError {
    fn from(err: CanonicalError) -> Self {
        let detail = err.detail().to_owned();
        match err {
            CanonicalError::InvalidArgument { ctx, .. } => project_invalid_argument(ctx, detail),

            CanonicalError::FailedPrecondition { ctx, .. } => {
                ctx.violations.into_iter().next().map_or_else(
                    || Self::FailedPrecondition {
                        subject: String::new(),
                        code: String::new(),
                        detail: detail.clone(),
                    },
                    |v| Self::FailedPrecondition {
                        subject: v.subject,
                        code: v.type_,
                        detail: v.description,
                    },
                )
            }

            CanonicalError::Aborted { ctx, .. } => Self::Aborted {
                code: ctx.reason,
                detail,
            },

            CanonicalError::NotFound {
                resource_type,
                resource_name,
                ..
            } => Self::NotFound {
                resource_type: resource_type.unwrap_or_default(),
                resource_name: resource_name.unwrap_or_default(),
                detail,
            },

            CanonicalError::ResourceExhausted { ctx, .. } => Self::ResourceExhausted {
                code: ctx
                    .violations
                    .into_iter()
                    .next()
                    .map(|v| v.subject)
                    .unwrap_or_default(),
                detail,
            },

            CanonicalError::PermissionDenied { ctx, .. } => Self::PermissionDenied {
                reason: ctx.reason,
                detail,
            },

            CanonicalError::Unauthenticated { .. } => Self::Unauthenticated { detail },

            CanonicalError::ServiceUnavailable { .. } => Self::Unavailable { detail },

            CanonicalError::Internal { .. } => Self::Internal { detail },

            other => Self::Other { canonical: other },
        }
    }
}

fn project_invalid_argument(ctx: InvalidArgument, detail: String) -> LedgerError {
    let first = match ctx {
        InvalidArgument::FieldViolations { field_violations } => {
            field_violations.into_iter().next()
        }
        InvalidArgument::Format { .. } | InvalidArgument::Constraint { .. } => None,
    };
    first.map_or(
        LedgerError::InvalidArgument {
            field: String::new(),
            code: String::new(),
            detail,
        },
        |v| LedgerError::InvalidArgument {
            field: v.field,
            code: v.reason,
            detail: v.description,
        },
    )
}

#[cfg(test)]
#[path = "error_tests.rs"]
mod tests;
