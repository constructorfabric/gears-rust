// Created: 2026-04-17 by Constructor Tech
//! Transport-agnostic errors for the `model-registry` module.
//!
//! These errors are returned by [`ModelRegistryClientV1`](super::api::ModelRegistryClientV1)
//! methods. The module implementation maps these to RFC 9457 Problem Details
//! responses in the REST layer.

use uuid::Uuid;

/// Errors returned by Model Registry SDK operations.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ModelRegistryError {
    #[error("model not found: {canonical_id}")]
    ModelNotFound { canonical_id: String },

    #[error("model not approved for tenant: {canonical_id}")]
    ModelNotApproved { canonical_id: String },

    #[error("model deprecated: {canonical_id}")]
    ModelDeprecated { canonical_id: String },

    #[error("provider not found: {id}")]
    ProviderNotFound { id: Uuid },

    #[error("provider disabled: {id}")]
    ProviderDisabled { id: Uuid },

    #[error("invalid state transition: {detail}")]
    InvalidTransition { detail: String },

    #[error("validation error: {message}")]
    Validation { message: String },

    #[error("unauthorized: {detail}")]
    Unauthorized { detail: String },

    #[error("alias not found: {name}")]
    AliasNotFound { name: String },

    #[error("alias already exists: {name}")]
    AliasConflict { name: String },

    #[error("provider slug already exists: {slug}")]
    ProviderConflict { slug: String },

    #[error("discovery failed for provider {provider_id}: {detail}")]
    DiscoveryFailed { provider_id: Uuid, detail: String },

    /// Catch-all for unexpected failures (DB/cache/OAGW/etc.). `detail` is a
    /// short human-readable summary; `source` carries the underlying error
    /// when available, accessible via `std::error::Error::source`.
    #[error("internal error: {detail}")]
    Internal {
        detail: String,
        #[source]
        source: Option<Box<dyn std::error::Error + Send + Sync + 'static>>,
    },
}

impl ModelRegistryError {
    #[must_use]
    pub fn model_not_found(canonical_id: impl Into<String>) -> Self {
        Self::ModelNotFound {
            canonical_id: canonical_id.into(),
        }
    }

    #[must_use]
    pub fn model_not_approved(canonical_id: impl Into<String>) -> Self {
        Self::ModelNotApproved {
            canonical_id: canonical_id.into(),
        }
    }

    #[must_use]
    pub fn model_deprecated(canonical_id: impl Into<String>) -> Self {
        Self::ModelDeprecated {
            canonical_id: canonical_id.into(),
        }
    }

    #[must_use]
    pub fn provider_not_found(id: Uuid) -> Self {
        Self::ProviderNotFound { id }
    }

    #[must_use]
    pub fn provider_disabled(id: Uuid) -> Self {
        Self::ProviderDisabled { id }
    }

    #[must_use]
    pub fn invalid_transition(detail: impl Into<String>) -> Self {
        Self::InvalidTransition {
            detail: detail.into(),
        }
    }

    #[must_use]
    pub fn validation(message: impl Into<String>) -> Self {
        Self::Validation {
            message: message.into(),
        }
    }

    #[must_use]
    pub fn unauthorized(detail: impl Into<String>) -> Self {
        Self::Unauthorized {
            detail: detail.into(),
        }
    }

    #[must_use]
    pub fn alias_not_found(name: impl Into<String>) -> Self {
        Self::AliasNotFound { name: name.into() }
    }

    #[must_use]
    pub fn alias_conflict(name: impl Into<String>) -> Self {
        Self::AliasConflict { name: name.into() }
    }

    #[must_use]
    pub fn provider_conflict(slug: impl Into<String>) -> Self {
        Self::ProviderConflict { slug: slug.into() }
    }

    #[must_use]
    pub fn discovery_failed(provider_id: Uuid, detail: impl Into<String>) -> Self {
        Self::DiscoveryFailed {
            provider_id,
            detail: detail.into(),
        }
    }

    /// Construct an `Internal` error with a free-form detail string and no
    /// source chain.
    #[must_use]
    pub fn internal(detail: impl Into<String>) -> Self {
        Self::Internal {
            detail: detail.into(),
            source: None,
        }
    }

    /// Construct an `Internal` error wrapping an upstream error as the
    /// `#[source]` of this variant.
    pub fn internal_from(
        detail: impl Into<String>,
        source: impl std::error::Error + Send + Sync + 'static,
    ) -> Self {
        Self::Internal {
            detail: detail.into(),
            source: Some(Box::new(source)),
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn internal_without_source_has_none() {
        let err = ModelRegistryError::internal("db pool exhausted");
        assert_eq!(err.to_string(), "internal error: db pool exhausted");
        assert!(std::error::Error::source(&err).is_none());
    }

    #[test]
    fn internal_from_preserves_source_chain() {
        let upstream = std::io::Error::new(std::io::ErrorKind::ConnectionReset, "rst");
        let err = ModelRegistryError::internal_from("oagw call failed", upstream);
        assert_eq!(err.to_string(), "internal error: oagw call failed");
        let source = std::error::Error::source(&err).expect("source preserved");
        assert!(source.to_string().contains("rst"));
    }
}
