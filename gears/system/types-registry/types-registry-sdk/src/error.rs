//! Types Registry SDK error surface — typed projection of [`CanonicalError`].
//!
//! # Opt-in convenience, not the contract
//!
//! Per [ADR 0005][adr] every trait boundary — [`PlatformTypesRegistryApi`] and the
//! legacy [`TypesRegistryClient`] — is `Result<_, CanonicalError>`, and so is every
//! per-item failure inside an operation. [`TypesRegistryError`] is an **opt-in**
//! typed view over that envelope for consumers that want flat dispatch on what
//! types-registry emits. It is *not* part of the trait contract: adding a variant is
//! non-breaking, and the single authoritative AIP-193 classification lives in the
//! impl crate's `From<DomainError> for CanonicalError` ladder (`api::error`) — this
//! projection only reads the finished `CanonicalError`.
//!
//! The conversion is infallible (`From<CanonicalError>`). Anything types-registry does
//! not emit, or emits in a shape this build does not recognize, falls through to
//! [`TypesRegistryError::Other`], which preserves the full [`CanonicalError`].
//!
//! # What types-registry emits — consumer dispatch reference
//!
//! | Disposition | Match arm | HTTP |
//! |---|---|---|
//! | invalid GTS id / query / `$select` / request field (inspect [`FieldIssue::reason`]) | [`TypesRegistryError::Validation`] | 400 |
//! | entity missing (`resource_type` = [`crate::gts::TYPE_RESOURCE_TYPE`]) | [`TypesRegistryError::NotFound`] | 404 |
//! | operation missing (`resource_type` = [`crate::gts::OPERATION_RESOURCE_TYPE`]) | [`TypesRegistryError::NotFound`] | 404 |
//! | `Idempotency-Key` bound to another request (operation resource type) | [`TypesRegistryError::AlreadyExists`] | 409 |
//! | one item of an operation refused (inspect [`AdmissionFailure::reason`]) | [`TypesRegistryError::Admission`] | — (operation item) |
//! | a registration policy refused the candidate | [`TypesRegistryError::PolicyRefused`] | 400 |
//! | accepted, but the operation could not be read back — replay the same key (SPEC D19) | [`TypesRegistryError::ReadBackFailed`] | — (client) |
//! | `register_and_await` ran out of time | [`TypesRegistryError::DeadlineExceeded`] | — (client) |
//! | the caller cancelled | [`TypesRegistryError::Cancelled`] | — (client) |
//! | registry not available | [`TypesRegistryError::Unavailable`] | 503 |
//! | internal failure | [`TypesRegistryError::Internal`] | 500 |
//! | legacy: entity already registered | [`TypesRegistryError::AlreadyExists`] | 409 |
//! | legacy: batch register, required parent type-schema absent | [`TypesRegistryError::ParentNotRegistered`] | — (in-process) |
//! | anything else (forward-compat) | [`TypesRegistryError::Other`] | — |
//!
//! Legacy rows belong to [`TypesRegistryClient`], which T31 deletes together with them.
//!
//! Resource-scoped variants ([`TypesRegistryError::NotFound`] /
//! [`TypesRegistryError::AlreadyExists`]) carry the raw `resource_type`; project it
//! with [`crate::gts::Resource::from_wire`]. The type-schema-vs-instance distinction
//! is intentionally **not** modeled: it is redundant with the method the caller
//! invoked and with the `~` suffix of the `gts_id`, and the canonical boundary
//! classifies both kinds identically (ADR 0005 single-classification).
//!
//! `FailedPrecondition` carries three distinct shapes, each with its own decoder:
//! [`TypesRegistryError::ParentNotRegistered`] (legacy,
//! [`precondition::PARENT_NOT_REGISTERED`](crate::precondition::PARENT_NOT_REGISTERED)),
//! [`TypesRegistryError::PolicyRefused`]
//! ([`precondition::REGISTRATION_POLICY_PREFIX`](crate::precondition::REGISTRATION_POLICY_PREFIX))
//! and [`TypesRegistryError::Admission`] ([`AdmissionFailure::from_canonical`]). None
//! accepts another's shape; a malformed one lands in `Other`.
//!
//! # Refusals from the `ToolKit` layer
//!
//! Some REST refusals are made by `ToolKit`'s extractors before the registry sees the
//! request. Their codes are `ToolKit`'s vocabulary, not this SDK's, and they arrive as
//! [`TypesRegistryError::Validation`] with [`ValidationReason::Unknown`]. The projection
//! keeps neither the HTTP status nor the `resource_type` (`ToolKit`'s own, on the
//! [`CanonicalError`]), so match on `field` and `reason`:
//!
//! | `field` | `reason` | HTTP | when |
//! |---|---|---|---|
//! | `body` | `json_syntax_error` | 400 | the body is not JSON |
//! | `body` | `invalid_json_body` | 422 | the JSON does not fit the request type, e.g. an `expected_resource_version` above `i64::MAX` |
//! | `body` | `missing_json_content_type` | 415 | no JSON `Content-Type` |
//! | `body` | `json_body_read_error` | 413 and others | the body could not be read, e.g. over the size limit |
//! | `query` | `INVALID_QUERY_PARAMS` | 400 | a discovery parameter of the wrong type, e.g. `limit=abc` |
//! | `query` | `invalid_query_string` | 400 | a deletion parameter of the wrong type |
//! | `path` | `invalid_path_params` | 400 | a path segment that is not UTF-8, or an operation id that is not a UUID |
//!
//! `INVALID_SELECT` comes from either layer and is typed. A missing or rejected
//! credential is `Unauthenticated` and lands in [`TypesRegistryError::Other`]. A response
//! the router makes itself, such as `405` for a wrong method, is a `Problem` with no
//! canonical type: `TryFrom<Problem> for CanonicalError` refuses it, so it never reaches
//! this projection.
//!
//! # Consumer integration — three patterns
//!
//! **Pattern 1 — pure propagation (no projection):**
//!
//! ```ignore
//! let page = registry.list_entities(&ctx, request).await?; // ? propagates CanonicalError
//! ```
//!
//! **Pattern 2 — explicit projection at the call site:**
//!
//! ```ignore
//! use types_registry_sdk::TypesRegistryError;
//!
//! match registry.register_entities(&ctx, key.clone(), request).await.map_err(TypesRegistryError::from) {
//!     Err(TypesRegistryError::ReadBackFailed { .. }) => /* replay with the same key */,
//!     Err(TypesRegistryError::Unavailable { .. }) => /* retry later */,
//!     _ => /* … */,
//! }
//! ```
//!
//! **Pattern 3 — transparent chaining via `From<CanonicalError> for OwnError`:**
//!
//! ```ignore
//! impl From<CanonicalError> for OwnConsumerError {
//!     fn from(err: CanonicalError) -> Self {
//!         TypesRegistryError::from(err).into() // route through the typed view
//!     }
//! }
//! // then every call site stays plain `?`.
//! ```
//!
//! Out-of-process consumers reconstruct the canonical error from the wire via
//! `TryFrom<Problem> for CanonicalError` first, then project:
//! `Problem JSON → Problem → CanonicalError → TypesRegistryError`.
//!
//! [`PlatformTypesRegistryApi`]: crate::PlatformTypesRegistryApi
//! [`TypesRegistryClient`]: crate::TypesRegistryClient
//! [adr]: https://github.com/constructorfabric/gears-rust/blob/main/docs/arch/errors/ADR/0005-cpt-cf-adr-sdk-canonical-projection.md

use thiserror::Error;
use toolkit_canonical_errors::{CanonicalError, InvalidArgument};
use uuid::Uuid;

use crate::field::ValidationReason;
use crate::gts::Resource;
use crate::item_failure::AdmissionFailure;
use crate::precondition::{PARENT_NOT_REGISTERED, PolicyParameter};
use crate::reason::aborted::AbortReason;

/// A single field-violation projected from a canonical
/// `InvalidArgument.field_violations[]` entry.
///
/// `reason` is the typed [`ValidationReason`] discriminator consumers dispatch
/// on; `field` is the raw attribution identifier (not a discriminator);
/// `description` is the human-readable message.
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

/// Typed projection of [`CanonicalError`] for Types Registry consumers.
///
/// The impl crate's `From<DomainError> for CanonicalError` is the single
/// authoritative AIP-193 mapping; this enum is a forward-compatible, flat view
/// over the categories types-registry emits, plus the mandatory catch-all
/// [`Self::Other`]. See the [gear docs](self) for the dispatch table and
/// consumer patterns.
#[derive(Error, Debug, Clone)]
#[non_exhaustive]
pub enum TypesRegistryError {
    /// Request-shape validation failure (invalid GTS id, query, or entity
    /// content). Each [`FieldIssue`] carries a typed
    /// [`ValidationReason`]; types-registry
    /// emits exactly one issue per error today, but the `Vec` mirrors the
    /// canonical `field_violations` carrier.
    #[error("validation failed: {} issue(s)", issues.len())]
    Validation {
        /// The projected field violations.
        issues: Vec<FieldIssue>,
    },

    /// No type-schema or instance is registered under the requested id / UUID,
    /// or no admission operation has the requested id. `resource_type` is the
    /// canonical GTS type — [`crate::gts::TYPE_RESOURCE_TYPE`] for an entity,
    /// [`crate::gts::OPERATION_RESOURCE_TYPE`] for an operation; `name` is the
    /// raw identifier the caller supplied.
    #[error("not found [{resource_type}]: {name}")]
    NotFound {
        resource_type: String,
        name: String,
        detail: String,
    },

    /// An entity with the same GTS id already exists (duplicate-on-register,
    /// [`crate::gts::TYPE_RESOURCE_TYPE`]), or an `Idempotency-Key` is already
    /// bound to another request ([`crate::gts::OPERATION_RESOURCE_TYPE`], `name`
    /// is that operation's UUID).
    #[error("already exists [{resource_type}]: {name}")]
    AlreadyExists {
        resource_type: String,
        name: String,
        detail: String,
    },

    /// Batch register: an entity could not be registered because its required
    /// parent type-schema is not yet registered. Register `parent_type_id`
    /// first, then retry `dependent_id`. Reconstructed losslessly from a
    /// `FailedPrecondition` whose `violations[].type` is
    /// [`precondition::PARENT_NOT_REGISTERED`](crate::precondition::PARENT_NOT_REGISTERED).
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
    /// Retrying with the same idempotency key replays it (SPEC D19).
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

    /// The registry is not currently available (e.g. still initializing).
    #[error("service unavailable: {detail}")]
    Unavailable { detail: String },

    /// Unclassified internal failure (HTTP 500). `detail` is already redacted
    /// at the canonical boundary — it never carries the server-side diagnostic.
    #[error("internal error: {detail}")]
    Internal { detail: String },

    /// Catch-all for canonical categories types-registry does not model —
    /// preserves the full [`CanonicalError`] so consumers stay
    /// forward-compatible if the impl crate ever emits a new category.
    #[error("[{}] {}", canonical.gts_type(), canonical.detail())]
    Other { canonical: CanonicalError },
}

// ─────────────────────────────────────────────────────────────────────
// CanonicalError → TypesRegistryError projection.
//
// The typed sub-enum (`field::ValidationReason`) lives next to its wire-string
// constants in `crate::field`; the precondition `type` discriminator stays a
// plain const in `crate::precondition`. This file owns only the top-level enum
// and the dispatch from `CanonicalError`.
// ─────────────────────────────────────────────────────────────────────

impl From<CanonicalError> for TypesRegistryError {
    fn from(err: CanonicalError) -> Self {
        if let Some(projected) = policy_refused(&err)
            .or_else(|| admission(&err))
            .or_else(|| read_back_failed(&err))
            .or_else(|| deadline_exceeded(&err))
        {
            return projected;
        }
        // Borrow the canonical detail before consuming `err`; the borrow ends
        // here so each arm below can move its fields out (no clones).
        let detail = err.detail().to_owned();
        match err {
            CanonicalError::InvalidArgument { ctx, .. } => Self::Validation {
                issues: project_field_issues(ctx),
            },

            // A modeled `NotFound`/`AlreadyExists` always carries its
            // `resource_type` (callers dispatch on it via `TYPE_RESOURCE_TYPE`).
            // A canonical envelope missing that metadata is malformed for our
            // purposes — fall through to `Other` (the `_` arm) so the empty
            // string never masquerades as a typed resource type, and the full
            // `CanonicalError` is preserved for inspection.
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

            // The only `FailedPrecondition` types-registry emits is
            // parent-type-schema-not-registered, discriminated by the
            // `PARENT_NOT_REGISTERED` violation type. The dependent id rides in
            // `resource_name`; the parent id and message ride in that violation
            // (`subject` / `description`). Any other precondition shape is
            // unmodeled and falls through to `Other` (the `_` arm below).
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
                    // Unreachable: the guard guaranteed a matching violation.
                    // Kept total rather than panicking.
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

/// Project a canonical `InvalidArgument` context into the typed field issues.
///
/// Types-registry only ever emits the `FieldViolations` shape; the `Format` /
/// `Constraint` shapes are mapped into a single field-less issue tagged with a
/// distinct synthetic sentinel ([`ValidationReason::Format`] /
/// [`ValidationReason::Constraint`]) so the discriminator is unambiguous and
/// the message is preserved, even though the impl never produces them today.
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
