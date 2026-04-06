//! Domain error types for the Types Registry module.

use modkit_macros::domain_model;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use types_registry_sdk::TypesRegistryError;

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

/// Domain-level errors for the Types Registry module.
#[domain_model]
#[derive(Error, Debug)]
pub enum DomainError {
    /// The GTS ID format is invalid.
    #[error("Invalid GTS ID: {0}")]
    InvalidGtsId(String),

    /// The requested entity was not found.
    #[error("Entity not found: {0}")]
    NotFound(String),

    /// An entity with the same GTS ID already exists.
    #[error("Entity already exists: {0}")]
    AlreadyExists(String),

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

impl DomainError {
    /// Creates an `InvalidGtsId` error.
    #[must_use]
    pub fn invalid_gts_id(message: impl Into<String>) -> Self {
        Self::InvalidGtsId(message.into())
    }

    /// Creates a `NotFound` error.
    #[must_use]
    pub fn not_found(gts_id: impl Into<String>) -> Self {
        Self::NotFound(gts_id.into())
    }

    /// Creates an `AlreadyExists` error.
    #[must_use]
    pub fn already_exists(gts_id: impl Into<String>) -> Self {
        Self::AlreadyExists(gts_id.into())
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

impl From<DomainError> for TypesRegistryError {
    fn from(e: DomainError) -> Self {
        match e {
            DomainError::InvalidGtsId(msg) => TypesRegistryError::invalid_gts_id(msg),
            DomainError::NotFound(id) => TypesRegistryError::not_found(id),
            DomainError::AlreadyExists(id) => TypesRegistryError::already_exists(id),
            DomainError::ValidationFailed(msg) => TypesRegistryError::validation_failed(msg),
            DomainError::NotInReadyMode => TypesRegistryError::not_in_ready_mode(),
            DomainError::ReadyCommitFailed(errors) => {
                let error_strings: Vec<String> = errors
                    .iter()
                    .map(std::string::ToString::to_string)
                    .collect();
                TypesRegistryError::validation_failed(format!(
                    "Ready commit failed with {} errors: {}",
                    errors.len(),
                    error_strings.join("; ")
                ))
            }
            DomainError::Internal(e) => TypesRegistryError::internal(e.to_string()),
        }
    }
}

#[cfg(test)]
#[path = "error_tests.rs"]
mod tests;
