//! Public error types for the `types` module.
//!
//! These errors are safe to expose to other modules and consumers.

use thiserror::Error;

/// Errors that can be returned by the `TypesClient`.
#[derive(Error, Debug, Clone)]
pub enum TypesError {
    /// Core types are not yet registered.
    #[error("Core types not ready")]
    NotReady,

    /// Failed to register core types.
    #[error("Registration failed: {0}")]
    RegistrationFailed(String),

    /// An internal error occurred.
    #[error("Internal error: {0}")]
    Internal(String),
}

impl TypesError {
    /// Creates a `NotReady` error.
    #[must_use]
    pub const fn not_ready() -> Self {
        Self::NotReady
    }

    /// Creates a `RegistrationFailed` error.
    #[must_use]
    pub fn registration_failed(message: impl Into<String>) -> Self {
        Self::RegistrationFailed(message.into())
    }

    /// Creates an `Internal` error.
    #[must_use]
    pub fn internal(message: impl Into<String>) -> Self {
        Self::Internal(message.into())
    }

    /// Returns `true` if this is a not ready error.
    #[must_use]
    pub const fn is_not_ready(&self) -> bool {
        matches!(self, Self::NotReady)
    }

    /// Returns `true` if this is a registration failed error.
    #[must_use]
    pub const fn is_registration_failed(&self) -> bool {
        matches!(self, Self::RegistrationFailed(_))
    }
}

#[cfg(test)]
#[path = "error_tests.rs"]
mod tests;
