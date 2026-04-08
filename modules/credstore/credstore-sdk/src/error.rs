use thiserror::Error;

/// Errors that can occur during credential store operations.
#[derive(Debug, Error)]
pub enum CredStoreError {
    #[error("invalid secret reference: {reason}")]
    InvalidSecretRef { reason: String },

    #[error("secret not found")]
    NotFound,

    #[error("no plugin available")]
    NoPluginAvailable,

    #[error("service unavailable: {0}")]
    ServiceUnavailable(String),

    #[error("internal error: {0}")]
    Internal(String),
}

impl CredStoreError {
    #[must_use]
    pub fn invalid_ref(reason: impl Into<String>) -> Self {
        Self::InvalidSecretRef {
            reason: reason.into(),
        }
    }

    #[must_use]
    pub fn service_unavailable(msg: impl Into<String>) -> Self {
        Self::ServiceUnavailable(msg.into())
    }

    #[must_use]
    pub fn internal(msg: impl Into<String>) -> Self {
        Self::Internal(msg.into())
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    #[test]
    fn invalid_ref_constructor_sets_reason() {
        let e = CredStoreError::invalid_ref("must not be empty");
        assert_eq!(e.to_string(), "invalid secret reference: must not be empty");
    }

    #[test]
    fn service_unavailable_constructor_sets_message() {
        let e = CredStoreError::service_unavailable("backend down");
        assert!(matches!(e, CredStoreError::ServiceUnavailable(ref m) if m == "backend down"));
        assert_eq!(e.to_string(), "service unavailable: backend down");
    }

    #[test]
    fn internal_constructor_sets_message() {
        let e = CredStoreError::internal("unexpected state");
        assert!(matches!(e, CredStoreError::Internal(ref m) if m == "unexpected state"));
        assert_eq!(e.to_string(), "internal error: unexpected state");
    }
}
