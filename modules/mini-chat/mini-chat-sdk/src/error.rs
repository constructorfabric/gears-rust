use thiserror::Error;

/// Errors returned by `MiniChatModelPolicyPluginClientV1` methods.
#[derive(Debug, Error)]
pub enum MiniChatModelPolicyPluginError {
    #[error("policy not found for the given tenant/version")]
    NotFound,

    #[error("internal policy plugin error: {0}")]
    Internal(String),
}

/// Errors returned by `MiniChatAuditPluginClientV1` methods.
///
/// Mirrors `PublishError` transient/permanent classification so callers can
/// decide whether to retry or record the failure as permanent.
#[derive(Debug, Error)]
pub enum MiniChatAuditPluginError {
    /// Transient failure — safe to retry (network timeout, broker unavailable).
    #[error("transient audit plugin error: {0}")]
    Transient(String),

    /// Permanent failure — do not retry (schema mismatch, auth rejected).
    #[error("permanent audit plugin error: {0}")]
    Permanent(String),
}

impl MiniChatAuditPluginError {
    #[must_use]
    pub fn is_transient(&self) -> bool {
        matches!(self, Self::Transient(_))
    }

    #[must_use]
    pub fn is_permanent(&self) -> bool {
        matches!(self, Self::Permanent(_))
    }
}

/// Errors returned by `publish_usage()`.
#[derive(Debug, Error)]
pub enum PublishError {
    /// Transient failure — safe to retry.
    #[error("transient publish error: {0}")]
    Transient(String),

    /// Permanent failure — do not retry.
    #[error("permanent publish error: {0}")]
    Permanent(String),
}

impl PublishError {
    #[must_use]
    pub fn is_transient(&self) -> bool {
        matches!(self, Self::Transient(_))
    }

    #[must_use]
    pub fn is_permanent(&self) -> bool {
        matches!(self, Self::Permanent(_))
    }
}

/// Errors exposed via the mini-chat SDK client.
#[derive(Debug, Error)]
pub enum MiniChatError {
    #[error("Chat not found: {id}")]
    ChatNotFound { id: uuid::Uuid },

    /// Attachment not found.
    #[error("Attachment not found: {id}")]
    AttachmentNotFound { id: uuid::Uuid },

    /// Attachment not in ready state.
    #[error("Attachment not ready: {id}")]
    AttachmentNotReady { id: uuid::Uuid },

    /// Access denied (authorization failure).
    #[error("Access denied")]
    Forbidden,

    /// File exceeds maximum upload size.
    #[error("File too large: max {max_bytes} bytes")]
    FileTooLarge { max_bytes: usize },

    /// Unsupported file content type.
    #[error("Unsupported file type: {content_type}")]
    UnsupportedFileType { content_type: String },

    /// Per-chat document limit exceeded.
    #[error("Document limit exceeded: max {max} documents per chat")]
    DocumentLimitExceeded { max: usize },

    /// Per-chat total upload size limit exceeded.
    #[error("Upload size limit exceeded: max {max_mb} MB per chat")]
    UploadSizeLimitExceeded { max_mb: usize },

    /// Upload quota exceeded.
    #[error("Upload quota exceeded")]
    UploadQuotaExceeded,

    /// An internal error occurred.
    #[error("Internal error")]
    Internal,
}
