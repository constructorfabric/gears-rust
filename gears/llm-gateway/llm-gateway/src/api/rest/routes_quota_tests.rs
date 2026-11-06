//! Quota failures on the wire (DESIGN §4.1 `quota_exceeded` 429,
//! `quota_check_unavailable` 503, `permission_denied` 403, `internal` 500):
//! Problem shape with the response id, on the sync, stream and embeddings
//! routes.

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use toolkit::api::openapi_registry::OpenApiRegistryImpl;
use tower::ServiceExt as _;

use super::register_routes;
use crate::domain::test_support::{
    Harness, StreamScript, ctx, denied, ev_completed, qe_errors, remove_cost, set_cost,
};

fn router_for(h: &Harness) -> Router {
    register_routes(Router::new(), &OpenApiRegistryImpl::new(), h.service())
}

fn priced() -> Harness {
    let mut h = Harness::new();
    h.set_model(|v| set_cost(v, Some(1000), Some(2000)));
    h
}

fn post(path: &str, json: &serde_json::Value) -> Request<Body> {
    Request::post(path)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(json.to_string()))
        .expect("request builds")
}

async fn send(router: Router, request: Request<Body>) -> (StatusCode, String, serde_json::Value) {
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
        .expect("body collects");
    let body = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
    (status, content_type, body)
}

const RESPONSES: &str = "/llm-gateway/v1/responses";
const EMBEDDINGS: &str = "/llm-gateway/v1/embeddings";

fn sync_request() -> serde_json::Value {
    serde_json::json!({ "model": "openai::gpt-4o", "input": "hi" })
}

fn stream_request() -> serde_json::Value {
    serde_json::json!({ "model": "openai::gpt-4o", "input": "hi", "stream": true })
}

fn embedding_request() -> serde_json::Value {
    serde_json::json!({ "model": "openai::text-embedding", "input": "hi" })
}

#[tokio::test]
async fn a_denied_lease_is_a_429_quota_exceeded_problem_with_the_response_id() {
    let h = priced();
    h.quota.script_acquire([Ok(denied("CAP_EXCEEDED"))]);
    let (status, content_type, body) = send(router_for(&h), post(RESPONSES, &sync_request())).await;

    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert!(content_type.contains("problem+json"), "{content_type}");
    assert_eq!(body["error_code"], "quota_exceeded");
    assert!(
        body["context"]["data"]["response_id"]
            .as_str()
            .is_some_and(|id| id.starts_with("resp_")),
        "{body}"
    );
    assert!(
        !body.to_string().contains("CAP_EXCEEDED"),
        "the reason token is not exposed: {body}"
    );
    assert!(
        !body
            .to_string()
            .contains(&uuid::Uuid::from_u128(0xBAD).to_string()),
        "violated ids stay in the log"
    );
    assert_eq!(h.plugin.calls(), 0);
}

#[tokio::test]
async fn an_unreachable_quota_enforcement_is_a_503_quota_check_unavailable_problem() {
    for err in [
        qe_errors::unavailable(),
        qe_errors::deadline_exceeded(),
        qe_errors::exhausted("LEASE_INFLIGHT_LIMIT_EXCEEDED"),
        qe_errors::aborted("LEASE_CONTENTION_TIMEOUT"),
    ] {
        let h = priced();
        h.quota.script_acquire([Err(err.clone())]);
        let (status, content_type, body) =
            send(router_for(&h), post(RESPONSES, &sync_request())).await;

        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{err:?}");
        assert!(content_type.contains("problem+json"), "{content_type}");
        assert_eq!(body["error_code"], "quota_check_unavailable");
        assert!(body["context"]["data"]["response_id"].is_string());
        assert_eq!(h.plugin.calls(), 0);
    }
}

#[tokio::test]
async fn a_pdp_refusal_by_quota_enforcement_is_a_403_permission_denied_problem() {
    let h = priced();
    h.quota
        .script_acquire([Err(qe_errors::permission_denied())]);
    let (status, _content_type, body) =
        send(router_for(&h), post(RESPONSES, &sync_request())).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["error_code"], "permission_denied");
}

#[tokio::test]
async fn contract_faults_and_unlisted_errors_are_a_redacted_500_never_a_429() {
    for err in [
        qe_errors::invalid_argument("TTL_OUT_OF_BOUNDS"),
        qe_errors::exhausted("ENGINE_COST_EXCEEDED"),
        qe_errors::precondition("CONSTRAINT_CONTRACT_MISMATCH"),
    ] {
        let h = priced();
        h.quota.script_acquire([Err(err.clone())]);
        let (status, _content_type, body) =
            send(router_for(&h), post(RESPONSES, &sync_request())).await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{err:?}");
        assert_eq!(body["error_code"], "internal");
        let text = body.to_string();
        assert!(
            !text.contains("TTL_OUT_OF_BOUNDS") && !text.contains("ENGINE_COST_EXCEEDED"),
            "{text}"
        );
    }
}

#[tokio::test]
async fn a_model_without_pricing_is_a_500_internal_problem_before_any_dependency() {
    let mut h = Harness::new();
    h.set_model(remove_cost);
    let (status, _content_type, body) =
        send(router_for(&h), post(RESPONSES, &sync_request())).await;

    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(body["error_code"], "internal");
    assert!(body["context"]["data"]["response_id"].is_string());
    assert_eq!(h.broker.calls(), 0);
    assert!(h.quota.calls().is_empty());
}

#[tokio::test]
async fn a_pre_stream_quota_denial_is_a_json_problem_not_an_event_stream() {
    let mut h = Harness::new();
    h.enable_streaming_with(|v| set_cost(v, Some(1000), Some(2000)));
    h.plugin
        .set_stream(StreamScript::Events(vec![ev_completed(0, "p", None)]));
    h.quota.script_acquire([Ok(denied("CAP_EXCEEDED"))]);
    let (status, content_type, body) =
        send(router_for(&h), post(RESPONSES, &stream_request())).await;

    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert!(content_type.contains("problem+json"), "{content_type}");
    assert_eq!(body["error_code"], "quota_exceeded");
    assert_eq!(h.plugin.stream_calls(), 0);
}

#[tokio::test]
async fn a_pre_stream_unavailable_quota_check_is_a_503_problem() {
    let mut h = Harness::new();
    h.enable_streaming_with(|v| set_cost(v, Some(1000), Some(2000)));
    h.quota.script_acquire([Err(qe_errors::unavailable())]);
    let (status, _content_type, body) =
        send(router_for(&h), post(RESPONSES, &stream_request())).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(body["error_code"], "quota_check_unavailable");
}

#[tokio::test]
async fn embeddings_report_quota_failures_with_the_emb_request_id() {
    let mut h = Harness::new();
    h.enable_embeddings_with(|v| set_cost(v, Some(1000), None));
    h.quota
        .script_acquire([Ok(denied("CAP_EXCEEDED")), Err(qe_errors::unavailable())]);

    let (status, _ct, body) = send(router_for(&h), post(EMBEDDINGS, &embedding_request())).await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(body["error_code"], "quota_exceeded");
    assert!(
        body["context"]["data"]["response_id"]
            .as_str()
            .is_some_and(|id| id.starts_with("emb_"))
    );

    let (status, _ct, body) = send(router_for(&h), post(EMBEDDINGS, &embedding_request())).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(body["error_code"], "quota_check_unavailable");
    assert_eq!(h.plugin.embedding_calls(), 0);
}

#[tokio::test]
async fn an_embedding_model_without_the_input_rate_is_a_500() {
    let mut h = Harness::new();
    h.enable_embeddings_with(|v| set_cost(v, None, Some(5)));
    let (status, _ct, body) = send(router_for(&h), post(EMBEDDINGS, &embedding_request())).await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(body["error_code"], "internal");
}

#[tokio::test]
async fn an_admitted_request_still_answers_200_and_the_settle_does_not_change_the_response() {
    let h = priced();
    let (status, _ct, body) = send(router_for(&h), post(RESPONSES, &sync_request())).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["status"], "completed");
}
