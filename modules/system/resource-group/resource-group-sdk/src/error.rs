// @cpt-dod:cpt-cf-resource-group-dod-sdk-foundation-sdk-errors:p1
//! Public error types for the resource-group module.
//!
//! These errors are safe to expose to other modules and consumers.

use thiserror::Error;

/// Errors that can be returned by the `ResourceGroupClient`.
#[derive(Error, Debug, Clone)]
pub enum ResourceGroupError {
    /// Resource with the specified code was not found.
    #[error("Type not found: {code}")]
    NotFound { code: String },

    /// A type with the specified code already exists.
    #[error("Type already exists: {code}")]
    TypeAlreadyExists { code: String },

    /// Validation error with the provided data.
    #[error("Validation error: {message}")]
    Validation { message: String },

    /// Removing allowed parents or disabling root placement would break
    /// existing group hierarchy relationships.
    #[error("Allowed parents violation: {message}")]
    AllowedParentsViolation { message: String },

    /// Cannot delete a type because groups of this type still exist.
    #[error("Active references exist: {message}")]
    ConflictActiveReferences { message: String },

    /// Parent type is not allowed by the type's `allowed_parents` configuration.
    #[error("Invalid parent type: {message}")]
    InvalidParentType { message: String },

    /// A cycle would be created in the group hierarchy.
    #[error("Cycle detected: {message}")]
    CycleDetected { message: String },

    /// A configured limit (depth, width, etc.) would be exceeded.
    #[error("Limit violation: {message}")]
    LimitViolation { message: String },

    /// Tenant scope incompatibility.
    #[error("Tenant incompatibility: {message}")]
    TenantIncompatibility { message: String },

    /// Service is temporarily unavailable.
    #[error("Service unavailable: {message}")]
    ServiceUnavailable { message: String },

    /// An internal error occurred.
    #[error("Internal error")]
    Internal,
}

impl ResourceGroupError {
    /// Create a `NotFound` error.
    pub fn not_found(code: impl Into<String>) -> Self {
        Self::NotFound { code: code.into() }
    }

    /// Create a `TypeAlreadyExists` error.
    pub fn type_already_exists(code: impl Into<String>) -> Self {
        Self::TypeAlreadyExists { code: code.into() }
    }

    /// Create a `Validation` error.
    pub fn validation(message: impl Into<String>) -> Self {
        Self::Validation {
            message: message.into(),
        }
    }

    /// Create an `AllowedParentsViolation` error.
    pub fn allowed_parents_violation(message: impl Into<String>) -> Self {
        Self::AllowedParentsViolation {
            message: message.into(),
        }
    }

    /// Create a `ConflictActiveReferences` error.
    pub fn conflict_active_references(message: impl Into<String>) -> Self {
        Self::ConflictActiveReferences {
            message: message.into(),
        }
    }

    /// Create an `InvalidParentType` error.
    pub fn invalid_parent_type(message: impl Into<String>) -> Self {
        Self::InvalidParentType {
            message: message.into(),
        }
    }

    /// Create a `CycleDetected` error.
    pub fn cycle_detected(message: impl Into<String>) -> Self {
        Self::CycleDetected {
            message: message.into(),
        }
    }

    /// Create a `LimitViolation` error.
    pub fn limit_violation(message: impl Into<String>) -> Self {
        Self::LimitViolation {
            message: message.into(),
        }
    }

    /// Create a `TenantIncompatibility` error.
    pub fn tenant_incompatibility(message: impl Into<String>) -> Self {
        Self::TenantIncompatibility {
            message: message.into(),
        }
    }

    /// Create a `ServiceUnavailable` error.
    pub fn service_unavailable(message: impl Into<String>) -> Self {
        Self::ServiceUnavailable {
            message: message.into(),
        }
    }

    /// Create an `Internal` error.
    #[must_use]
    pub fn internal() -> Self {
        Self::Internal
    }
}
