use thiserror::Error;
use toolkit_db::DbError;
use toolkit_db::secure::InfraError;
use toolkit_db::secure::ScopeError;
use toolkit_macros::domain_model;
use uuid::Uuid;

/// Domain-specific errors using thiserror
#[domain_model]
#[derive(Error, Debug)]
pub enum DomainError {
    #[error("User not found: {id}")]
    UserNotFound { id: Uuid },

    #[error("User with email '{email}' already exists")]
    EmailAlreadyExists { email: String },

    #[error("Invalid email format: '{email}'")]
    InvalidEmail { email: String },

    #[error("Display name cannot be empty")]
    EmptyDisplayName,

    #[error("Display name too long: {len} characters (max: {max})")]
    DisplayNameTooLong { len: usize, max: usize },

    #[error("Database error: {message}")]
    Database { message: String },

    #[error("Validation failed: {field}: {message}")]
    Validation { field: String, message: String },

    #[error("{entity_type} not found: {id}")]
    NotFound { entity_type: String, id: Uuid },

    #[error("Access denied")]
    Forbidden,

    #[error("Internal error")]
    InternalError,
}

impl DomainError {
    #[must_use]
    pub fn user_not_found(id: Uuid) -> Self {
        Self::UserNotFound { id }
    }

    #[must_use]
    pub fn email_already_exists(email: String) -> Self {
        Self::EmailAlreadyExists { email }
    }

    #[must_use]
    pub fn invalid_email(email: String) -> Self {
        Self::InvalidEmail { email }
    }

    #[must_use]
    pub fn empty_display_name() -> Self {
        Self::EmptyDisplayName
    }

    #[must_use]
    pub fn display_name_too_long(len: usize, max: usize) -> Self {
        Self::DisplayNameTooLong { len, max }
    }

    pub fn database(message: impl Into<String>) -> Self {
        Self::Database {
            message: message.into(),
        }
    }

    /// Convert an infrastructure error into a domain database error.
    #[must_use]
    #[allow(clippy::needless_pass_by_value)]
    pub fn database_infra(e: InfraError) -> Self {
        Self::database(e.to_string())
    }

    pub fn validation(field: impl Into<String>, message: impl Into<String>) -> Self {
        Self::Validation {
            field: field.into(),
            message: message.into(),
        }
    }

    #[must_use]
    pub fn not_found(entity_type: impl Into<String>, id: Uuid) -> Self {
        Self::NotFound {
            entity_type: entity_type.into(),
            id,
        }
    }
}

impl From<Box<dyn std::error::Error>> for DomainError {
    fn from(value: Box<dyn std::error::Error>) -> Self {
        tracing::debug!(error = %value, "Converting boxed error to DomainError");

        DomainError::InternalError
    }
}

// TODO(DE1302): `DomainError::database(...)` only accepts a String, so these
// From impls drop the source error. Extend `Database` to hold a boxed source
// so `.source()` returns the original error, then remove these allows.
#[allow(unknown_lints, de1302_error_from_to_string)]
impl From<DbError> for DomainError {
    fn from(e: DbError) -> Self {
        DomainError::database(e.to_string())
    }
}

#[allow(unknown_lints, de1302_error_from_to_string)]
impl From<ScopeError> for DomainError {
    fn from(e: ScopeError) -> Self {
        DomainError::database(e.to_string())
    }
}

impl From<authz_resolver_sdk::EnforcerError> for DomainError {
    fn from(e: authz_resolver_sdk::EnforcerError) -> Self {
        tracing::error!(error = %e, "AuthZ scope resolution failed");
        match e {
            authz_resolver_sdk::EnforcerError::AccessDenied(_)
            | authz_resolver_sdk::EnforcerError::CompileFailed(_) => Self::Forbidden,
            // An evaluation failure, or an unknown outcome class: an internal
            // failure, never a denial.
            _ => Self::InternalError,
        }
    }
}
