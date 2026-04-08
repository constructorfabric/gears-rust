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

    /// The plugin emit future exceeded its deadline; treated as transient so
    /// the outbox will retry the message on the next cycle.
    #[error("audit plugin timed out")]
    PluginTimeout,
}

impl MiniChatAuditPluginError {
    #[must_use]
    pub fn is_transient(&self) -> bool {
        matches!(self, Self::Transient(_) | Self::PluginTimeout)
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
