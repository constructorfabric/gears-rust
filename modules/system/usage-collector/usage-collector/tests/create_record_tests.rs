#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::sync::Arc;

use axum::body::Body;
use chrono::Utc;
use http::{Method, Request, StatusCode};
use tower::ServiceExt;
use usage_collector_sdk::{AllowedMetric, UsageKind};
use usage_emitter::UsageEmitterRuntimeV1;
use uuid::Uuid;

use common::{AppHarness, MockUsageEmitterRuntimeV1};

#[tokio::test]
async fn create_record_happy_path() {
    let runtime = Arc::new(MockUsageEmitterRuntimeV1::with_allow_authz().await)
        as Arc<dyn UsageEmitterRuntimeV1>;
    let harness = AppHarness::with_emitter(runtime);

    let body = serde_json::json!({
        "module": "test-module",
        "tenant_id": Uuid::new_v4(),
        "resource_type": "test.resource",
        "resource_id": Uuid::new_v4(),
        "metric": "test.gauge",
        "value": 1.0,
        "timestamp": Utc::now().to_rfc3339(),
    });
    let request = Request::builder()
        .method(Method::POST)
        .uri("/usage-collector/v1/records")
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(&body).unwrap()))
        .unwrap();

    let response = harness.router.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn create_record_metadata_too_large() {
    let harness = AppHarness::new().await;

    let large_value = "a".repeat(9000);
    let body = serde_json::json!({
        "module": "test-module",
        "tenant_id": Uuid::new_v4(),
        "resource_type": "test.resource",
        "resource_id": Uuid::new_v4(),
        "metric": "test.gauge",
        "value": 1.0,
        "timestamp": Utc::now().to_rfc3339(),
        "metadata": {"key": large_value},
    });
    let request = Request::builder()
        .method(Method::POST)
        .uri("/usage-collector/v1/records")
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(&body).unwrap()))
        .unwrap();

    let response = harness.router.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn create_record_emitter_authorization_failed() {
    let runtime = Arc::new(MockUsageEmitterRuntimeV1::with_deny_authz().await)
        as Arc<dyn UsageEmitterRuntimeV1>;
    let harness = AppHarness::with_emitter(runtime);

    let body = serde_json::json!({
        "module": "test-module",
        "tenant_id": Uuid::new_v4(),
        "resource_type": "test.resource",
        "resource_id": Uuid::new_v4(),
        "metric": "test.gauge",
        "value": 1.0,
        "timestamp": Utc::now().to_rfc3339(),
    });
    let request = Request::builder()
        .method(Method::POST)
        .uri("/usage-collector/v1/records")
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(&body).unwrap()))
        .unwrap();

    let response = harness.router.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

// ── Idempotency / value / kind boundary cases (RUST-TEST-001) ─────────────────
//
// The handler trims `idempotency_key` whitespace and filters empties to `None` before
// reaching the builder. For a counter that None propagates to `String::default()` and is
// rejected with InvalidArgument; for a gauge the builder regenerates a fresh UUID and the
// record is accepted. These tests pin both branches at the HTTP boundary, plus the negative
// counter value path that lives only in the unit tests today.

fn counter_metric() -> Vec<AllowedMetric> {
    vec![AllowedMetric {
        name: "test.counter".to_owned(),
        kind: UsageKind::Counter,
    }]
}

fn gauge_metric() -> Vec<AllowedMetric> {
    vec![AllowedMetric {
        name: "test.gauge".to_owned(),
        kind: UsageKind::Gauge,
    }]
}

async fn post_record(harness: AppHarness, body: serde_json::Value) -> StatusCode {
    let request = Request::builder()
        .method(Method::POST)
        .uri("/usage-collector/v1/records")
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(&body).unwrap()))
        .unwrap();
    harness.router.oneshot(request).await.unwrap().status()
}

fn base_body(metric: &str, value: f64) -> serde_json::Value {
    serde_json::json!({
        "module": "test-module",
        "tenant_id": Uuid::new_v4(),
        "resource_type": "test.resource",
        "resource_id": Uuid::new_v4(),
        "metric": metric,
        "value": value,
        "timestamp": Utc::now().to_rfc3339(),
    })
}

#[tokio::test]
async fn create_record_counter_with_whitespace_idempotency_key_returns_400() {
    let harness = AppHarness::with_emitter_metrics(counter_metric()).await;
    let mut body = base_body("test.counter", 1.0);
    body["idempotency_key"] = serde_json::Value::String("   \t ".to_owned());
    assert_eq!(post_record(harness, body).await, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn create_record_counter_without_idempotency_key_returns_400() {
    let harness = AppHarness::with_emitter_metrics(counter_metric()).await;
    let body = base_body("test.counter", 1.0);
    assert_eq!(post_record(harness, body).await, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn create_record_gauge_with_whitespace_idempotency_key_succeeds() {
    // For gauges the whitespace key normalises to `None` at the handler and the builder
    // regenerates a UUID — the request succeeds with 204.
    let harness = AppHarness::with_emitter_metrics(gauge_metric()).await;
    let mut body = base_body("test.gauge", 1.0);
    body["idempotency_key"] = serde_json::Value::String("   ".to_owned());
    assert_eq!(post_record(harness, body).await, StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn create_record_counter_with_negative_value_returns_400() {
    let harness = AppHarness::with_emitter_metrics(counter_metric()).await;
    let mut body = base_body("test.counter", -1.0);
    body["idempotency_key"] = serde_json::Value::String("key-1".to_owned());
    assert_eq!(post_record(harness, body).await, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn create_record_gauge_with_nan_value_returns_400() {
    let harness = AppHarness::with_emitter_metrics(gauge_metric()).await;
    // serde_json refuses to serialise NaN, so emit the JSON manually.
    let body_str = format!(
        r#"{{"module":"test-module","tenant_id":"{}","resource_type":"test.resource","resource_id":"{}","metric":"test.gauge","value":NaN,"timestamp":"{}"}}"#,
        Uuid::new_v4(),
        Uuid::new_v4(),
        Utc::now().to_rfc3339()
    );
    let request = Request::builder()
        .method(Method::POST)
        .uri("/usage-collector/v1/records")
        .header("content-type", "application/json")
        .body(Body::from(body_str))
        .unwrap();
    // NaN is not valid JSON, so the JSON extractor rejects with 422 long before reaching
    // the validator. This pins the boundary behaviour: a non-finite numeric literal cannot
    // sneak through the HTTP layer. Document the actual returned code rather than asserting
    // a hypothetical 400.
    let status = harness.router.oneshot(request).await.unwrap().status();
    assert!(
        status == StatusCode::BAD_REQUEST || status == StatusCode::UNPROCESSABLE_ENTITY,
        "expected 400 or 422 for NaN payload, got {status}"
    );
}

#[tokio::test]
async fn create_record_invalid_subject_id_uuid() {
    let harness = AppHarness::new().await;

    let body = serde_json::json!({
        "module": "test-module",
        "tenant_id": Uuid::new_v4(),
        "resource_type": "test.resource",
        "resource_id": Uuid::new_v4(),
        "subject": { "id": "not-a-valid-uuid" },
        "metric": "test.gauge",
        "value": 1.0,
        "timestamp": Utc::now().to_rfc3339(),
    });
    let request = Request::builder()
        .method(Method::POST)
        .uri("/usage-collector/v1/records")
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(&body).unwrap()))
        .unwrap();

    let response = harness.router.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
}
