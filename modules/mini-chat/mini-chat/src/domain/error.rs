use modkit_db::DbError;
use modkit_db::secure::InfraError;
use modkit_db::secure::ScopeError;
use modkit_macros::domain_model;
use thiserror::Error;
use uuid::Uuid;

/// Domain-specific errors for the mini-chat module.
#[domain_model]
#[derive(Error, Debug)]
pub enum DomainError {
    #[error("Chat not found: {id}")]
    ChatNotFound { id: Uuid },

    #[error("Invalid model: {model}")]
    InvalidModel { model: String },

    #[error("Attachment not found: {id}")]
    AttachmentNotFound { id: Uuid },

    #[error("Attachment not ready: {id}")]
    AttachmentNotReady { id: Uuid },

    #[error("Invalid attachment: {message}")]
    InvalidAttachment { message: String },

    #[error("File too large: max {max_bytes} bytes")]
    FileTooLarge { max_bytes: usize },

    #[error("Unsupported file type: {content_type}")]
    UnsupportedFileType { content_type: String },

    #[error("Document limit exceeded: max {max} documents per chat")]
    DocumentLimitExceeded { max: usize },

    #[error("Upload size limit exceeded: max {max_mb} MB per chat")]
    UploadSizeLimitExceeded { max_mb: usize },

    #[error("Upload quota exceeded")]
    UploadQuotaExceeded,

    #[error("Images cannot be used in rag_attachment_ids: {id}")]
    ImageInRagScope { id: Uuid },

    #[error("Attachment ID appears in both attachment_ids and rag_attachment_ids: {id}")]
    AttachmentIdOverlap { id: Uuid },

    #[error("Duplicate attachment ID: {id}")]
    DuplicateAttachmentId { id: Uuid },

    #[error("Validation failed: {message}")]
    Validation { message: String },

    #[error("Database error: {message}")]
    Database { message: String },

    #[error("Conflict: {code}: {message}")]
    Conflict { code: String, message: String },

    #[error("{entity} not found: {id}")]
    NotFound { entity: String, id: Uuid },

    #[error("Already exists: {message}")]
    AlreadyExists { message: String },

    #[error("Message not found: {id}")]
    MessageNotFound { id: Uuid },

    #[error("Invalid reaction target: message {id} is not an assistant message")]
    InvalidReactionTarget { id: Uuid },

    #[error("Model not found: {model_id}")]
    ModelNotFound { model_id: String },

    #[error("Access forbidden: {message}")]
    Forbidden { message: String },

    #[error("Internal error: {message}")]
    Internal { message: String },
}

impl DomainError {
    #[must_use]
    pub fn chat_not_found(id: Uuid) -> Self {
        Self::ChatNotFound { id }
    }

    #[must_use]
    pub fn invalid_model(model: impl Into<String>) -> Self {
        Self::InvalidModel {
            model: model.into(),
        }
    }

    pub fn validation(message: impl Into<String>) -> Self {
        Self::Validation {
            message: message.into(),
        }
    }

    pub fn database(message: impl Into<String>) -> Self {
        Self::Database {
            message: message.into(),
        }
    }

    pub fn conflict(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self::Conflict {
            code: code.into(),
            message: message.into(),
        }
    }

    pub fn not_found(entity: impl Into<String>, id: Uuid) -> Self {
        Self::NotFound {
            entity: entity.into(),
            id,
        }
    }

    pub fn forbidden(message: impl Into<String>) -> Self {
        Self::Forbidden {
            message: message.into(),
        }
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self::Internal {
            message: message.into(),
        }
    }

    #[must_use]
    pub fn message_not_found(id: Uuid) -> Self {
        Self::MessageNotFound { id }
    }

    #[must_use]
    pub fn invalid_reaction_target(id: Uuid) -> Self {
        Self::InvalidReactionTarget { id }
    }

    #[must_use]
    pub fn model_not_found(model_id: impl Into<String>) -> Self {
        Self::ModelNotFound {
            model_id: model_id.into(),
        }
    }

    #[must_use]
    #[allow(clippy::needless_pass_by_value)]
    pub fn database_infra(e: InfraError) -> Self {
        Self::database(e.to_string())
    }
}

impl From<Box<dyn std::error::Error>> for DomainError {
    fn from(value: Box<dyn std::error::Error>) -> Self {
        tracing::debug!(error = %value, "Converting boxed error to DomainError");
        DomainError::internal(value.to_string())
    }
}

/// Helper to convert any displayable error into `DomainError::Database`.
pub fn db_err(e: impl std::fmt::Display) -> DomainError {
    DomainError::database(e.to_string())
}

impl From<DbError> for DomainError {
    fn from(e: DbError) -> Self {
        DomainError::database(e.to_string())
    }
}

impl From<ScopeError> for DomainError {
    #[allow(clippy::cognitive_complexity)]
    fn from(e: ScopeError) -> Self {
        match e {
            ScopeError::Db(ref db_err) => map_db_err(db_err),
            ScopeError::Denied(msg) => {
                tracing::warn!("scope denied: {msg}");
                DomainError::Forbidden {
                    message: "access denied".to_owned(),
                }
            }
            ScopeError::TenantNotInScope { tenant_id } => {
                tracing::warn!("tenant {tenant_id} not in scope");
                DomainError::Forbidden {
                    message: "access denied".to_owned(),
                }
            }
            ScopeError::Invalid(msg) => {
                tracing::error!("invalid scope: {msg}");
                DomainError::internal(msg)
            }
        }
    }
}

impl From<authz_resolver_sdk::EnforcerError> for DomainError {
    #[allow(clippy::cognitive_complexity)]
    fn from(e: authz_resolver_sdk::EnforcerError) -> Self {
        match e {
            authz_resolver_sdk::EnforcerError::Denied { .. }
            | authz_resolver_sdk::EnforcerError::CompileFailed(_) => Self::Forbidden {
                message: e.to_string(),
            },
            authz_resolver_sdk::EnforcerError::EvaluationFailed(ref err) => {
                tracing::error!(error = %err, "AuthZ evaluation failed (internal error)");
                Self::internal(err.to_string())
            }
        }
    }
}

fn map_db_err(db_err: &sea_orm::DbErr) -> DomainError {
    if let Some(sea_orm::SqlErr::UniqueConstraintViolation(constraint_msg)) = db_err.sql_err() {
        return DomainError::Conflict {
            code: "unique_violation".into(),
            message: constraint_msg,
        };
    }
    DomainError::database(db_err.to_string())
}
