//! `POST /llm-gateway/v1/responses`: registration (DESIGN §3.3) and dispatch.

use axum::Router;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use llm_gateway_sdk::{LlmGatewayError, ProviderErrorCategory, ProviderFailure, ResponseResource};
use toolkit::api::openapi_registry::OpenApiRegistryImpl;
use toolkit::api::operation_builder::OperationSpec;
use tower::ServiceExt as _;

use super::register_routes;
use crate::domain::test_support::{
    FakePlugin, FakeRules, Harness, PdpMode, StreamScript, ctx, ev_completed, ev_error,
    ev_text_delta,
};

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

#[tokio::test]
async fn unsupported_parameter_is_a_400_problem_naming_the_param() {
    let mut h = Harness::new();
    h.rules = FakeRules {
        unsupported_params: ["temperature"].into(),
        ..FakeRules::default()
    };
    let (_registry, router) = router_for(&h);
    let (status, content_type, bytes) = send(
        router,
        post(&serde_json::json!({ "model": "openai::gpt-4o", "input": "hi", "temperature": 0.5 })),
    )
    .await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(content_type.contains("problem+json"), "{content_type}");
    let body = problem(&bytes);
    assert_eq!(body["error_code"], "unsupported_parameter");
    assert_eq!(body["context"]["data"]["param"], "temperature");
    assert_eq!(h.plugin.calls(), 0);
}

// ── streaming (SSE) ─────────────────────────────────────────────────────────

fn stream_post() -> Request<Body> {
    post(&serde_json::json!({ "model": "openai::gpt-4o", "input": "hi", "stream": true }))
}

/// Harness whose model and plugin can stream the scripted `events`.
fn streaming_harness(events: Vec<llm_gateway_sdk::StreamingEvent>) -> Harness {
    let mut h = Harness::new();
    h.enable_streaming();
    h.plugin.set_stream(StreamScript::Events(events));
    h
}

/// Split an SSE body into frames: `(event name, data)`.
fn frames(bytes: &[u8]) -> Vec<(Option<String>, String)> {
    let text = String::from_utf8(bytes.to_vec()).expect("SSE body is UTF-8");
    assert!(
        text.ends_with("\n\n"),
        "every frame ends with a blank line: {text:?}"
    );
    text.split("\n\n")
        .filter(|frame| !frame.is_empty())
        .map(|frame| {
            let mut event = None;
            let mut data = Vec::new();
            for line in frame.lines() {
                if let Some(name) = line.strip_prefix("event: ") {
                    event = Some(name.to_owned());
                } else if let Some(value) = line.strip_prefix("data: ") {
                    data.push(value.to_owned());
                } else {
                    panic!("unexpected SSE line {line:?} in frame {frame:?}");
                }
            }
            (event, data.join("\n"))
        })
        .collect()
}

#[test]
fn create_response_declares_json_and_event_stream_success_media_types() {
    let (registry, _router) = router_for(&Harness::new());
    let spec = spec(&registry);

    for content_type in ["application/json", "text/event-stream"] {
        assert!(
            spec.responses
                .iter()
                .any(|r| r.status == 200 && r.content_type == content_type),
            "operation MUST declare a 200 {content_type} response"
        );
    }
}

#[tokio::test]
async fn stream_true_returns_200_event_stream() {
    let h = streaming_harness(vec![ev_completed(0, "p", None)]);
    let (_registry, router) = router_for(&h);
    let (status, content_type, _bytes) = send(router, stream_post()).await;

    assert_eq!(status, StatusCode::OK);
    assert!(
        content_type.starts_with("text/event-stream"),
        "{content_type}"
    );
}

#[tokio::test]
async fn sse_frames_carry_event_name_and_json_data_and_close_with_done() {
    let h = streaming_harness(vec![
        ev_text_delta(0, "he"),
        ev_text_delta(1, "llo"),
        ev_completed(2, "p", None),
    ]);
    let (_registry, router) = router_for(&h);
    let (_status, _content_type, bytes) = send(router, stream_post()).await;
    let frames = frames(&bytes);

    let (last_event, last_data) = frames.last().expect("frames");
    assert_eq!(*last_event, None, "[DONE] has no event line");
    assert_eq!(last_data, "[DONE]");

    let events: Vec<_> = frames[..frames.len() - 1].to_vec();
    let names: Vec<_> = events
        .iter()
        .map(|(n, _)| n.clone().expect("event name"))
        .collect();
    assert_eq!(
        names,
        [
            "response.created",
            "response.in_progress",
            "response.output_text.delta",
            "response.output_text.delta",
            "response.completed"
        ]
    );
    for (index, (name, data)) in events.iter().enumerate() {
        let json: serde_json::Value = serde_json::from_str(data).expect("data is JSON");
        assert_eq!(
            json["type"].as_str(),
            name.as_deref(),
            "event name matches type"
        );
        assert_eq!(json["sequence_number"], index as u64);
    }
    // Wire events decode as SDK streaming events.
    for (_, data) in &events {
        let _: llm_gateway_sdk::StreamingEvent =
            serde_json::from_str(data).expect("data decodes as a StreamingEvent");
    }
}

#[tokio::test]
async fn done_is_sent_exactly_once_and_last() {
    let h = streaming_harness(vec![ev_completed(0, "p", None)]);
    let (_registry, router) = router_for(&h);
    let (_status, _content_type, bytes) = send(router, stream_post()).await;
    let text = String::from_utf8(bytes).expect("UTF-8");

    assert_eq!(text.matches("data: [DONE]").count(), 1);
    assert!(text.ends_with("data: [DONE]\n\n"), "{text:?}");
}

#[tokio::test]
async fn other_event_with_a_line_break_in_its_type_does_not_break_the_stream() {
    let other = llm_gateway_sdk::StreamingEvent::Other(llm_gateway_sdk::Extension(
        serde_json::json!({ "type": "a\nb", "sequence_number": 1 }),
    ));
    let h = streaming_harness(vec![
        ev_text_delta(0, "x"),
        other,
        ev_completed(2, "p", None),
    ]);
    let (_registry, router) = router_for(&h);
    let (status, _content_type, bytes) = send(router, stream_post()).await;

    assert_eq!(status, StatusCode::OK);
    let frames = frames(&bytes);
    let other_frame = frames
        .iter()
        .find(|(_, data)| data.contains("\"a\\nb\""))
        .expect("the data of the Other event is delivered");
    assert_eq!(other_frame.0, None, "no event line for an unsafe name");
    let names: Vec<_> = frames.iter().filter_map(|(n, _)| n.clone()).collect();
    assert_eq!(names.last().map(String::as_str), Some("response.completed"));
    assert_eq!(frames.last().expect("frames").1, "[DONE]");
}

#[tokio::test]
async fn in_band_failure_is_a_200_stream_with_one_failed_event_then_done() {
    let h = streaming_harness(vec![
        ev_text_delta(0, "x"),
        ev_error(1, "provider_unavailable"),
    ]);
    let (_registry, router) = router_for(&h);
    let (status, content_type, bytes) = send(router, stream_post()).await;

    assert_eq!(status, StatusCode::OK);
    assert!(
        content_type.starts_with("text/event-stream"),
        "{content_type}"
    );
    let frames = frames(&bytes);
    let names: Vec<_> = frames.iter().filter_map(|(n, _)| n.clone()).collect();
    assert_eq!(names.last().map(String::as_str), Some("response.failed"));
    assert_eq!(names.iter().filter(|n| *n == "response.failed").count(), 1);
    assert!(
        !names.iter().any(|n| n == "error"),
        "no standalone error event: {names:?}"
    );
    let failed: serde_json::Value =
        serde_json::from_str(&frames[frames.len() - 2].1).expect("failed event JSON");
    assert_eq!(failed["response"]["error"]["code"], "provider_unavailable");
    assert_eq!(frames.last().expect("frames").1, "[DONE]");
}

#[tokio::test]
async fn stream_false_on_a_streaming_capable_model_still_returns_json() {
    let h = streaming_harness(Vec::new());
    let (_registry, router) = router_for(&h);
    let (status, content_type, _bytes) = send(
        router,
        post(&serde_json::json!({ "model": "openai::gpt-4o", "input": "hi", "stream": false })),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert!(
        content_type.starts_with("application/json"),
        "{content_type}"
    );
}

#[tokio::test]
async fn pre_stream_permission_denied_is_a_403_problem_not_a_200_stream() {
    let mut h = streaming_harness(Vec::new());
    h.pdp = crate::domain::test_support::FakePdp::new(PdpMode::Deny, h.log.clone());
    let (_registry, router) = router_for(&h);
    let (status, content_type, bytes) = send(router, stream_post()).await;

    assert_eq!(status, StatusCode::FORBIDDEN);
    assert!(content_type.contains("problem+json"), "{content_type}");
    let body = problem(&bytes);
    assert_eq!(body["error_code"], "permission_denied");
    assert!(
        body["context"]["data"]["response_id"]
            .as_str()
            .is_some_and(|id| id.starts_with("resp_")),
        "the Problem carries the allocated response id: {body}"
    );
    assert_eq!(h.plugin.stream_calls(), 0);
}

#[tokio::test]
async fn pre_stream_unknown_model_is_a_404_problem() {
    let mut h = streaming_harness(Vec::new());
    h.models = crate::domain::test_support::FakeModels::with(
        || Err(LlmGatewayError::model_not_found("nope")),
        h.log.clone(),
    );
    let (_registry, router) = router_for(&h);
    let (status, content_type, bytes) = send(router, stream_post()).await;

    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(content_type.contains("problem+json"), "{content_type}");
    assert_eq!(problem(&bytes)["error_code"], "model_not_found");
}

#[tokio::test]
async fn pre_stream_model_without_streaming_is_a_400_capability_not_supported_problem_after_the_pep_check()
 {
    let h = Harness::new(); // model has no `streaming` capability
    let (_registry, router) = router_for(&h);
    let (status, content_type, bytes) = send(router, stream_post()).await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(content_type.contains("problem+json"), "{content_type}");
    assert_eq!(problem(&bytes)["error_code"], "capability_not_supported");
    assert_eq!(
        h.pdp.calls(),
        1,
        "streaming requests are authorized (F-006)"
    );
    assert_eq!(h.plugin.stream_calls(), 0);
}

#[tokio::test]
async fn pre_stream_provider_start_failure_is_a_problem_with_the_provider_status() {
    let h = streaming_harness(Vec::new());
    h.plugin.set_stream(StreamScript::StartError(|| {
        LlmGatewayError::provider(
            ProviderErrorCategory::RateLimited,
            "slow down",
            ProviderFailure::default().with_retry_after(7),
        )
    }));
    let (_registry, router) = router_for(&h);
    let (status, content_type, bytes) = send(router, stream_post()).await;

    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert!(content_type.contains("problem+json"), "{content_type}");
    assert_eq!(problem(&bytes)["error_code"], "rate_limited");
}

#[tokio::test]
async fn stream_limit_is_a_503_gateway_unavailable_problem() {
    let mut h = streaming_harness(Vec::new());
    h.limits.max_inflight_streams = 0;
    let (_registry, router) = router_for(&h);
    let (status, content_type, bytes) = send(router, stream_post()).await;

    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert!(content_type.contains("problem+json"), "{content_type}");
    assert_eq!(problem(&bytes)["error_code"], "gateway_unavailable");
    assert_eq!(h.models.calls(), 0);
}

#[tokio::test]
async fn dropping_the_http_response_cancels_the_request() {
    let h = streaming_harness(Vec::new());
    h.plugin
        .set_stream(StreamScript::EventsThenHang(vec![ev_text_delta(0, "x")]));
    let (_registry, router) = router_for(&h);
    let response = router
        .layer(axum::Extension(ctx()))
        .oneshot(stream_post())
        .await
        .expect("Router's error type is Infallible");
    assert_eq!(response.status(), StatusCode::OK);
    let cancel = h.plugin.contexts()[0].cancel.clone();
    assert!(!cancel.is_cancelled());

    // The connection closes: the body is dropped without being read to the end.
    drop(response);

    tokio::time::timeout(std::time::Duration::from_secs(2), cancel.cancelled())
        .await
        .expect("the request token is cancelled on disconnect");
    h.plugin.wait_stream_dropped().await;
}
