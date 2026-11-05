//! `POST /llm-gateway/v1/responses` with media input: the 400 Problems of
//! the media checks (DESIGN §4.1 `media_not_found`, `capability_not_supported`,
//! `validation_error`).

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use serde_json::json;
use tower::ServiceExt as _;

use super::register_routes;
use crate::domain::test_support::{Harness, ctx};
use toolkit::api::openapi_registry::OpenApiRegistryImpl;

const PATH: &str = "/llm-gateway/v1/responses";
const PNG_URI: &str = "data:image/png;base64,iVBORw0KGgo=";

fn harness() -> Harness {
    let mut h = Harness::new();
    h.set_model(|v| {
        v["capabilities"]["vision"]["enabled"] = json!(true);
        v["capabilities"]["file_input"]["enabled"] = json!(true);
    });
    h
}

fn router_for(h: &Harness) -> Router {
    register_routes(Router::new(), &OpenApiRegistryImpl::new(), h.service())
}

#[allow(clippy::needless_pass_by_value)] // call sites build the parts inline with `json!`
fn payload(parts: serde_json::Value) -> serde_json::Value {
    json!({
        "model": "openai::gpt-4o",
        "input": [{ "type": "message", "role": "user", "content": parts }]
    })
}

async fn send(h: &Harness, body: &serde_json::Value) -> (StatusCode, String, serde_json::Value) {
    let request = Request::post(PATH)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string()))
        .expect("request builds");
    let response = router_for(h)
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
    let json = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
    (status, content_type, json)
}

#[tokio::test]
async fn a_rejected_url_is_a_400_media_not_found_problem_naming_the_part() {
    let h = harness();
    let (status, content_type, problem) = send(
        &h,
        &payload(json!([
            { "type": "input_text", "text": "x" },
            { "type": "input_image", "image_url": "http://secret-host.example/a.png" }
        ])),
    )
    .await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(content_type.contains("problem+json"), "{content_type}");
    assert_eq!(problem["error_code"], "media_not_found");
    assert_eq!(problem["context"]["data"]["code"], "media_not_found");
    assert_eq!(problem["context"]["data"]["param"], "input[0].content[1]");
    assert!(!problem.to_string().contains("secret-host"), "{problem}");
    assert_eq!(h.plugin.calls(), 0);
}

#[tokio::test]
async fn a_disallowed_data_uri_type_is_a_400_media_not_found_problem() {
    let h = harness();
    let (status, _, problem) = send(
        &h,
        &payload(json!([
            { "type": "input_image", "image_url": "data:image/svg+xml;base64,aGk=" }
        ])),
    )
    .await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(problem["error_code"], "media_not_found");
    assert_eq!(problem["context"]["data"]["param"], "input[0].content[0]");
}

#[tokio::test]
async fn file_id_input_is_a_400_capability_not_supported_problem() {
    let h = harness();
    let (status, content_type, problem) = send(
        &h,
        &payload(json!([{ "type": "input_file", "file_id": "file_1" }])),
    )
    .await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(content_type.contains("problem+json"), "{content_type}");
    assert_eq!(problem["error_code"], "capability_not_supported");
    assert!(problem.to_string().contains("file_id"), "{problem}");
    assert_eq!(h.plugin.calls(), 0);
}

#[tokio::test]
async fn audio_and_video_input_are_400_capability_not_supported_problems() {
    for (part, capability) in [
        (
            json!({ "type": "input_audio", "data": "aGk=" }),
            "audio_input",
        ),
        (
            json!({ "type": "input_video", "url": "https://example.com/v.mp4" }),
            "video_input",
        ),
    ] {
        let h = harness();
        let (status, _, problem) = send(&h, &payload(json!([part]))).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(problem["error_code"], "capability_not_supported");
        assert!(problem.to_string().contains(capability), "{problem}");
    }
}

#[tokio::test]
async fn a_url_for_a_prefetch_plugin_without_a_route_is_a_400_capability_problem() {
    let h = harness();
    h.plugin.set_prefetch_media(true);
    let (status, _, problem) = send(
        &h,
        &payload(json!([{ "type": "input_image", "image_url": "https://example.com/a.png" }])),
    )
    .await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(problem["error_code"], "capability_not_supported");
    assert!(problem.to_string().contains("media_url"), "{problem}");
}

#[tokio::test]
async fn oversized_inline_media_is_a_400_validation_error_problem_with_param_input() {
    let mut h = harness();
    h.limits.max_inline_media_bytes = 4;
    let (status, _, problem) = send(
        &h,
        &payload(json!([{ "type": "input_image", "image_url": PNG_URI }])),
    )
    .await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(problem["error_code"], "validation_error");
    assert_eq!(problem["context"]["data"]["param"], "input");
    assert_eq!(h.plugin.calls(), 0);
}

#[tokio::test]
async fn valid_inline_media_is_a_200() {
    let h = harness();
    let (status, _, _) = send(
        &h,
        &payload(json!([{ "type": "input_image", "image_url": PNG_URI }])),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(h.plugin.calls(), 1);
}
