// Created: 2026-07-10 by Constructor Tech
//! Transport-agnostic errors for the LLM Gateway.
//!
//! Returned by [`LlmGatewayClientV1`](super::api::LlmGatewayClientV1) methods.
//! Variants mirror the Open Responses error codes in `docs/DESIGN.md` §3.3; the
//! module implementation maps them to RFC 9457 Problem Details in the REST layer.

/// Errors returned by LLM Gateway SDK operations.
///
/// Each variant corresponds to a `code` in the Open Responses error contract
/// (`docs/DESIGN.md` §3.3).
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum LlmGatewayError {
    /// `model_not_found` — model not in catalog.
    #[error("model not found: {model}")]
    ModelNotFound { model: String },

    /// `model_not_approved` — model not approved for tenant.
    #[error("model not approved for tenant: {model}")]
    ModelNotApproved { model: String },

    /// `validation_error` — invalid request format. `param` names the offending
    /// field when known.
    #[error("validation error: {message}")]
    Validation {
        message: String,
        param: Option<String>,
    },

    /// `capability_not_supported` — model lacks a required capability (or an
    /// unsupported feature such as a non-null `previous_response_id`).
    #[error("capability not supported: {capability}")]
    CapabilityNotSupported { capability: String },

    /// `budget_exceeded` — tenant AI-credit budget exhausted.
    #[error("budget exceeded")]
    BudgetExceeded,

    /// `rate_limited` — rate limit exceeded.
    #[error("rate limited")]
    RateLimited,

    /// `request_blocked` — blocked by a pre-call hook plugin.
    #[error("request blocked: {reason}")]
    RequestBlocked { reason: String },

    /// `hook_timeout` — a hook plugin call timed out.
    #[error("hook timeout")]
    HookTimeout,

    /// `output_validation_error` — provider output does not conform to the
    /// requested JSON schema.
    #[error("output validation error: {message}")]
    OutputValidationError { message: String },

    /// `provider_error` — provider returned an error.
    #[error("provider error: {message}")]
    ProviderError { message: String },

    /// `provider_timeout` — provider request timed out.
    #[error("provider timeout")]
    ProviderTimeout,

    /// Catch-all for unexpected failures (DB/cache/OAGW/etc.). `detail` is a
    /// short human-readable summary; `source` carries the underlying error when
    /// available, accessible via `std::error::Error::source`.
    #[error("internal error: {detail}")]
    Internal {
        detail: String,
        #[source]
        source: Option<Box<dyn std::error::Error + Send + Sync + 'static>>,
    },
}

impl LlmGatewayError {
    #[must_use]
    pub fn model_not_found(model: impl Into<String>) -> Self {
        Self::ModelNotFound {
            model: model.into(),
        }
    }

    #[must_use]
    pub fn model_not_approved(model: impl Into<String>) -> Self {
        Self::ModelNotApproved {
            model: model.into(),
        }
    }

    #[must_use]
    pub fn validation(message: impl Into<String>) -> Self {
        Self::Validation {
            message: message.into(),
            param: None,
        }
    }

    #[must_use]
    pub fn validation_param(message: impl Into<String>, param: impl Into<String>) -> Self {
        Self::Validation {
            message: message.into(),
            param: Some(param.into()),
        }
    }

    #[must_use]
    pub fn capability_not_supported(capability: impl Into<String>) -> Self {
        Self::CapabilityNotSupported {
            capability: capability.into(),
        }
    }

    #[must_use]
    pub fn request_blocked(reason: impl Into<String>) -> Self {
        Self::RequestBlocked {
            reason: reason.into(),
        }
    }

    #[must_use]
    pub fn output_validation_error(message: impl Into<String>) -> Self {
        Self::OutputValidationError {
            message: message.into(),
        }
    }

    #[must_use]
    pub fn provider_error(message: impl Into<String>) -> Self {
        Self::ProviderError {
            message: message.into(),
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
        let err = LlmGatewayError::internal("oagw pool exhausted");
        assert_eq!(err.to_string(), "internal error: oagw pool exhausted");
        assert!(std::error::Error::source(&err).is_none());
    }

    #[test]
    fn internal_from_preserves_source_chain() {
        let upstream = std::io::Error::new(std::io::ErrorKind::ConnectionReset, "rst");
        let err = LlmGatewayError::internal_from("provider call failed", upstream);
        assert_eq!(err.to_string(), "internal error: provider call failed");
        let source = std::error::Error::source(&err).expect("source preserved");
        assert!(source.to_string().contains("rst"));
    }
}
