#![cfg(feature = "axum")]

use axum::response::IntoResponse;
use toolkit_canonical_errors::problem::APPLICATION_PROBLEM_JSON;
use toolkit_canonical_errors::resource_error;
use toolkit_canonical_errors::{CanonicalError, Http, Problem};

#[resource_error(gts_id!("cf.core.test.axum.v1~"))]
struct AxumTestR;

#[test]
fn problem_into_response_sets_status_and_content_type() {
    let err = AxumTestR::not_found("missing")
        .with_resource("abc")
        .create();
    let response = Problem::from(err).into_response();

    assert_eq!(response.status(), http::StatusCode::NOT_FOUND);
    assert_eq!(
        response
            .headers()
            .get(http::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok()),
        Some(APPLICATION_PROBLEM_JSON)
    );
}

#[test]
fn problem_into_response_sets_retry_after_header() {
    let problem = Problem::from(
        CanonicalError::service_unavailable()
            .with_retry_after_seconds(30)
            .create(),
    );

    let response = problem.into_response();

    assert_eq!(response.status(), http::StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        response.headers().get(http::header::RETRY_AFTER),
        Some(&http::HeaderValue::from_static("30"))
    );
}

#[test]
fn canonical_error_into_response_sets_retry_after_header() {
    let err = CanonicalError::service_unavailable()
        .with_retry_after_seconds(30)
        .create();

    let response = err.into_response();

    assert_eq!(
        response.headers().get(http::header::RETRY_AFTER),
        Some(&http::HeaderValue::from_static("30"))
    );
}

#[test]
fn service_unavailable_without_retry_hint_omits_retry_after_header() {
    let response = CanonicalError::service_unavailable()
        .create()
        .into_response();

    assert!(response.headers().get(http::header::RETRY_AFTER).is_none());
}

#[test]
fn service_unavailable_zero_retry_hint_emits_zero_header() {
    let response = CanonicalError::service_unavailable()
        .with_retry_after_seconds(0)
        .create()
        .into_response();

    assert_eq!(
        response.headers().get(http::header::RETRY_AFTER),
        Some(&http::HeaderValue::from_static("0"))
    );
}

#[test]
fn another_category_with_retry_field_omits_retry_after_header() {
    let mut problem = Problem::from(
        AxumTestR::not_found("missing")
            .with_resource("abc")
            .create(),
    );
    problem.context["retry_after_seconds"] = serde_json::json!(30);

    let response = problem.into_response();

    assert!(response.headers().get(http::header::RETRY_AFTER).is_none());
}

#[test]
fn malformed_service_unavailable_context_omits_retry_after_header() {
    let mut problem = Problem::from(
        CanonicalError::service_unavailable()
            .with_retry_after_seconds(30)
            .create(),
    );
    problem.context = serde_json::json!({ "retry_after_seconds": "soon" });

    let response = problem.into_response();

    assert_eq!(response.status(), http::StatusCode::SERVICE_UNAVAILABLE);
    assert!(response.headers().get(http::header::RETRY_AFTER).is_none());
}

#[test]
fn service_unavailable_status_override_keeps_retry_after_header() {
    let response = CanonicalError::service_unavailable()
        .with_retry_after_seconds(30)
        .with_override(Http::status_code(502))
        .create()
        .into_response();

    assert_eq!(response.status(), http::StatusCode::BAD_GATEWAY);
    assert_eq!(
        response.headers().get(http::header::RETRY_AFTER),
        Some(&http::HeaderValue::from_static("30"))
    );
}

#[test]
fn canonical_error_into_response_delegates_through_problem() {
    let err = CanonicalError::internal("db failure").create();
    let response = err.into_response();

    assert_eq!(response.status(), http::StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(
        response
            .headers()
            .get(http::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok()),
        Some(APPLICATION_PROBLEM_JSON)
    );
}

#[test]
fn invalid_argument_maps_to_400() {
    let err = AxumTestR::invalid_argument()
        .with_field_violation("name", "must not be empty", "REQUIRED")
        .create();
    let response = err.into_response();
    assert_eq!(response.status(), http::StatusCode::BAD_REQUEST);
}

#[test]
fn canonical_error_into_response_attaches_self_to_extensions() {
    // Middleware reads the `CanonicalError` extension to surface
    // `diagnostic()` server-side without leaking it on the wire.
    let err = CanonicalError::internal("db connection refused: secret-host:5432").create();
    let response = err.into_response();

    let recovered = response
        .extensions()
        .get::<CanonicalError>()
        .expect("CanonicalError must be attached to response extensions");
    assert_eq!(
        recovered.diagnostic(),
        Some("db connection refused: secret-host:5432")
    );
    assert_eq!(recovered.status_code(), 500);
}
