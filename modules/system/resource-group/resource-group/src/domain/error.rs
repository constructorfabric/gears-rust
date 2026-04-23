// Created: 2026-04-16 by Constructor Tech
// @cpt-dod:cpt-cf-resource-group-algo-sdk-foundation-map-domain-error:p1
// @cpt-dod:cpt-cf-resource-group-dod-testing-error-conversions:p2
//! Domain error types for the resource-group module.

use authz_resolver_sdk::pep::EnforcerError;
use resource_group_sdk::ResourceGroupError;
use thiserror::Error;

/// Domain-specific errors for the resource-group module.
#[allow(unknown_lints, de0309_must_have_domain_model)]
#[derive(Error, Debug)]
pub enum DomainError {
    #[error("Type not found: {code}")]
    TypeNotFound { code: String },

    #[error("Type already exists: {code}")]
    TypeAlreadyExists { code: String },

    #[error("Validation failed: {message}")]
    Validation { message: String },

    #[error("Allowed parents violation: {message}")]
    AllowedParentTypesViolation { message: String },

    #[error("Active references exist: {message}")]
    ConflictActiveReferences { message: String },

    #[error("Group not found: {id}")]
    GroupNotFound { id: uuid::Uuid },

    #[error("Membership not found: {key}")]
    MembershipNotFound { key: String },

    #[error("Duplicate membership: {message}")]
    DuplicateMembership { message: String },

    #[error("Invalid parent type: {message}")]
    InvalidParentType { message: String },

    #[error("Cycle detected: {message}")]
    CycleDetected { message: String },

    #[error("Limit violation: {message}")]
    LimitViolation { message: String },

    #[error("Conflict: {message}")]
    Conflict { message: String },

    /// Second tenant-type root rejected.
    ///
    /// Raised when a `create_group`/`update_group` would leave the RG forest
    /// with more than one root group whose GTS type code starts with
    /// `TENANT_RG_TYPE_PATH`. Enforces
    /// `cpt-cf-resource-group-fr-enforce-tenant-root-uniqueness`. Maps to
    /// `ResourceGroupError::conflict` (HTTP 409).
    #[error("Tenant root already exists: {message}")]
    TenantRootAlreadyExists { message: String },

    /// Cross-tenant link rejected when adding a membership.
    ///
    /// Raised by `MembershipService::add_membership` when the target group's
    /// tenant differs from the tenant of any existing membership for the same
    /// `(resource_type, resource_id)` pair. A resource must belong to groups
    /// of a single tenant.
    #[error("Tenant incompatibility: {message}")]
    TenantIncompatibility { message: String },

    #[error("Access denied: {message}")]
    AccessDenied { message: String },

    #[error("Database error: {message}")]
    Database { message: String },

    #[error("Internal error")]
    InternalError,
}

impl DomainError {
    /// Returns `true` if this error represents a serialization failure
    /// (SQLSTATE `40001`) that is safe to retry.
    ///
    /// Covers `PostgreSQL` `SERIALIZABLE` conflicts and `MySQL`/`MariaDB` deadlocks.
    #[must_use]
    pub fn is_serialization_failure(&self) -> bool {
        match self {
            DomainError::Database { message } => {
                let msg = message.to_ascii_lowercase();
                msg.contains("40001")
                    || msg.contains("could not serialize access")
                    || msg.contains("deadlock detected")
                    || msg.contains("deadlock found when trying to get lock")
            }
            _ => false,
        }
    }

    pub fn type_not_found(code: impl Into<String>) -> Self {
        Self::TypeNotFound { code: code.into() }
    }

    pub fn type_already_exists(code: impl Into<String>) -> Self {
        Self::TypeAlreadyExists { code: code.into() }
    }

    pub fn validation(message: impl Into<String>) -> Self {
        Self::Validation {
            message: message.into(),
        }
    }

    pub fn allowed_parent_types_violation(message: impl Into<String>) -> Self {
        Self::AllowedParentTypesViolation {
            message: message.into(),
        }
    }

    pub fn conflict_active_references(message: impl Into<String>) -> Self {
        Self::ConflictActiveReferences {
            message: message.into(),
        }
    }

    #[must_use]
    pub fn group_not_found(id: uuid::Uuid) -> Self {
        Self::GroupNotFound { id }
    }

    pub fn membership_not_found(key: impl Into<String>) -> Self {
        Self::MembershipNotFound { key: key.into() }
    }

    pub fn duplicate_membership(message: impl Into<String>) -> Self {
        Self::DuplicateMembership {
            message: message.into(),
        }
    }

    pub fn invalid_parent_type(message: impl Into<String>) -> Self {
        Self::InvalidParentType {
            message: message.into(),
        }
    }

    pub fn cycle_detected(message: impl Into<String>) -> Self {
        Self::CycleDetected {
            message: message.into(),
        }
    }

    pub fn limit_violation(message: impl Into<String>) -> Self {
        Self::LimitViolation {
            message: message.into(),
        }
    }

    pub fn conflict(message: impl Into<String>) -> Self {
        Self::Conflict {
            message: message.into(),
        }
    }

    pub fn tenant_root_already_exists(message: impl Into<String>) -> Self {
        Self::TenantRootAlreadyExists {
            message: message.into(),
        }
    }

    pub fn tenant_incompatibility(message: impl Into<String>) -> Self {
        Self::TenantIncompatibility {
            message: message.into(),
        }
    }

    pub fn database(message: impl Into<String>) -> Self {
        Self::Database {
            message: message.into(),
        }
    }
}

/// Convert domain errors to SDK errors for public API consumption.
impl From<DomainError> for ResourceGroupError {
    fn from(e: DomainError) -> Self {
        match e {
            DomainError::TypeNotFound { code } => ResourceGroupError::not_found(code),
            DomainError::TypeAlreadyExists { code } => {
                ResourceGroupError::type_already_exists(code)
            }
            DomainError::Validation { message } => ResourceGroupError::validation(message),
            DomainError::InvalidParentType { message } => {
                ResourceGroupError::invalid_parent_type(message)
            }
            DomainError::CycleDetected { message } => ResourceGroupError::cycle_detected(message),
            DomainError::LimitViolation { message } => ResourceGroupError::limit_violation(message),
            DomainError::AllowedParentTypesViolation { message } => {
                ResourceGroupError::allowed_parent_types_violation(message)
            }
            DomainError::ConflictActiveReferences { message } => {
                ResourceGroupError::conflict_active_references(message)
            }
            DomainError::Conflict { message }
            | DomainError::DuplicateMembership { message }
            | DomainError::TenantRootAlreadyExists { message } => {
                ResourceGroupError::conflict(message)
            }
            DomainError::GroupNotFound { id } => ResourceGroupError::not_found(id.to_string()),
            DomainError::MembershipNotFound { key } => ResourceGroupError::not_found(key),
            DomainError::TenantIncompatibility { message } => {
                ResourceGroupError::tenant_incompatibility(message)
            }
            DomainError::AccessDenied { .. } => ResourceGroupError::access_denied(),
            DomainError::Database { .. } | DomainError::InternalError => {
                ResourceGroupError::internal()
            }
        }
    }
}

impl From<sea_orm::DbErr> for DomainError {
    fn from(e: sea_orm::DbErr) -> Self {
        DomainError::database(format!("{e}"))
    }
}

impl From<modkit_db::DbError> for DomainError {
    fn from(e: modkit_db::DbError) -> Self {
        DomainError::database(format!("{e}"))
    }
}

impl From<EnforcerError> for DomainError {
    fn from(e: EnforcerError) -> Self {
        match e {
            EnforcerError::Denied { deny_reason } => DomainError::AccessDenied {
                message: deny_reason.map_or_else(
                    || "access denied by PDP".to_owned(),
                    |reason| format!("access denied by PDP: {reason:?}"),
                ),
            },
            // PDP RPC or constraint compilation failures are infrastructure problems,
            // not authorization denials — surface as internal errors.
            EnforcerError::EvaluationFailed(_) | EnforcerError::CompileFailed(_) => {
                DomainError::InternalError
            }
        }
    }
}
