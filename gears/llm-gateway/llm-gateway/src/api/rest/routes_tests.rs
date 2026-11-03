//! `POST /llm-gateway/v1/responses`: registration (DESIGN §3.3) and dispatch.

use axum::Router;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use llm_gateway_sdk::{LlmGatewayError, ProviderErrorCategory, ProviderFailure, ResponseResource};
use toolkit::api::openapi_registry::OpenApiRegistryImpl;
use toolkit::api::operation_builder::OperationSpec;
use tower::ServiceExt as _;

use super::register_routes;
use crate::domain::test_support::{FakePlugin, Harness, PdpMode, ctx};

const PATH: &str = "/llm-gateway/v1/responses";

fn router_for(h: &Harness) -> (OpenApiRegistryImpl, Router) {
    let registry = OpenApiRegistryImpl::new();
    let router = register_routes(Router::new(), &registry, h.service());
    (registry, router)
}

fn post(json: &serde_json::Value) -> Request<Body> {
    Request::post(PATH)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(json.to_string()))
        .expect("request builds")
}

async fn send(router: Router, request: Request<Body>) -> (StatusCode, String, Vec<u8>) {
    let response = router
        .layer(axum::Extension(ctx()))
        .oneshot(request)
        .await
        .expect("Router's error type is Infallible");
    let status = response.status();
    let content_type = response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_owned();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body collects")
        .to_vec();
    (status, content_type, bytes)
}

fn spec(registry: &OpenApiRegistryImpl) -> OperationSpec {
    registry
        .operation_specs
        .get(&format!("{}:{PATH}", Method::POST.as_str()))
        .expect("POST /llm-gateway/v1/responses is registered")
        .value()
        .clone()
}

#[test]
fn create_response_is_registered_with_operation_id_and_authentication() {
    let (registry, _router) = router_for(&Harness::new());
    let spec = spec(&registry);

    assert_eq!(
        spec.operation_id.as_deref(),
        Some("llm_gateway.responses.create")
    );
    assert_eq!(spec.auth, toolkit::api::RouteAuth::Authenticated);
}

#[test]
fn create_response_declares_the_gateway_problem_responses() {
    let (registry, _router) = router_for(&Harness::new());
    let spec = spec(&registry);

    for status in [400u16, 403, 404, 409, 429, 500, 501, 503] {
        assert!(
            spec.responses
                .iter()
                .any(|r| r.status == status && r.content_type == "application/problem+json"),
            "operation MUST declare problem response {status}"
        );
    }
}

#[tokio::test]
async fn non_streaming_request_returns_200_json_response_resource() {
    let h = Harness::new();
    let (_registry, router) = router_for(&h);
    let (status, content_type, bytes) = send(
        router,
        post(&serde_json::json!({ "model": "openai::gpt-4o", "input": "hi", "stream": false })),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert!(
        content_type.starts_with("application/json"),
        "{content_type}"
    );
    let resource: ResponseResource = serde_json::from_slice(&bytes).expect("ResponseResource body");
    assert_eq!(resource.id, h.plugin.contexts()[0].request_id);
}

#[tokio::test]
async fn domain_error_is_a_problem_json_with_mapped_status_and_code() {
    let mut h = Harness::new();
    h.pdp = crate::domain::test_support::FakePdp::new(PdpMode::Deny, h.log.clone());
    let (_registry, router) = router_for(&h);
    let (status, content_type, bytes) = send(
        router,
        post(&serde_json::json!({ "model": "openai::gpt-4o", "input": "hi", "stream": false })),
    )
    .await;

    assert_eq!(status, StatusCode::FORBIDDEN);
    assert!(content_type.contains("problem+json"), "{content_type}");
    let body: serde_json::Value = serde_json::from_slice(&bytes).expect("Problem body is JSON");
    assert_eq!(body["error_code"], "permission_denied");
    assert!(
        body["context"]["data"]["response_id"]
            .as_str()
            .is_some_and(|id| !id.is_empty()),
        "the Problem carries the allocated response id: {body}"
    );
}

#[tokio::test]
async fn unknown_model_is_a_404_problem() {
    let mut h = Harness::new();
    h.models = crate::domain::test_support::FakeModels::with(
        || Err(llm_gateway_sdk::LlmGatewayError::model_not_found("nope")),
        h.log.clone(),
    );
    let (_registry, router) = router_for(&h);
    let (status, content_type, bytes) = send(
        router,
        post(&serde_json::json!({ "model": "nope", "input": "hi" })),
    )
    .await;

    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(content_type.contains("problem+json"), "{content_type}");
    let body: serde_json::Value = serde_json::from_slice(&bytes).expect("Problem body is JSON");
    assert_eq!(body["error_code"], "model_not_found");
}

fn problem(bytes: &[u8]) -> serde_json::Value {
    serde_json::from_slice(bytes).expect("Problem body is JSON")
}

#[tokio::test]
async fn streaming_request_is_a_400_capability_not_supported_problem() {
    let h = Harness::new();
    let (_registry, router) = router_for(&h);
    let (status, content_type, bytes) = send(
        router,
        post(&serde_json::json!({ "model": "openai::gpt-4o", "input": "hi", "stream": true })),
    )
    .await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(content_type.contains("problem+json"), "{content_type}");
    assert_eq!(problem(&bytes)["error_code"], "capability_not_supported");
    assert_eq!(h.plugin.calls(), 0);
}

#[tokio::test]
async fn malformed_json_body_is_a_client_error() {
    let h = Harness::new();
    let (_registry, router) = router_for(&h);
    let request = Request::post(PATH)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from("{ not json"))
        .expect("request builds");
    let (status, _content_type, _bytes) = send(router, request).await;

    assert!(status.is_client_error(), "{status}");
    assert_eq!(h.plugin.calls(), 0);
}

#[tokio::test]
async fn body_without_model_is_a_client_error() {
    let h = Harness::new();
    let (_registry, router) = router_for(&h);
    let (status, _content_type, _bytes) =
        send(router, post(&serde_json::json!({ "input": "hi" }))).await;

    assert!(status.is_client_error(), "{status}");
    assert_eq!(h.plugin.calls(), 0);
}

#[tokio::test]
async fn provider_error_passes_through_with_status_and_response_id() {
    let mut h = Harness::new();
    h.set_plugin(FakePlugin::with(
        |_| {
            Err(LlmGatewayError::provider(
                ProviderErrorCategory::RateLimited,
                "slow down",
                ProviderFailure::default()
                    .with_provider_request_id("req_abc")
                    .with_retry_after(7),
            ))
        },
        h.log.clone(),
        false,
    ));
    let (_registry, router) = router_for(&h);
    let (status, content_type, bytes) = send(
        router,
        post(&serde_json::json!({ "model": "openai::gpt-4o", "input": "hi" })),
    )
    .await;

    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert!(content_type.contains("problem+json"), "{content_type}");
    let body = problem(&bytes);
    assert_eq!(body["error_code"], "rate_limited");
    assert_eq!(body["context"]["data"]["provider_request_id"], "req_abc");
    assert_eq!(
        body["context"]["data"]["response_id"],
        h.plugin.contexts()[0].request_id
    );
}

#[tokio::test]
async fn inflight_limit_is_a_503_gateway_unavailable_problem() {
    let mut h = Harness::new();
    h.limits.max_inflight_sync = 0;
    let (_registry, router) = router_for(&h);
    let (status, content_type, bytes) = send(
        router,
        post(&serde_json::json!({ "model": "openai::gpt-4o", "input": "hi" })),
    )
    .await;

    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert!(content_type.contains("problem+json"), "{content_type}");
    assert_eq!(problem(&bytes)["error_code"], "gateway_unavailable");
    assert_eq!(h.models.calls(), 0);
}

#[tokio::test]
#[tracing_test::traced_test]
async fn internal_failure_stays_redacted_on_the_wire_but_is_logged() {
    let mut h = Harness::new();
    h.set_plugin(FakePlugin::with(
        |_| {
            Err(LlmGatewayError::internal_from(
                "x",
                std::io::Error::other("disk exploded"),
            ))
        },
        h.log.clone(),
        false,
    ));
    let (_registry, router) = router_for(&h);
    let (status, _content_type, bytes) = send(
        router,
        post(&serde_json::json!({ "model": "openai::gpt-4o", "input": "hi" })),
    )
    .await;

    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    let text = String::from_utf8_lossy(&bytes).into_owned();
    assert!(text.contains("internal error"), "{text}");
    assert!(!text.contains("disk exploded"), "{text}");
    assert!(logs_contain("disk exploded"));
    assert!(logs_contain(&h.plugin.contexts()[0].request_id));
}
