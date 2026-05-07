//! Internal `DomainError` and translation to the public `FileStorageError`.

use file_storage_sdk::FileStorageError;
use modkit_db::DbError;
use thiserror::Error;

#[derive(Debug, Clone, Error)]
pub enum DomainError {
    #[error("not found")]
    NotFound,

    #[error("access denied: {0}")]
    AccessDenied(String),

    #[error("bad request: {0}")]
    BadRequest(String),

    #[error("etag mismatch")]
    EtagMismatch,

    #[error("delete in progress")]
    DeleteInProgress,

    #[error("capability unavailable: {0}")]
    CapabilityUnavailable(String),

    #[error("payload too large (max {max_bytes} bytes)")]
    PayloadTooLarge { max_bytes: u64 },

    #[error("upload expired")]
    UploadExpired,

    #[error("backend failure: {0}")]
    BackendFailure(String),

    #[error("conflict on conditional update")]
    Conflict,

    #[error("internal: {0}")]
    Internal(String),

    #[error("database error: {0}")]
    Database(String),
}

impl From<DbError> for DomainError {
    fn from(e: DbError) -> Self {
        Self::Database(e.to_string())
    }
}

impl DomainError {
    pub fn bad_request(detail: impl Into<String>) -> Self {
        Self::BadRequest(detail.into())
    }

    pub fn internal(detail: impl Into<String>) -> Self {
        Self::Internal(detail.into())
    }

    pub fn capability(detail: impl Into<String>) -> Self {
        Self::CapabilityUnavailable(detail.into())
    }

    pub fn backend(detail: impl Into<String>) -> Self {
        Self::BackendFailure(detail.into())
    }
}

impl From<authz_resolver_sdk::EnforcerError> for DomainError {
    fn from(e: authz_resolver_sdk::EnforcerError) -> Self {
        match e {
            authz_resolver_sdk::EnforcerError::Denied { .. }
            | authz_resolver_sdk::EnforcerError::CompileFailed(_) => {
                Self::AccessDenied(e.to_string())
            }
            authz_resolver_sdk::EnforcerError::EvaluationFailed(_) => Self::Internal(e.to_string()),
        }
    }
}

impl From<modkit_db::secure::ScopeError> for DomainError {
    fn from(e: modkit_db::secure::ScopeError) -> Self {
        match e {
            modkit_db::secure::ScopeError::Denied(msg) => Self::AccessDenied(msg.to_string()),
            modkit_db::secure::ScopeError::Invalid(msg) => Self::Internal(format!("scope invalid: {msg}")),
            modkit_db::secure::ScopeError::Db(e) => Self::Internal(format!("database error: {e}")),
            modkit_db::secure::ScopeError::TenantNotInScope { tenant_id } => {
                Self::AccessDenied(format!("tenant {tenant_id} not in scope"))
            }
        }
    }
}

impl From<DomainError> for FileStorageError {
    fn from(e: DomainError) -> Self {
        match e {
            DomainError::NotFound => Self::NotFound,
            DomainError::AccessDenied(_) => Self::AccessDenied,
            DomainError::BadRequest(s) => Self::BadRequest(s),
            DomainError::EtagMismatch => Self::EtagMismatch,
            DomainError::DeleteInProgress => Self::DeleteInProgress,
            DomainError::CapabilityUnavailable(s) => Self::CapabilityUnavailable(s),
            DomainError::PayloadTooLarge { max_bytes } => Self::PayloadTooLarge { max_bytes },
            DomainError::UploadExpired => Self::UploadExpired,
            DomainError::BackendFailure(s) => Self::BackendFailure(s),
            DomainError::Conflict => Self::BackendFailure("conflict on conditional update".into()),
            DomainError::Internal(_) | DomainError::Database(_) => Self::Internal,
        }
    }
}
