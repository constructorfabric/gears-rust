use thiserror::Error;
use toolkit_db::DbError;
use toolkit_db::secure::InfraError;
use toolkit_db::secure::ScopeError;
use toolkit_macros::domain_model;
use uuid::Uuid;

/// Resource kind carried by [`DomainError::NotFound`]. The REST layer picks the
/// problem `resource_type` from it, so it is an enum rather than a free string.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotFoundEntity {
    Chat,
    Attachment,
}

impl std::fmt::Display for NotFoundEntity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Chat => "Chat",
            Self::Attachment => "Attachment",
        })
    }
}

/// Domain-specific errors for the mini-chat gear.
#[domain_model]
#[derive(Error, Debug)]
pub enum DomainError {
    #[error("Chat not found: {id}")]
    ChatNotFound { id: Uuid },

    #[error("Invalid model: {model}")]
    InvalidModel { model: String },

    #[error("Validation failed: {message}")]
    Validation { message: String },

    /// A client-side `OData` query error (bad `$filter`, `$orderby`, `limit`
    /// or cursor). `Db` / `ParsingUnavailable` are mapped to `Database`
    /// instead. The REST layer renders it through the canonical `OData`
    /// mapping so clients get the `field_violations[].reason` codes.
    #[error(transparent)]
    OData(toolkit_odata::Error),

    #[error("Database error: {message}")]
    Database { message: String },

    #[error("Conflict: {code}: {message}")]
    Conflict { code: String, message: String },

    #[error("{entity} not found: {id}")]
    NotFound { entity: NotFoundEntity, id: Uuid },

    #[error("Access denied")]
    Forbidden,

    /// The authorization decision point could not be evaluated (PDP down or
    /// failing). Still fail closed (no access), but reported as 503 so
    /// clients and monitoring can tell an outage from a denial.
    #[error("Authorization service unavailable")]
    AuthzUnavailable,

    #[error("Message not found: {id}")]
    MessageNotFound { id: Uuid },

    #[error("Invalid reaction target: message {id} is not an assistant message")]
    InvalidReactionTarget { id: Uuid },

    #[error("Model not found: {model_id}")]
    ModelNotFound { model_id: String },

    #[error("Internal error: {message}")]
    InternalError { message: String },

    /// An outbox enqueue failed. The typed `OutboxError`
    /// is kept as the error source so the chain survives; the REST layer derives
    /// the HTTP status from the inner variant (oversize is a client error, the
    /// rest are server faults).
    #[error(transparent)]
    Outbox(#[from] crate::domain::repos::OutboxError),

    #[error("Web search is currently disabled")]
    WebSearchDisabled,

    #[error("Image input is currently disabled")]
    ImagesDisabled,

    #[error("Unsupported file type: {mime}")]
    UnsupportedFileType { mime: String },

    #[error("File too large: {message}")]
    FileTooLarge { message: String },

    #[error("Document limit exceeded: {message}")]
    DocumentLimitExceeded { message: String },

    #[error("Storage limit exceeded: {message}")]
    StorageLimitExceeded { message: String },

    /// Provider returned an error. `sanitized_message` is pre-sanitized by
    /// `sanitize_provider_message()` at construction — safe for client exposure.
    #[error("Provider error: {sanitized_message}")]
    ProviderError {
        code: String,
        sanitized_message: String,
    },
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

    #[must_use]
    pub fn not_found(entity: NotFoundEntity, id: Uuid) -> Self {
        Self::NotFound { entity, id }
    }

    #[must_use]
    pub fn attachment_not_found(id: Uuid) -> Self {
        Self::not_found(NotFoundEntity::Attachment, id)
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self::InternalError {
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

// TODO(DE1302): `DomainError::database(...)` only accepts a String, so the
// source `DbError` is dropped. Extend `Database` to hold the source error and
// remove this allow.
#[allow(unknown_lints, de1302_error_from_to_string)]
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
                DomainError::Forbidden
            }
            ScopeError::TenantNotInScope { tenant_id } => {
                tracing::warn!("tenant {tenant_id} not in scope");
                DomainError::Forbidden
            }
            ScopeError::Invalid(msg) => {
                tracing::error!("invalid scope: {msg}");
                DomainError::internal(msg)
            }
            // `ScopeError` is `#[non_exhaustive]`: variants this gear has no
            // specific answer for (today the graph-query refusals, which it can
            // never trigger) map to an internal error, like `Invalid`.
            other => {
                tracing::error!("invalid scope: {other}");
                DomainError::internal(format!("invalid scope: {other}"))
            }
        }
    }
}

// TODO(DE1302): `DomainError::internal(...)` only accepts a String, so the
// source `EnforcerError` is dropped. Extend the variant to hold the source and
// remove this allow.
#[allow(unknown_lints, de1302_error_from_to_string)]
impl From<authz_resolver_sdk::EnforcerError> for DomainError {
    #[allow(clippy::cognitive_complexity)]
    fn from(e: authz_resolver_sdk::EnforcerError) -> Self {
        match e {
            authz_resolver_sdk::EnforcerError::Denied { ref deny_reason } => {
                tracing::warn!(deny_reason = ?deny_reason, "AuthZ denied access");
                Self::Forbidden
            }
            authz_resolver_sdk::EnforcerError::CompileFailed(ref err) => {
                tracing::warn!(error = %err, "AuthZ constraint compile failed - access denied");
                Self::Forbidden
            }
            // Fail closed, but as 503: the PDP could not decide, which is
            // not a denial.
            authz_resolver_sdk::EnforcerError::EvaluationFailed(ref err) => {
                tracing::error!(error = %err, "AuthZ evaluation failed - request refused");
                Self::AuthzUnavailable
            }
        }
    }
}

fn map_db_err(db_err: &sea_orm::DbErr) -> DomainError {
    if let Some(sea_orm::SqlErr::UniqueConstraintViolation(msg)) = db_err.sql_err() {
        return DomainError::Conflict {
            code: "unique_violation".into(),
            message: msg,
        };
    }
    // Fallback: SeaORM's sql_err() may fail to classify the violation when
    // the error is wrapped by a connection proxy or driver layer. Use the
    // robust string-based detector from toolkit-db.
    if toolkit_db::secure::is_unique_violation(db_err) {
        return DomainError::Conflict {
            code: "unique_violation".into(),
            message: db_err.to_string(),
        };
    }
    DomainError::database(db_err.to_string())
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::DomainError;

    #[test]
    fn pdp_evaluation_failure_is_authz_unavailable() {
        let e = authz_resolver_sdk::EnforcerError::EvaluationFailed(
            toolkit_canonical_errors::CanonicalError::service_unavailable()
                .with_detail("authz-resolver unreachable")
                .create(),
        );
        assert!(matches!(
            DomainError::from(e),
            DomainError::AuthzUnavailable
        ));
    }
}
