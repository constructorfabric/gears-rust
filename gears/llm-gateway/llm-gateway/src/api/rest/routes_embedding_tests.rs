//! `POST /llm-gateway/v1/embeddings`: registration (DESIGN §3.3) and dispatch.

use axum::Router;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use llm_gateway_sdk::{EmbeddingResponse, LlmGatewayError, ProviderErrorCategory, ProviderFailure};
use toolkit::api::openapi_registry::OpenApiRegistryImpl;
use toolkit::api::operation_builder::OperationSpec;
use tower::ServiceExt as _;

use super::register_routes;
use crate::domain::test_support::{FakePdp, FakeRules, Harness, PdpMode, ctx};

const PATH: &str = "/llm-gateway/v1/embeddings";

fn router_for(h: &Harness) -> (OpenApiRegistryImpl, Router) {
    let registry = OpenApiRegistryImpl::new();
    let router = register_routes(Router::new(), &registry, h.service());
    (registry, router)
}

fn harness() -> Harness {
    let mut h = Harness::new();
    h.enable_embeddings();
    h
}

fn post(json: &serde_json::Value) -> Request<Body> {
    Request::post(PATH)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(json.to_string()))
        .expect("request builds")
}

fn valid() -> serde_json::Value {
    serde_json::json!({ "model": "openai::text-embedding", "input": "hi" })
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

fn problem(bytes: &[u8]) -> serde_json::Value {
    serde_json::from_slice(bytes).expect("Problem body is JSON")
}

fn spec(registry: &OpenApiRegistryImpl) -> OperationSpec {
    registry
        .operation_specs
        .get(&format!("{}:{PATH}", Method::POST.as_str()))
        .expect("POST /llm-gateway/v1/embeddings is registered")
        .value()
        .clone()
}

#[test]
fn create_embedding_is_registered_with_operation_id_and_authentication() {
    let (registry, _router) = router_for(&Harness::new());
    let spec = spec(&registry);

    assert_eq!(
        spec.operation_id.as_deref(),
        Some("llm_gateway.embeddings.create")
    );
    assert_eq!(spec.auth, toolkit::api::RouteAuth::Authenticated);
}

#[test]
fn create_embedding_declares_the_gateway_problem_responses() {
    let (registry, _router) = router_for(&Harness::new());
    let spec = spec(&registry);

    for status in [400u16, 403, 404, 429, 500, 501, 503] {
        assert!(
            spec.responses
                .iter()
                .any(|r| r.status == status && r.content_type == "application/problem+json"),
            "operation MUST declare problem response {status}"
        );
    }
    assert!(
        spec.responses
            .iter()
            .any(|r| r.status == 200 && r.content_type == "application/json")
    );
}

#[tokio::test]
async fn embeddings_request_returns_200_json_embedding_response() {
    let h = harness();
    let (_registry, router) = router_for(&h);
    let (status, content_type, bytes) = send(router, post(&valid())).await;

    assert_eq!(status, StatusCode::OK);
    assert!(
        content_type.starts_with("application/json"),
        "{content_type}"
    );
    let response: EmbeddingResponse =
        serde_json::from_slice(&bytes).expect("EmbeddingResponse body");
    assert_eq!(response.id, h.plugin.embedding_contexts()[0].request_id);
    assert!(response.id.starts_with("emb_"));
    assert_eq!(response.data.len(), 1);
}

#[tokio::test]
async fn optional_parameters_in_the_json_body_reach_the_plugin() {
    let h = harness();
    let (_registry, router) = router_for(&h);
    let (status, _ct, _bytes) = send(
        router,
        post(&serde_json::json!({
            "model": "openai::text-embedding",
            "input": ["a", "b"],
            "dimensions": 256,
            "input_type": "query",
            "encoding_format": "base64"
        })),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    let sent = &h.plugin.embedding_requests()[0];
    assert_eq!(sent.dimensions, Some(256));
    assert_eq!(
        sent.encoding_format,
        Some(llm_gateway_sdk::EncodingFormat::Base64)
    );
}

#[tokio::test]
async fn permission_denied_is_a_403_problem_carrying_the_request_id() {
    let mut h = harness();
    h.pdp = FakePdp::new(PdpMode::Deny, h.log.clone());
    let (_registry, router) = router_for(&h);
    let (status, content_type, bytes) = send(router, post(&valid())).await;

    assert_eq!(status, StatusCode::FORBIDDEN);
    assert!(content_type.contains("problem+json"), "{content_type}");
    let body = problem(&bytes);
    assert_eq!(body["error_code"], "permission_denied");
    assert!(
        body["context"]["data"]["response_id"]
            .as_str()
            .is_some_and(|id| id.starts_with("emb_")),
        "the Problem carries the allocated id: {body}"
    );
}

#[tokio::test]
async fn unknown_model_is_a_404_problem() {
    let mut h = harness();
    h.models = crate::domain::test_support::FakeModels::with(
        || Err(LlmGatewayError::model_not_found("nope")),
        h.log.clone(),
    );
    let (_registry, router) = router_for(&h);
    let (status, _ct, bytes) = send(router, post(&valid())).await;

    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(problem(&bytes)["error_code"], "model_not_found");
}

#[tokio::test]
async fn non_embedding_model_is_a_400_capability_not_supported_problem() {
    let h = Harness::new();
    let (_registry, router) = router_for(&h);
    let (status, content_type, bytes) = send(router, post(&valid())).await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(content_type.contains("problem+json"), "{content_type}");
    assert_eq!(problem(&bytes)["error_code"], "capability_not_supported");
    assert_eq!(h.plugin.embedding_calls(), 0);
}

#[tokio::test]
async fn empty_input_is_a_400_validation_problem_naming_input() {
    let h = harness();
    let (_registry, router) = router_for(&h);
    let (status, _ct, bytes) = send(
        router,
        post(&serde_json::json!({ "model": "openai::text-embedding", "input": [] })),
    )
    .await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    let body = problem(&bytes);
    assert_eq!(body["error_code"], "validation_error");
    assert_eq!(body["context"]["data"]["param"], "input");
    assert_eq!(h.plugin.embedding_calls(), 0);
}

#[tokio::test]
async fn an_empty_text_inside_a_batch_is_a_400_validation_problem_naming_input() {
    let h = harness();
    let (_registry, router) = router_for(&h);
    let (status, _ct, bytes) = send(
        router,
        post(&serde_json::json!({ "model": "openai::text-embedding", "input": ["a", ""] })),
    )
    .await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    let body = problem(&bytes);
    assert_eq!(body["error_code"], "validation_error");
    assert_eq!(body["context"]["data"]["param"], "input");
    assert_eq!(h.plugin.embedding_calls(), 0);
}

#[tokio::test]
async fn unsupported_dimensions_is_a_400_problem_naming_dimensions() {
    let mut h = harness();
    h.rules = FakeRules {
        reject_all_dimensions: true,
        ..FakeRules::default()
    };
    let (_registry, router) = router_for(&h);
    let (status, _ct, bytes) = send(
        router,
        post(&serde_json::json!({
            "model": "openai::text-embedding",
            "input": "hi",
            "dimensions": 7,
            "drop_unsupported_params": true
        })),
    )
    .await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    let body = problem(&bytes);
    assert_eq!(body["error_code"], "unsupported_parameter");
    assert_eq!(body["context"]["data"]["param"], "dimensions");
}

#[tokio::test]
async fn provider_error_passes_through_with_status_and_request_id() {
    let h = harness();
    h.plugin.set_embedding(|_| {
        Err(LlmGatewayError::provider(
            ProviderErrorCategory::RateLimited,
            "slow down",
            ProviderFailure::default()
                .with_provider_request_id("req_abc")
                .with_retry_after(7),
        ))
    });
    let (_registry, router) = router_for(&h);
    let (status, _ct, bytes) = send(router, post(&valid())).await;

    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    let body = problem(&bytes);
    assert_eq!(body["error_code"], "rate_limited");
    assert_eq!(body["context"]["data"]["provider_request_id"], "req_abc");
    assert_eq!(
        body["context"]["data"]["response_id"],
        h.plugin.embedding_contexts()[0].request_id
    );
}

#[tokio::test]
async fn inflight_limit_is_a_503_gateway_unavailable_problem() {
    let mut h = harness();
    h.limits.max_inflight_sync = 0;
    let (_registry, router) = router_for(&h);
    let (status, _ct, bytes) = send(router, post(&valid())).await;

    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(problem(&bytes)["error_code"], "gateway_unavailable");
    assert_eq!(h.models.calls(), 0);
}

#[tokio::test]
async fn malformed_json_body_is_a_client_error() {
    let h = harness();
    let (_registry, router) = router_for(&h);
    let request = Request::post(PATH)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from("{ not json"))
        .expect("request builds");
    let (status, _ct, _bytes) = send(router, request).await;

    assert!(status.is_client_error(), "{status}");
    assert_eq!(h.plugin.embedding_calls(), 0);
}

#[tokio::test]
async fn body_without_input_is_a_client_error() {
    let h = harness();
    let (_registry, router) = router_for(&h);
    let (status, _ct, _bytes) = send(
        router,
        post(&serde_json::json!({ "model": "openai::text-embedding" })),
    )
    .await;

    assert!(status.is_client_error(), "{status}");
    assert_eq!(h.plugin.embedding_calls(), 0);
}

#[tokio::test]
async fn a_plugin_is_not_reached_when_the_pdp_is_unreachable() {
    let mut h = harness();
    h.pdp = FakePdp::new(PdpMode::Unreachable, h.log.clone());
    let (_registry, router) = router_for(&h);
    let (status, _ct, _bytes) = send(router, post(&valid())).await;

    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(h.plugin.embedding_calls(), 0);
}
