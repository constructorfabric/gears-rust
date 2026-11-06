//! `LlmGatewayError` to Problem mapping (DESIGN §4.1 table and rules).

use llm_gateway_sdk::{
    LlmGatewayError, MAX_PROVIDER_TEXT_CHARS, ProviderErrorCategory, ProviderFailure,
};

use super::{ERROR_DOMAIN, error_to_problem};

fn provider(category: ProviderErrorCategory) -> LlmGatewayError {
    LlmGatewayError::provider(category, "provider said no", ProviderFailure::default())
}

/// One row per `LlmGatewayError` variant: error, wire code, canonical
/// category (GTS fragment name), HTTP status.
fn table() -> Vec<(LlmGatewayError, &'static str, &'static str, u16)> {
    vec![
        (
            LlmGatewayError::validation("bad"),
            "validation_error",
            "invalid_argument",
            400,
        ),
        (
            LlmGatewayError::output_validation("mismatch"),
            "output_validation_error",
            "aborted",
            409,
        ),
        (
            LlmGatewayError::model_not_found("m"),
            "model_not_found",
            "not_found",
            404,
        ),
        (
            LlmGatewayError::ModelResolutionUnavailable,
            "model_resolution_unavailable",
            "service_unavailable",
            503,
        ),
        (
            LlmGatewayError::provider_not_supported("p"),
            "provider_not_supported",
            "unimplemented",
            501,
        ),
        (
            LlmGatewayError::capability_not_supported("c"),
            "capability_not_supported",
            "failed_precondition",
            400,
        ),
        (
            LlmGatewayError::unsupported_parameter("p"),
            "unsupported_parameter",
            "invalid_argument",
            400,
        ),
        (
            LlmGatewayError::unsupported_schema("c"),
            "unsupported_schema",
            "invalid_argument",
            400,
        ),
        (
            LlmGatewayError::schema_not_found("s"),
            "schema_not_found",
            "not_found",
            404,
        ),
        (
            LlmGatewayError::media_not_found(),
            "media_not_found",
            "invalid_argument",
            400,
        ),
        (
            LlmGatewayError::CredentialsUnavailable,
            "credentials_unavailable",
            "service_unavailable",
            503,
        ),
        (
            LlmGatewayError::UnapprovedDestination,
            "unapproved_destination",
            "failed_precondition",
            400,
        ),
        (
            LlmGatewayError::QuotaExceeded,
            "quota_exceeded",
            "resource_exhausted",
            429,
        ),
        (
            LlmGatewayError::QuotaCheckUnavailable,
            "quota_check_unavailable",
            "service_unavailable",
            503,
        ),
        (
            LlmGatewayError::PermissionDenied,
            "permission_denied",
            "permission_denied",
            403,
        ),
        (
            LlmGatewayError::GatewayUnavailable,
            "gateway_unavailable",
            "service_unavailable",
            503,
        ),
        (
            provider(ProviderErrorCategory::ContextLengthExceeded),
            "context_length_exceeded",
            "invalid_argument",
            400,
        ),
        (
            provider(ProviderErrorCategory::RateLimited),
            "rate_limited",
            "resource_exhausted",
            429,
        ),
        (
            provider(ProviderErrorCategory::ContentFiltered),
            "content_filtered",
            "invalid_argument",
            400,
        ),
        (
            provider(ProviderErrorCategory::InvalidRequest),
            "invalid_request",
            "invalid_argument",
            400,
        ),
        (
            provider(ProviderErrorCategory::AuthenticationFailed),
            "authentication_failed",
            "failed_precondition",
            400,
        ),
        (
            provider(ProviderErrorCategory::ProviderUnavailable),
            "provider_unavailable",
            "service_unavailable",
            503,
        ),
        (
            provider(ProviderErrorCategory::ProviderError),
            "provider_error",
            "unknown",
            500,
        ),
        (
            LlmGatewayError::internal("boom"),
            "internal",
            "internal",
            500,
        ),
    ]
}

#[test]
fn every_variant_maps_to_its_documented_status_category_and_code() {
    for (err, code, category, status) in table() {
        let problem = error_to_problem(&err, Some("resp_1"));
        assert_eq!(problem.status, Some(status), "status of {code}");
        assert_eq!(problem.error_code.as_deref(), Some(code), "code of {code}");
        assert_eq!(problem.error_domain.as_deref(), Some(ERROR_DOMAIN));
        assert!(
            problem
                .problem_type
                .contains(&format!("cf.core.err.{category}.v1")),
            "type of {code} is {} but expected category {category}",
            problem.problem_type
        );
        assert_eq!(err.code(), code, "table row uses the SDK wire code");
    }
}

#[test]
fn provider_authentication_failure_is_never_401() {
    let problem = error_to_problem(&provider(ProviderErrorCategory::AuthenticationFailed), None);
    assert_eq!(problem.status, Some(400));
    assert_ne!(problem.status, Some(401));
}

#[test]
fn problem_carries_the_response_id_when_known() {
    let problem = error_to_problem(&LlmGatewayError::model_not_found("m"), Some("resp_42"));
    assert_eq!(problem.context["data"]["response_id"], "resp_42");
    assert_eq!(problem.context["data"]["code"], "model_not_found");
}

#[test]
fn problem_omits_the_response_id_when_not_allocated() {
    let problem = error_to_problem(&LlmGatewayError::PermissionDenied, None);
    assert!(problem.context["data"].get("response_id").is_none());
}

#[test]
fn problem_carries_param_when_known() {
    let problem = error_to_problem(&LlmGatewayError::validation_param("bad", "input"), None);
    assert_eq!(problem.context["data"]["param"], "input");
}

#[test]
fn provider_details_and_retry_after_are_carried() {
    let err = LlmGatewayError::provider(
        ProviderErrorCategory::RateLimited,
        "slow down",
        ProviderFailure::default()
            .with_provider_code("rate_limit_exceeded")
            .with_provider_request_id("req_9")
            .with_retry_after(7),
    );
    let problem = error_to_problem(&err, Some("resp_1"));
    let data = &problem.context["data"];
    assert_eq!(data["provider_code"], "rate_limit_exceeded");
    assert_eq!(data["provider_request_id"], "req_9");
    assert_eq!(data["retry_after"], 7);
}

#[test]
fn provider_text_is_truncated_like_the_stream_error() {
    let long = "x".repeat(MAX_PROVIDER_TEXT_CHARS * 4);
    let err = LlmGatewayError::provider(
        ProviderErrorCategory::ProviderError,
        long.clone(),
        ProviderFailure::default().with_provider_message(long),
    );
    let problem = error_to_problem(&err, None);
    let message = problem.context["data"]["provider_message"]
        .as_str()
        .expect("provider_message present");
    assert!(message.chars().count() <= MAX_PROVIDER_TEXT_CHARS + 1);
    assert!(problem.detail.chars().count() <= MAX_PROVIDER_TEXT_CHARS + 1);
}

#[test]
fn internal_errors_expose_only_a_generic_message() {
    let err = LlmGatewayError::internal_from(
        "db password hunter2 rejected",
        std::io::Error::other("connection string postgres://u:hunter2@db"),
    );
    let problem = error_to_problem(&err, Some("resp_1"));
    let wire = serde_json::to_string(&problem).expect("problem serializes");

    assert_eq!(problem.status, Some(500));
    assert!(!wire.contains("hunter2"), "internal detail leaked: {wire}");
    assert!(wire.contains("resp_1"), "the response id still rides along");
}
