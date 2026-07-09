//! Types Registry SDK error surface — typed projection of [`CanonicalError`].
//!
//! # Opt-in convenience, not the contract
//!
//! Per [ADR 0005][adr] the [`TypesRegistryClient`] trait boundary is
//! `Result<_, CanonicalError>` (and every per-item `Result` it returns inside a
//! map or [`RegisterResult`](crate::RegisterResult) carries `CanonicalError`
//! too). [`TypesRegistryError`] is an **opt-in** typed view over that envelope,
//! shipped for consumers that want flat dispatch on the categories
//! types-registry emits. It is *not* part of the trait contract: adding a
//! variant is non-breaking, and the single authoritative AIP-193 classification
//! lives in the impl crate's one `From<DomainError> for CanonicalError` ladder
//! (`api::rest::error`) — this projection only reads the finished
//! `CanonicalError`.
//!
//! The conversion is infallible (`From<CanonicalError>`). Canonical categories
//! types-registry does not emit fall through to [`TypesRegistryError::Other`],
//! which preserves the full [`CanonicalError`] for inspection / forward-compatible
//! dispatch on the inner variant.
//!
//! # What types-registry emits — consumer dispatch reference
//!
//! | Disposition | Match arm | HTTP |
//! |---|---|---|
//! | invalid GTS id / query / entity content (inspect [`FieldIssue::reason`]) | [`TypesRegistryError::Validation`] | 400 |
//! | type-schema / instance missing | [`TypesRegistryError::NotFound`] | 404 |
//! | duplicate-on-register | [`TypesRegistryError::AlreadyExists`] | 409 |
//! | batch register: required parent type-schema absent | [`TypesRegistryError::ParentNotRegistered`] | — (in-process batch outcome) |
//! | registry still initializing | [`TypesRegistryError::Unavailable`] | 503 |
//! | internal failure | [`TypesRegistryError::Internal`] | 500 |
//! | anything else (forward-compat) | [`TypesRegistryError::Other`] | — |
//!
//! Resource-scoped variants ([`TypesRegistryError::NotFound`] /
//! [`TypesRegistryError::AlreadyExists`]) carry the raw `resource_type`; match it
//! against [`crate::gts::TYPE_RESOURCE_TYPE`]. The type-schema-vs-instance
//! distinction the legacy error enum carried is intentionally **not** modeled:
//! it is redundant with the method the caller invoked (`get_type_schema` vs
//! `get_instance`) and with the `~` suffix of the `gts_id`, and the canonical
//! boundary classifies both kinds identically (ADR 0005 single-classification).
//!
//! [`TypesRegistryError::ParentNotRegistered`] is the one structured
//! batch-registration outcome; it is reconstructed losslessly from a
//! `FailedPrecondition` whose `violations[].type` is
//! [`precondition::PARENT_NOT_REGISTERED`](crate::precondition::PARENT_NOT_REGISTERED)
//! (`subject` ⇒ parent id, `resource_name` ⇒ dependent id).
//!
//! # Consumer integration — three patterns
//!
//! **Pattern 1 — pure propagation (no projection):**
//!
//! ```ignore
//! let schema = tr_client.get_type_schema(type_id).await?; // ? propagates CanonicalError
//! ```
//!
//! **Pattern 2 — explicit projection at the call site:**
//!
//! ```ignore
//! use types_registry_sdk::TypesRegistryError;
//!
//! let res = tr_client.get_type_schema(type_id).await
//!     .map_err(TypesRegistryError::from);
//! match res {
//!     Err(TypesRegistryError::NotFound { .. }) => /* unregistered type */,
//!     Err(TypesRegistryError::Unavailable { .. }) => /* retry: still initializing */,
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
//! [`TypesRegistryClient`]: crate::TypesRegistryClient
//! [adr]: https://github.com/constructorfabric/gears-rust/blob/main/docs/arch/errors/ADR/0005-cpt-cf-adr-sdk-canonical-projection.md

use thiserror::Error;
use toolkit_canonical_errors::{CanonicalError, InvalidArgument};

use crate::field::ValidationReason;
use crate::precondition::PARENT_NOT_REGISTERED;

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
    /// [`ValidationReason`](crate::field::ValidationReason); types-registry
    /// emits exactly one issue per error today, but the `Vec` mirrors the
    /// canonical `field_violations` carrier.
    #[error("validation failed: {} issue(s)", issues.len())]
    Validation {
        /// The projected field violations.
        issues: Vec<FieldIssue>,
    },

    /// No type-schema or instance is registered under the requested id / UUID.
    /// `resource_type` is the canonical GTS type — match it against
    /// [`crate::gts::TYPE_RESOURCE_TYPE`]; `name` is the raw identifier the
    /// caller supplied.
    #[error("not found [{resource_type}]: {name}")]
    NotFound {
        resource_type: String,
        name: String,
        detail: String,
    },

    /// An entity with the same GTS id already exists (duplicate-on-register).
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

            CanonicalError::ServiceUnavailable { .. } => Self::Unavailable { detail },

            CanonicalError::Internal { .. } => Self::Internal { detail },

            other => Self::Other { canonical: other },
        }
    }
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
