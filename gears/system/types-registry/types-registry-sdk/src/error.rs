//! Typed view of [`CanonicalError`] for Types Registry consumers.
//!
//! API contracts and operation-item failures return `CanonicalError`. Convert with
//! [`TypesRegistryError::from`] for typed matching; unrecognized categories and shapes
//! remain intact in [`TypesRegistryError::Other`]. Item failures decode through
//! [`AdmissionFailure::from_canonical`].
//!
//! Unknown validation codes become [`ValidationReason::Unknown`].

use thiserror::Error;
use toolkit_canonical_errors::{CanonicalError, InvalidArgument};
use uuid::Uuid;

use crate::field::ValidationReason;
use crate::gts::Resource;
use crate::item_failure::AdmissionFailure;
use crate::precondition::{PARENT_NOT_REGISTERED, PolicyParameter};
use crate::reason::aborted::AbortReason;

/// A canonical `InvalidArgument.field_violations[]` entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldIssue {
    /// The request field the violation is attributed to (e.g.
    /// [`field::GTS_ID_FIELD`](crate::field::GTS_ID_FIELD)).
    pub field: String,
    /// The typed reason discriminator.
    pub reason: ValidationReason,
    /// Human-readable description of the violation.
    pub description: String,
}

/// Typed projection of [`CanonicalError`]; [`Self::Other`] preserves unrecognized errors.
#[derive(Error, Debug, Clone)]
#[non_exhaustive]
pub enum TypesRegistryError {
    /// Request validation failure with typed field-violation reasons.
    #[error("validation failed: {} issue(s)", issues.len())]
    Validation {
        /// The projected field violations.
        issues: Vec<FieldIssue>,
    },

    /// The entity or operation does not exist. `resource_type` identifies its kind;
    /// `name` is the requested identifier or UUID.
    #[error("not found [{resource_type}]: {name}")]
    NotFound {
        resource_type: String,
        name: String,
        detail: String,
    },

    /// An entity already exists, or an `Idempotency-Key` is bound to another request.
    /// For key conflicts, `resource_type` is [`crate::gts::OPERATION_RESOURCE_TYPE`]
    /// and `name` is the operation UUID.
    #[error("already exists [{resource_type}]: {name}")]
    AlreadyExists {
        resource_type: String,
        name: String,
        detail: String,
    },

    /// Legacy batch registration failed because its required parent Type Schema is absent.
    /// Register `parent_type_id`, then retry `dependent_id`.
    #[error(
        "cannot register {dependent_id}: required type-schema {parent_type_id} is not registered"
    )]
    ParentNotRegistered {
        parent_type_id: String,
        dependent_id: String,
        detail: String,
    },

    /// One item of a registration or deletion was refused. `key` is the item's
    /// canonical spelling; dispatch on [`AdmissionFailure::reason`].
    #[error("{key} was refused ({}): {}", failure.reason, failure.message)]
    Admission {
        key: String,
        failure: AdmissionFailure,
    },

    /// A registration policy refused the candidate `gts_id`. `region` is the policy
    /// region that refused it (`<default>` for the default policy).
    #[error("registration policy {parameter} refused {gts_id}: {detail}")]
    PolicyRefused {
        gts_id: String,
        parameter: PolicyParameter,
        region: String,
        detail: String,
    },

    /// The mutation was accepted as `operation_id`, but reading it back failed.
    /// Retrying with the same idempotency key replays it.
    #[error("operation {operation_id} was accepted but not read back: {detail}")]
    ReadBackFailed { operation_id: Uuid, detail: String },

    /// Waiting for an operation ran out of time; the accepted write is not cancelled.
    /// `operation_id` is known once the submission was accepted.
    #[error("deadline exceeded: {detail}")]
    DeadlineExceeded {
        operation_id: Option<Uuid>,
        detail: String,
    },

    /// The caller's cancellation token stopped the call.
    #[error("cancelled: {detail}")]
    Cancelled { detail: String },

    /// The registry is temporarily unavailable.
    #[error("service unavailable: {detail}")]
    Unavailable { detail: String },

    /// Unclassified failure (HTTP 500), with redacted detail.
    #[error("internal error: {detail}")]
    Internal { detail: String },

    /// Unrecognized category or shape, preserving the full [`CanonicalError`].
    #[error("[{}] {}", canonical.gts_type(), canonical.detail())]
    Other { canonical: CanonicalError },
}

impl From<CanonicalError> for TypesRegistryError {
    fn from(err: CanonicalError) -> Self {
        if let Some(projected) = policy_refused(&err)
            .or_else(|| admission(&err))
            .or_else(|| read_back_failed(&err))
            .or_else(|| deadline_exceeded(&err))
        {
            return projected;
        }
        let detail = err.detail().to_owned();
        match err {
            CanonicalError::InvalidArgument { ctx, .. } => Self::Validation {
                issues: project_field_issues(ctx),
            },

            // Missing resource metadata falls through to `Other`.
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

            // Parent refusals use the dependent id as `resource_name` and the parent id
            // as the violation subject. Other shapes fall through to `Other`.
            CanonicalError::FailedPrecondition {
                ctx, resource_name, ..
            } if ctx
                .violations
                .iter()
                .any(|v| v.type_ == PARENT_NOT_REGISTERED) =>
            {
                let dependent_id = resource_name.unwrap_or_default();
                match ctx
                    .violations
                    .into_iter()
                    .find(|v| v.type_ == PARENT_NOT_REGISTERED)
                {
                    Some(v) => Self::ParentNotRegistered {
                        parent_type_id: v.subject,
                        dependent_id,
                        detail: v.description,
                    },
                    // The guard guarantees a match; retain a total, non-panicking fallback.
                    None => Self::ParentNotRegistered {
                        parent_type_id: String::new(),
                        dependent_id,
                        detail,
                    },
                }
            }

            CanonicalError::Cancelled { .. } => Self::Cancelled { detail },

            CanonicalError::ServiceUnavailable { .. } => Self::Unavailable { detail },

            CanonicalError::Internal { .. } => Self::Internal { detail },

            other => Self::Other { canonical: other },
        }
    }
}

/// An entity-scoped `FailedPrecondition` with exactly one registration-policy violation.
fn policy_refused(err: &CanonicalError) -> Option<TypesRegistryError> {
    let CanonicalError::FailedPrecondition {
        ctx,
        resource_type,
        resource_name,
        ..
    } = err
    else {
        return None;
    };
    if Resource::from_wire(resource_type.as_deref()?) != Resource::Entity {
        return None;
    }
    let [violation] = ctx.violations.as_slice() else {
        return None;
    };
    Some(TypesRegistryError::PolicyRefused {
        gts_id: resource_name.clone()?,
        parameter: PolicyParameter::from_wire(&violation.type_)?,
        region: violation.subject.clone(),
        detail: violation.description.clone(),
    })
}

/// An item failure, decoded only from the exact shape its encoder writes.
fn admission(err: &CanonicalError) -> Option<TypesRegistryError> {
    let failure = AdmissionFailure::from_canonical(err)?;
    Some(TypesRegistryError::Admission {
        key: err.resource_name()?.to_owned(),
        failure,
    })
}

/// `Aborted` + `OPERATION_READ_FAILED` naming an operation by a well-formed UUID.
fn read_back_failed(err: &CanonicalError) -> Option<TypesRegistryError> {
    let CanonicalError::Aborted {
        ctx,
        resource_type,
        resource_name,
        ..
    } = err
    else {
        return None;
    };
    if Resource::from_wire(resource_type.as_deref()?) != Resource::Operation
        || AbortReason::from_wire(&ctx.reason) != AbortReason::OperationReadFailed
    {
        return None;
    }
    Some(TypesRegistryError::ReadBackFailed {
        operation_id: Uuid::parse_str(resource_name.as_deref()?).ok()?,
        detail: err.detail().to_owned(),
    })
}

/// An operation-scoped `DeadlineExceeded`; a present but malformed id is not projected.
fn deadline_exceeded(err: &CanonicalError) -> Option<TypesRegistryError> {
    let CanonicalError::DeadlineExceeded {
        resource_type,
        resource_name,
        ..
    } = err
    else {
        return None;
    };
    if Resource::from_wire(resource_type.as_deref()?) != Resource::Operation {
        return None;
    }
    let operation_id = match resource_name.as_deref() {
        None => None,
        Some(name) => Some(Uuid::parse_str(name).ok()?),
    };
    Some(TypesRegistryError::DeadlineExceeded {
        operation_id,
        detail: err.detail().to_owned(),
    })
}

/// Project field violations; format and constraint errors become field-less issues
/// with distinct synthetic reasons.
fn project_field_issues(ctx: InvalidArgument) -> Vec<FieldIssue> {
    match ctx {
        InvalidArgument::FieldViolations { field_violations } => field_violations
            .into_iter()
            .map(|v| FieldIssue {
                field: v.field,
                reason: ValidationReason::from_wire(&v.reason),
                description: v.description,
            })
            .collect(),
        InvalidArgument::Format { format } => vec![FieldIssue {
            field: String::new(),
            reason: ValidationReason::Format,
            description: format,
        }],
        InvalidArgument::Constraint { constraint } => vec![FieldIssue {
            field: String::new(),
            reason: ValidationReason::Constraint,
            description: constraint,
        }],
    }
}

#[cfg(test)]
#[path = "error_tests.rs"]
mod error_tests;
