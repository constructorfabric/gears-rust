//! Domain error types for the Types Registry gear.

use serde::{Deserialize, Serialize};
use thiserror::Error;
use toolkit_macros::domain_model;

/// A structured validation error with typed fields.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ValidationError {
    /// The GTS ID of the entity that failed validation.
    pub gts_id: String,
    /// The validation error message.
    pub message: String,
}

impl ValidationError {
    /// Creates a new validation error.
    #[must_use]
    pub fn new(gts_id: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            gts_id: gts_id.into(),
            message: message.into(),
        }
    }

    /// Parses a validation error from a string in the format "`gts_id`: message".
    #[must_use]
    pub fn from_string(s: &str) -> Self {
        if let Some((gts_id, message)) = s.split_once(": ") {
            Self::new(gts_id, message)
        } else {
            Self::new("unknown", s)
        }
    }
}

impl std::fmt::Display for ValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.gts_id, self.message)
    }
}

/// Domain-level errors for the Types Registry gear.
///
/// This enum is intentionally **kind-agnostic** — the storage layer doesn't
/// know whether a string identifies a type-schema or an instance, it just
/// stores and retrieves entities by their GTS ID. Per [ADR 0005][adr] this
/// enum is mapped to the platform [`CanonicalError`] by the single
/// `From<DomainError> for CanonicalError` ladder in `crate::api::rest::error`;
/// both the REST boundary and the in-process `TypesRegistryLocalClient` route
/// through that one ladder.
///
/// [`CanonicalError`]: toolkit_canonical_errors::CanonicalError
/// [adr]: https://github.com/constructorfabric/gears-rust/blob/main/docs/arch/errors/ADR/0005-cpt-cf-adr-sdk-canonical-projection.md
#[domain_model]
#[derive(Error, Debug)]
pub enum DomainError {
    /// The GTS ID format is invalid.
    #[error("Invalid GTS ID: {0}")]
    InvalidGtsId(String),

    /// The requested entity was not found. `kind` records which lookup
    /// surface the caller used (GTS id vs. UUID v5) so the REST layer
    /// renders an accurate "No entity with X: …" message and SDK
    /// conversions stay symmetric.
    #[error("Entity not found ({kind}): {target}")]
    NotFound { kind: LookupKind, target: String },

    /// An entity with the same GTS ID already exists.
    #[error("Entity already exists: {0}")]
    AlreadyExists(String),

    /// A batch-register item could not be registered because its required
    /// parent type-schema is not yet registered. Produced only by the
    /// in-process `TypesRegistryLocalClient` parent pre-check (never by the
    /// kind-agnostic service), and surfaced only as a per-item
    /// `RegisterResult::Err` — it is unreachable from REST handlers. Maps to a
    /// `FailedPrecondition` (wire `type` `PARENT_NOT_REGISTERED`) that carries
    /// `parent_type_id` / `dependent_id` losslessly; see
    /// `crate::api::rest::error`.
    #[error(
        "Cannot register {dependent_id}: required type-schema {parent_type_id} is not registered"
    )]
    ParentTypeSchemaNotRegistered {
        /// The parent type-schema id that must be registered first.
        parent_type_id: String,
        /// The id of the entity whose registration failed.
        dependent_id: String,
    },

    /// The list/query parameters are syntactically invalid (e.g. an
    /// out-of-spec wildcard pattern). Distinct from `InvalidGtsId`, which
    /// covers id-shaped inputs.
    #[error("Invalid query: {0}")]
    InvalidQuery(String),

    /// Validation of the entity content failed.
    #[error("Validation failed: {0}")]
    ValidationFailed(String),

    /// The operation requires ready mode but registry is in configuration mode.
    #[error("Not in ready mode")]
    NotInReadyMode,

    /// Multiple validation errors occurred during `switch_to_ready`.
    #[error("Ready commit failed with {} errors", .0.len())]
    ReadyCommitFailed(Vec<ValidationError>),

    /// An internal error occurred.
    #[error("Internal error: {0}")]
    Internal(#[from] anyhow::Error),
}

/// Identifies which surface a `NotFound` lookup used. Carried inside
/// [`DomainError::NotFound`] so renderers (REST, logs) can produce
/// accurate "No entity with X" messages.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LookupKind {
    /// Lookup by canonical GTS id string.
    GtsId,
    /// Lookup by deterministic UUID v5.
    Uuid,
}

impl std::fmt::Display for LookupKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::GtsId => f.write_str("GTS ID"),
            Self::Uuid => f.write_str("UUID"),
        }
    }
}

impl DomainError {
    /// Creates an `InvalidGtsId` error.
    #[must_use]
    pub fn invalid_gts_id(message: impl Into<String>) -> Self {
        Self::InvalidGtsId(message.into())
    }

    /// Creates a `NotFound` error for a GTS-id-keyed lookup miss.
    #[must_use]
    pub fn not_found_by_id(gts_id: impl Into<String>) -> Self {
        Self::NotFound {
            kind: LookupKind::GtsId,
            target: gts_id.into(),
        }
    }

    /// Creates a `NotFound` error for a UUID-keyed lookup miss.
    #[must_use]
    pub fn not_found_by_uuid(uuid: uuid::Uuid) -> Self {
        Self::NotFound {
            kind: LookupKind::Uuid,
            target: uuid.to_string(),
        }
    }

    /// Creates an `AlreadyExists` error.
    #[must_use]
    pub fn already_exists(gts_id: impl Into<String>) -> Self {
        Self::AlreadyExists(gts_id.into())
    }

    /// Creates an `InvalidQuery` error.
    #[must_use]
    pub fn invalid_query(message: impl Into<String>) -> Self {
        Self::InvalidQuery(message.into())
    }

    /// Creates a `ValidationFailed` error.
    #[must_use]
    pub fn validation_failed(message: impl Into<String>) -> Self {
        Self::ValidationFailed(message.into())
    }

    /// Returns the list of validation errors if this is a `ReadyCommitFailed` error.
    #[must_use]
    pub fn validation_errors(&self) -> Option<&[ValidationError]> {
        match self {
            Self::ReadyCommitFailed(errors) => Some(errors),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_error_constructors() {
        let err = DomainError::invalid_gts_id("missing vendor");
        assert!(matches!(err, DomainError::InvalidGtsId(_)));

        let err = DomainError::not_found_by_id("gts.acme.core.events.test.v1~");
        assert!(matches!(
            err,
            DomainError::NotFound {
                kind: LookupKind::GtsId,
                ..
            }
        ));

        let err = DomainError::not_found_by_uuid(uuid::Uuid::nil());
        assert!(matches!(
            err,
            DomainError::NotFound {
                kind: LookupKind::Uuid,
                ..
            }
        ));

        let err = DomainError::already_exists("gts.acme.core.events.test.v1~");
        assert!(matches!(err, DomainError::AlreadyExists(_)));

        let err = DomainError::validation_failed("schema invalid");
        assert!(matches!(err, DomainError::ValidationFailed(_)));
    }

    #[test]
    fn test_error_display() {
        let err = DomainError::InvalidGtsId("bad format".to_owned());
        assert_eq!(err.to_string(), "Invalid GTS ID: bad format");

        let err = DomainError::not_found_by_id("gts.cf.core.events.test.v1~");
        assert_eq!(
            err.to_string(),
            "Entity not found (GTS ID): gts.cf.core.events.test.v1~"
        );

        let err = DomainError::not_found_by_uuid(uuid::Uuid::nil());
        assert_eq!(
            err.to_string(),
            "Entity not found (UUID): 00000000-0000-0000-0000-000000000000"
        );

        let err = DomainError::AlreadyExists("gts.cf.core.events.test.v1~".to_owned());
        assert_eq!(
            err.to_string(),
            "Entity already exists: gts.cf.core.events.test.v1~"
        );

        let err = DomainError::ValidationFailed("schema invalid".to_owned());
        assert_eq!(err.to_string(), "Validation failed: schema invalid");

        let err = DomainError::NotInReadyMode;
        assert_eq!(err.to_string(), "Not in ready mode");

        let err = DomainError::ReadyCommitFailed(vec![
            ValidationError::new("gts.test1~", "error1"),
            ValidationError::new("gts.test2~", "error2"),
            ValidationError::new("gts.test3~", "error3"),
        ]);
        assert_eq!(err.to_string(), "Ready commit failed with 3 errors");
    }

    #[test]
    fn test_internal_error_from_anyhow() {
        let anyhow_err = anyhow::anyhow!("test error");
        let domain_err: DomainError = anyhow_err.into();
        assert!(matches!(domain_err, DomainError::Internal(_)));
    }
}
