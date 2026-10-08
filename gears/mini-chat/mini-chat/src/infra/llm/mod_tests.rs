// Created: 2026-04-07 by Constructor Tech
#![allow(clippy::str_to_string)]
use super::*;
use oagw_sdk::error::ServiceGatewayError;
use oagw_sdk::reason::auth::FailureReason as AuthFailureReason;

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
fn sanitize_removes_file_and_vector_store_ids() {
    let msg = "File file-4XkVvZt9pQ2rS8mN not found in vector store vs_67a1b2c3d4e5f6a7 \
               (assistant-Ab12Cd34Ef56Gh78, file_011CNha8iCJcU1wXNR6q4V8w)";
    let sanitized = sanitize_provider_message(msg);
    assert!(!sanitized.contains("file-4XkVvZt9pQ2rS8mN"));
    assert!(!sanitized.contains("file_011CNha8iCJcU1wXNR6q4V8w"));
    assert!(!sanitized.contains("vs_67a1b2c3d4e5f6a7"));
    assert!(!sanitized.contains("assistant-Ab12Cd34Ef56Gh78"));
}

#[test]
fn sanitize_keeps_ordinary_words() {
    let msg = "file-based upload failed; file_search disabled";
    assert_eq!(sanitize_provider_message(msg), msg);
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
    let err = ServiceGatewayError::RateLimited {
        retry_after_secs: None,
    };
    let mapped: LlmProviderError = err.into();
    assert!(matches!(
        mapped,
        LlmProviderError::RateLimited {
            retry_after_secs: None,
        },
    ));
}

#[test]
fn gateway_rate_limit_forwards_retry_hint() {
    let err = ServiceGatewayError::RateLimited {
        retry_after_secs: Some(15),
    };
    let mapped: LlmProviderError = err.into();
    assert!(matches!(
        mapped,
        LlmProviderError::RateLimited {
            retry_after_secs: Some(15),
        },
    ));
}

#[test]
fn gateway_timeout_maps_to_timeout() {
    let mapped: LlmProviderError = ServiceGatewayError::Timeout.into();
    assert!(matches!(mapped, LlmProviderError::Timeout));
}

#[test]
fn gateway_unavailable_maps_to_provider_unavailable() {
    let err = ServiceGatewayError::Unavailable {
        retry_after_secs: Some(5),
    };
    let mapped: LlmProviderError = err.into();
    assert!(matches!(mapped, LlmProviderError::ProviderUnavailable));
}

#[test]
fn gateway_auth_plugin_internal_maps_to_provider_unavailable() {
    // AUTH_PLUGIN_INTERNAL indicates the gateway-side auth machinery
    // itself failed (plugin panic, transport). Treat as transient
    // unavailability, not a hard auth rejection.
    let err = ServiceGatewayError::AuthFailed {
        reason: AuthFailureReason::PluginInternal,
        detail: "plugin crashed".into(),
    };
    let mapped: LlmProviderError = err.into();
    assert!(matches!(mapped, LlmProviderError::ProviderUnavailable));
}

#[test]
fn gateway_auth_plugin_failed_maps_to_provider_error() {
    // PluginFailed = creds rejected; user-facing failure, not transient.
    let err = ServiceGatewayError::AuthFailed {
        reason: AuthFailureReason::PluginFailed,
        detail: "bad token".into(),
    };
    let mapped: LlmProviderError = err.into();
    assert!(matches!(mapped, LlmProviderError::ProviderError { .. }));
}

#[test]
fn gateway_internal_maps_to_provider_error() {
    let err = ServiceGatewayError::Internal {
        detail: "resp_xyz789 failed at https://api.example.com".into(),
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

#[test]
fn provider_429_response_is_rate_limited() {
    let (parts, ()) = http::Response::builder()
        .status(http::StatusCode::TOO_MANY_REQUESTS)
        .header(http::header::RETRY_AFTER, "7")
        .body(())
        .unwrap()
        .into_parts();
    assert!(matches!(
        error_from_status(&parts),
        Some(LlmProviderError::RateLimited {
            retry_after_secs: Some(7)
        })
    ));

    let (parts, ()) = http::Response::builder()
        .status(http::StatusCode::BAD_GATEWAY)
        .body(())
        .unwrap()
        .into_parts();
    assert!(error_from_status(&parts).is_none());
}

#[test]
fn provider_429_without_numeric_retry_after_has_no_hint() {
    let (parts, ()) = http::Response::builder()
        .status(http::StatusCode::TOO_MANY_REQUESTS)
        .body(())
        .unwrap()
        .into_parts();
    assert!(matches!(
        error_from_status(&parts),
        Some(LlmProviderError::RateLimited {
            retry_after_secs: None
        })
    ));

    // The HTTP-date form of Retry-After is not parsed.
    let (parts, ()) = http::Response::builder()
        .status(http::StatusCode::TOO_MANY_REQUESTS)
        .header(http::header::RETRY_AFTER, "Wed, 21 Oct 2015 07:28:00 GMT")
        .body(())
        .unwrap()
        .into_parts();
    assert!(matches!(
        error_from_status(&parts),
        Some(LlmProviderError::RateLimited {
            retry_after_secs: None
        })
    ));
}

#[test]
fn gateway_504_problem_is_a_timeout_provider_504_is_not() {
    let (parts, ()) = http::Response::builder()
        .status(http::StatusCode::GATEWAY_TIMEOUT)
        .header(http::header::CONTENT_TYPE, "application/problem+json")
        .body(())
        .unwrap()
        .into_parts();
    assert!(matches!(
        error_from_status(&parts),
        Some(LlmProviderError::Timeout)
    ));

    // The provider's own 504 with a JSON error body is parsed as a provider error.
    let (parts, ()) = http::Response::builder()
        .status(http::StatusCode::GATEWAY_TIMEOUT)
        .header(http::header::CONTENT_TYPE, "application/json")
        .body(())
        .unwrap()
        .into_parts();
    assert!(error_from_status(&parts).is_none());
}
