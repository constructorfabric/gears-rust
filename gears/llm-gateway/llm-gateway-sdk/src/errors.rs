// Created: 2026-07-10 by Constructor Tech
//! Transport-agnostic errors for the LLM Gateway.
//!
//! Returned by [`LlmGatewayClientV1`](super::api::LlmGatewayClientV1) and
//! [`LlmGatewayProviderPluginClientV1`](super::plugin_api::LlmGatewayProviderPluginClientV1)
//! methods. Each variant has a stable wire `code` ([`LlmGatewayError::code`])
//! that consumers branch on; the gear maps errors to RFC 9457 Problem Details
//! at REST and to the `error` of the terminal `response.failed` event in
//! streams ([`LlmGatewayError::to_response_error`]).

use crate::models::core::ResponseError;

/// Normalized category of a provider error
/// (`cpt-cf-llm-gateway-fr-provider-error-normalization-v1`).
///
/// Provider plugins map every provider error to one of these categories, so
/// consumers can branch on the category regardless of provider.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum ProviderErrorCategory {
    /// Input exceeds the model context window; trim history and retry.
    ContextLengthExceeded,
    /// Provider rate limit hit; see `retry_after` when the provider supplied it.
    RateLimited,
    /// Request or response blocked by the provider's content policy.
    ContentFiltered,
    /// Request rejected by the provider as malformed or unsupported.
    InvalidRequest,
    /// Provider rejected the API key.
    AuthenticationFailed,
    /// Provider outage, overload, server error, or network failure; the
    /// request may succeed on retry.
    ProviderUnavailable,
    /// Provider error that matches no other category.
    ProviderError,
}

impl ProviderErrorCategory {
    /// Wire code of the category.
    #[must_use]
    pub fn code(self) -> &'static str {
        match self {
            Self::ContextLengthExceeded => "context_length_exceeded",
            Self::RateLimited => "rate_limited",
            Self::ContentFiltered => "content_filtered",
            Self::InvalidRequest => "invalid_request",
            Self::AuthenticationFailed => "authentication_failed",
            Self::ProviderUnavailable => "provider_unavailable",
            Self::ProviderError => "provider_error",
        }
    }
}

/// A normalized provider error with the provider's originals.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ProviderFailure {
    /// The provider's original error code, when available.
    pub provider_code: Option<String>,
    /// The provider's original error message, when available.
    pub provider_message: Option<String>,
    /// Request identifier assigned by the provider, when available.
    pub provider_request_id: Option<String>,
    /// Seconds to wait before retrying, when the provider supplied it.
    pub retry_after: Option<u64>,
}

/// Which timeout expired (`cpt-cf-llm-gateway-fr-timeout-v1`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimeoutKind {
    /// No first response chunk within the time-to-first-token limit.
    TimeToFirstToken,
    /// Response not complete within the total generation limit.
    Total,
}

/// Errors returned by LLM Gateway SDK operations.
///
/// Each variant corresponds to one wire `code` ([`LlmGatewayError::code`]).
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum LlmGatewayError {
    /// `validation_error` — the request is invalid (empty input, `tool_choice`
    /// naming an undefined function, provider tool prefix that does not match
    /// the model's provider), or structured output does not match the
    /// requested schema. `param` names the offending field when known.
    #[error("validation error: {message}")]
    Validation {
        message: String,
        param: Option<String>,
    },

    /// `model_not_found` — the model is unknown or not available for the
    /// tenant (the two cases are not distinguished).
    #[error("model not found: {model}")]
    ModelNotFound { model: String },

    /// `model_resolution_unavailable` — Model Registry is unavailable.
    #[error("model resolution unavailable")]
    ModelResolutionUnavailable,

    /// `provider_not_supported` — no provider plugin is registered for the
    /// model's provider type.
    #[error("provider not supported: {provider_type}")]
    ProviderNotSupported { provider_type: String },

    /// `capability_not_supported` — the model or the provider integration
    /// lacks a capability the request needs (streaming, image or file input,
    /// tool calling, web search, a reasoning option, structured output,
    /// embeddings).
    #[error("capability not supported: {capability}")]
    CapabilityNotSupported { capability: String },

    /// `unsupported_parameter` — a generic parameter the model does not
    /// support, and `drop_unsupported_params` is not set (or the parameter is
    /// `dimensions`, which is never dropped).
    #[error("unsupported parameter: {param}")]
    UnsupportedParameter { param: String },

    /// `unsupported_schema` — the structured output schema cannot be expressed
    /// in the provider's JSON Schema subset; `construct` names the construct.
    #[error("unsupported schema construct: {construct}")]
    UnsupportedSchema { construct: String },

    /// `schema_not_found` — a GTS tool schema reference cannot be resolved.
    #[error("schema not found: {schema_id}")]
    SchemaNotFound { schema_id: String },

    /// `media_not_found` — an image or document input cannot be retrieved or
    /// decoded.
    #[error("media not found: {message}")]
    MediaNotFound { message: String },

    /// `credentials_unavailable` — the provider API key referenced by the
    /// model cannot be fetched from `CredStore`.
    #[error("credentials unavailable")]
    CredentialsUnavailable,

    /// `unapproved_destination` — the model has a secret reference and its
    /// `base_url` is not an approved destination for the tenant or does not use
    /// HTTPS; the key is neither fetched nor sent.
    #[error("unapproved destination")]
    UnapprovedDestination,

    /// `quota_exceeded` — the tenant's AI credit quota is exhausted.
    #[error("quota exceeded")]
    QuotaExceeded,

    /// `quota_check_unavailable` — Quota Enforcement is unavailable; the
    /// quota check is never bypassed.
    #[error("quota check unavailable")]
    QuotaCheckUnavailable,

    /// `timeout` — a Gateway-enforced timeout expired.
    #[error("timeout: {kind:?}")]
    Timeout { kind: TimeoutKind },

    /// A provider error, normalized to a [`ProviderErrorCategory`]; the wire
    /// `code` is the category's code.
    #[error("{}: {message}", category.code())]
    Provider {
        category: ProviderErrorCategory,
        message: String,
        details: ProviderFailure,
    },

    /// `internal` — unexpected failure. `detail` is a short human-readable
    /// summary; `source` carries the underlying error when available,
    /// accessible via `std::error::Error::source`.
    #[error("internal error: {detail}")]
    Internal {
        detail: String,
        #[source]
        source: Option<Box<dyn std::error::Error + Send + Sync + 'static>>,
    },
}

impl LlmGatewayError {
    /// Stable wire code of the error.
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            Self::Validation { .. } => "validation_error",
            Self::ModelNotFound { .. } => "model_not_found",
            Self::ModelResolutionUnavailable => "model_resolution_unavailable",
            Self::ProviderNotSupported { .. } => "provider_not_supported",
            Self::CapabilityNotSupported { .. } => "capability_not_supported",
            Self::UnsupportedParameter { .. } => "unsupported_parameter",
            Self::UnsupportedSchema { .. } => "unsupported_schema",
            Self::SchemaNotFound { .. } => "schema_not_found",
            Self::MediaNotFound { .. } => "media_not_found",
            Self::CredentialsUnavailable => "credentials_unavailable",
            Self::UnapprovedDestination => "unapproved_destination",
            Self::QuotaExceeded => "quota_exceeded",
            Self::QuotaCheckUnavailable => "quota_check_unavailable",
            Self::Timeout { .. } => "timeout",
            Self::Provider { category, .. } => category.code(),
            Self::Internal { .. } => "internal",
        }
    }

    /// Build the `error` object of a terminal `response.failed` event, with
    /// the same code as the error and the provider originals when present.
    /// `Internal` errors expose only a generic message.
    #[must_use]
    pub fn to_response_error(&self) -> ResponseError {
        let message = match self {
            Self::Internal { .. } => "internal error".to_owned(),
            other => other.to_string(),
        };
        let mut error = ResponseError {
            code: self.code().to_owned(),
            message,
            ..ResponseError::default()
        };
        match self {
            Self::Validation { param, .. } => error.param.clone_from(param),
            Self::UnsupportedParameter { param } => error.param = Some(param.clone()),
            Self::Provider { details, .. } => {
                error.provider_code.clone_from(&details.provider_code);
                error.provider_message.clone_from(&details.provider_message);
                error
                    .provider_request_id
                    .clone_from(&details.provider_request_id);
                error.retry_after = details.retry_after;
            }
            _ => {}
        }
        error
    }

    #[must_use]
    pub fn model_not_found(model: impl Into<String>) -> Self {
        Self::ModelNotFound {
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
    pub fn unsupported_parameter(param: impl Into<String>) -> Self {
        Self::UnsupportedParameter {
            param: param.into(),
        }
    }

    #[must_use]
    pub fn unsupported_schema(construct: impl Into<String>) -> Self {
        Self::UnsupportedSchema {
            construct: construct.into(),
        }
    }

    /// Construct a normalized provider error.
    #[must_use]
    pub fn provider(
        category: ProviderErrorCategory,
        message: impl Into<String>,
        details: ProviderFailure,
    ) -> Self {
        Self::Provider {
            category,
            message: message.into(),
            details,
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

    #[test]
    fn provider_error_code_is_category_code() {
        let err = LlmGatewayError::provider(
            ProviderErrorCategory::ContextLengthExceeded,
            "too long",
            ProviderFailure::default(),
        );
        assert_eq!(err.code(), "context_length_exceeded");
        assert_eq!(err.to_string(), "context_length_exceeded: too long");
    }

    #[test]
    fn category_serde_matches_code() {
        for category in [
            ProviderErrorCategory::ContextLengthExceeded,
            ProviderErrorCategory::RateLimited,
            ProviderErrorCategory::ContentFiltered,
            ProviderErrorCategory::InvalidRequest,
            ProviderErrorCategory::AuthenticationFailed,
            ProviderErrorCategory::ProviderUnavailable,
            ProviderErrorCategory::ProviderError,
        ] {
            assert_eq!(
                serde_json::to_value(category).unwrap(),
                serde_json::Value::from(category.code())
            );
        }
    }

    #[test]
    fn response_error_carries_provider_originals() {
        let err = LlmGatewayError::provider(
            ProviderErrorCategory::RateLimited,
            "slow down",
            ProviderFailure {
                provider_code: Some("rate_limit_error".into()),
                provider_message: Some("Too many requests".into()),
                provider_request_id: Some("req_1".into()),
                retry_after: Some(30),
            },
        );
        let wire = err.to_response_error();
        assert_eq!(wire.code, "rate_limited");
        assert_eq!(wire.provider_code.as_deref(), Some("rate_limit_error"));
        assert_eq!(wire.provider_message.as_deref(), Some("Too many requests"));
        assert_eq!(wire.provider_request_id.as_deref(), Some("req_1"));
        assert_eq!(wire.retry_after, Some(30));
    }

    #[test]
    fn response_error_hides_internal_detail() {
        let wire = LlmGatewayError::internal("db password rejected").to_response_error();
        assert_eq!(wire.code, "internal");
        assert_eq!(wire.message, "internal error");
    }

    #[test]
    fn unsupported_parameter_names_param() {
        let wire = LlmGatewayError::unsupported_parameter("temperature").to_response_error();
        assert_eq!(wire.code, "unsupported_parameter");
        assert_eq!(wire.param.as_deref(), Some("temperature"));
    }
}
