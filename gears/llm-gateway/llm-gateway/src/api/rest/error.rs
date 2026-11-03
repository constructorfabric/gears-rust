//! `LlmGatewayError` to RFC 9457 Problem mapping (DESIGN §4.1).

use llm_gateway_sdk::{LlmGatewayError, ProviderErrorCategory};
use toolkit_canonical_errors::{Problem, ProblemCategory};

/// Problem `error_domain` of every gateway Problem.
pub const ERROR_DOMAIN: &str = "llm_gateway.v1";

/// Map `err` to a Problem. The canonical category, status and title follow
/// the DESIGN §4.1 table; `error_code` is the Open Responses `code`;
/// `context.data` carries `code`, the response id (`response_id`) when
/// known, `param`, and provider details. `Internal` errors expose only a
/// generic message.
#[must_use]
pub fn error_to_problem(err: &LlmGatewayError, response_id: Option<&str>) -> Problem {
    // The wire error object is shared with the SSE `response.failed` event:
    // it already redacts `Internal` and truncates provider text.
    let wire = err.to_response_error();

    let mut data = serde_json::Map::new();
    data.insert("code".to_owned(), wire.code.clone().into());
    if let Some(id) = response_id {
        data.insert("response_id".to_owned(), id.into());
    }
    if let Some(param) = wire.param {
        data.insert("param".to_owned(), param.into());
    }
    if let Some(value) = wire.provider_code {
        data.insert("provider_code".to_owned(), value.into());
    }
    if let Some(value) = wire.provider_message {
        data.insert("provider_message".to_owned(), value.into());
    }
    if let Some(value) = wire.provider_request_id {
        data.insert("provider_request_id".to_owned(), value.into());
    }
    if let Some(value) = wire.retry_after {
        data.insert("retry_after".to_owned(), value.into());
    }

    Problem::contract_error(
        category(err),
        wire.code,
        ERROR_DOMAIN,
        wire.message,
        serde_json::Value::Object(data),
    )
}

/// Canonical category of `err` (DESIGN §4.1 table).
fn category(err: &LlmGatewayError) -> ProblemCategory {
    match err {
        LlmGatewayError::Validation { .. }
        | LlmGatewayError::UnsupportedParameter { .. }
        | LlmGatewayError::UnsupportedSchema { .. }
        | LlmGatewayError::MediaNotFound { .. } => ProblemCategory::InvalidArgument,
        LlmGatewayError::OutputValidation { .. } => ProblemCategory::Aborted,
        LlmGatewayError::ModelNotFound { .. } | LlmGatewayError::SchemaNotFound { .. } => {
            ProblemCategory::NotFound
        }
        LlmGatewayError::ModelResolutionUnavailable
        | LlmGatewayError::CredentialsUnavailable
        | LlmGatewayError::QuotaCheckUnavailable
        | LlmGatewayError::GatewayUnavailable => ProblemCategory::ServiceUnavailable,
        LlmGatewayError::ProviderNotSupported { .. } => ProblemCategory::Unimplemented,
        LlmGatewayError::CapabilityNotSupported { .. } | LlmGatewayError::UnapprovedDestination => {
            ProblemCategory::FailedPrecondition
        }
        LlmGatewayError::QuotaExceeded => ProblemCategory::ResourceExhausted,
        LlmGatewayError::PermissionDenied => ProblemCategory::PermissionDenied,
        LlmGatewayError::Provider { category, .. } => provider_category(*category),
        // `Internal` and any future variant: generic 500.
        _ => ProblemCategory::Internal,
    }
}

fn provider_category(category: ProviderErrorCategory) -> ProblemCategory {
    match category {
        ProviderErrorCategory::ContextLengthExceeded
        | ProviderErrorCategory::ContentFiltered
        | ProviderErrorCategory::InvalidRequest => ProblemCategory::InvalidArgument,
        ProviderErrorCategory::RateLimited => ProblemCategory::ResourceExhausted,
        // The consumer's own authentication is not at fault: never 401.
        ProviderErrorCategory::AuthenticationFailed => ProblemCategory::FailedPrecondition,
        ProviderErrorCategory::ProviderUnavailable => ProblemCategory::ServiceUnavailable,
        _ => ProblemCategory::Unknown,
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "error_tests.rs"]
mod error_tests;
