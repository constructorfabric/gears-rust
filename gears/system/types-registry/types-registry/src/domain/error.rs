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

/// Kind-agnostic Types Registry errors, projected to [`CanonicalError`] through
/// `crate::api::error` for REST and local calls ([ADR 0005][adr]).
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

    /// Local batch-register parent pre-check failure; only a per-item `RegisterResult::Err`,
    /// never produced by the service or REST. Maps to `FailedPrecondition` with reason
    /// `PARENT_NOT_REGISTERED` and lossless `parent_type_id` / `dependent_id` context.
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

/// What only the local client refuses: after an accepted submit (D19), and where an SDK
/// value cannot reach the service. The API ladder writes each one's wire form.
#[domain_model]
#[derive(Error, Debug)]
pub enum LocalClientError {
    /// The submit was accepted, but its operation could not be read back.
    #[error("operation {operation_id} was accepted, but {why}")]
    ReadBackFailed {
        operation_id: uuid::Uuid,
        why: &'static str,
    },
    /// The read-back operation is of the other kind.
    #[error("operation {operation_id} was read back as an operation of the other kind")]
    WrongKind { operation_id: uuid::Uuid },
    /// The read-back operation has no SDK representation.
    #[error("operation {operation_id} could not be represented")]
    Unrepresentable { operation_id: uuid::Uuid },
    /// No operation has this id.
    #[error("no operation with id {operation_id}")]
    OperationNotFound { operation_id: uuid::Uuid },
    /// An `expected_resource_version` above any version the registry issues.
    #[error("{version} is not a resource version this registry issues")]
    VersionOutOfRange { key: String, version: u64 },
}

#[cfg(test)]
mod tests {
    use super::*;
    use toolkit_gts::{GTS_ID_PREFIX, gts_id};

    #[test]
    fn test_error_constructors() {
        let err = DomainError::invalid_gts_id("missing vendor");
        assert!(matches!(err, DomainError::InvalidGtsId(_)));

        let err = DomainError::not_found_by_id(gts_id!("acme.core.events.test.v1~"));
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

        let err = DomainError::already_exists(gts_id!("acme.core.events.test.v1~"));
        assert!(matches!(err, DomainError::AlreadyExists(_)));

        let err = DomainError::validation_failed("schema invalid");
        assert!(matches!(err, DomainError::ValidationFailed(_)));
    }

    #[test]
    fn test_error_display() {
        let err = DomainError::InvalidGtsId("bad format".to_owned());
        assert_eq!(err.to_string(), "Invalid GTS ID: bad format");

        let err = DomainError::not_found_by_id(gts_id!("cf.core.events.test.v1~"));
        assert_eq!(
            err.to_string(),
            format!(
                "Entity not found (GTS ID): {}",
                gts_id!("cf.core.events.test.v1~")
            )
        );

        let err = DomainError::not_found_by_uuid(uuid::Uuid::nil());
        assert_eq!(
            err.to_string(),
            "Entity not found (UUID): 00000000-0000-0000-0000-000000000000"
        );

        let err = DomainError::AlreadyExists(gts_id!("cf.core.events.test.v1~").to_owned());
        assert_eq!(
            err.to_string(),
            format!(
                "Entity already exists: {}",
                gts_id!("cf.core.events.test.v1~")
            )
        );

        let err = DomainError::ValidationFailed("schema invalid".to_owned());
        assert_eq!(err.to_string(), "Validation failed: schema invalid");

        let err = DomainError::NotInReadyMode;
        assert_eq!(err.to_string(), "Not in ready mode");

        let err = DomainError::ReadyCommitFailed(vec![
            ValidationError::new(format!("{GTS_ID_PREFIX}test1~"), "error1"),
            ValidationError::new(format!("{GTS_ID_PREFIX}test2~"), "error2"),
            ValidationError::new(format!("{GTS_ID_PREFIX}test3~"), "error3"),
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
