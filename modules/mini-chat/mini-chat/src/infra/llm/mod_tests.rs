use super::*;
use oagw_sdk::error::ServiceGatewayError;

#[test]
fn sanitize_removes_provider_response_ids() {
    let msg = "Error in response resp_abc123xyz: rate limit exceeded";
    let sanitized = sanitize_provider_message(msg);
    assert!(!sanitized.contains("resp_abc123xyz"));
    assert!(sanitized.contains("[provider_id]"));
}

#[test]
fn sanitize_removes_urls() {
    let msg = "Error at https://api.openai.com/v1/responses: bad request";
    let sanitized = sanitize_provider_message(msg);
    assert!(!sanitized.contains("https://api.openai.com"));
    assert!(sanitized.contains("[url]"));
}

#[test]
fn sanitize_removes_credentials() {
    let msg = "Auth failed with sk-proj1234567890abcdef";
    let sanitized = sanitize_provider_message(msg);
    assert!(!sanitized.contains("sk-proj1234567890abcdef"));
    assert!(sanitized.contains("[credential]"));
}

#[test]
fn sanitize_mixed_content() {
    let msg = "resp_abc123 at https://api.openai.com with sk-test1234567890";
    let sanitized = sanitize_provider_message(msg);
    assert!(!sanitized.contains("resp_abc123"));
    assert!(!sanitized.contains("https://api.openai.com"));
    assert!(!sanitized.contains("sk-test1234567890"));
}

#[test]
fn raw_detail_preserves_original() {
    let err = LlmProviderError::ProviderError {
        code: "error".to_string(),
        message: "sanitized".to_string(),
        raw_detail: Some(RawDetail(
            "resp_abc123 at https://api.openai.com".to_string(),
        )),
    };
    assert_eq!(
        err.raw_detail(),
        Some("resp_abc123 at https://api.openai.com")
    );
}

#[test]
fn gateway_rate_limit_maps_to_rate_limited() {
    let err = ServiceGatewayError::RateLimitExceeded {
        detail: "too many requests".into(),
        instance: "/test".into(),
        retry_after_secs: Some(60),
    };
    let mapped: LlmProviderError = err.into();
    assert!(matches!(
        mapped,
        LlmProviderError::RateLimited {
            retry_after_secs: Some(60)
        }
    ));
}

#[test]
fn gateway_connection_timeout_maps_to_timeout() {
    let err = ServiceGatewayError::ConnectionTimeout {
        detail: "timed out".into(),
        instance: "/test".into(),
    };
    let mapped: LlmProviderError = err.into();
    assert!(matches!(mapped, LlmProviderError::Timeout));
}

#[test]
fn gateway_request_timeout_maps_to_timeout() {
    let err = ServiceGatewayError::RequestTimeout {
        detail: "timed out".into(),
        instance: "/test".into(),
    };
    let mapped: LlmProviderError = err.into();
    assert!(matches!(mapped, LlmProviderError::Timeout));
}

#[test]
fn gateway_upstream_disabled_maps_to_unavailable() {
    let err = ServiceGatewayError::UpstreamDisabled {
        detail: "disabled".into(),
        instance: "/test".into(),
    };
    let mapped: LlmProviderError = err.into();
    assert!(matches!(mapped, LlmProviderError::ProviderUnavailable));
}

#[test]
fn gateway_downstream_error_maps_to_provider_error() {
    let err = ServiceGatewayError::DownstreamError {
        detail: "resp_xyz789 failed at https://api.example.com".into(),
        instance: "/test".into(),
    };
    let mapped: LlmProviderError = err.into();
    match mapped {
        LlmProviderError::ProviderError {
            code,
            message,
            raw_detail,
        } => {
            assert_eq!(code, "gateway_error");
            assert!(!message.contains("resp_xyz789"));
            assert!(!message.contains("https://api.example.com"));
            assert!(raw_detail.is_some());
        }
        _ => panic!("expected ProviderError"),
    }
}
