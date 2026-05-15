#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::collections::HashMap;

use axum::body::{Body, to_bytes};
use http::{Method, Request, StatusCode};
use tower::ServiceExt;
use usage_collector::config::MetricConfig;
use usage_collector_sdk::UsageKind;

use common::AppHarness;

#[tokio::test]
async fn module_config_found() {
    let mut metrics = HashMap::new();
    metrics.insert(
        "cpu.usage".to_owned(),
        MetricConfig {
            kind: UsageKind::Gauge,
            modules: None,
        },
    );
    let harness = AppHarness::with_metrics(metrics).await;

    let request = Request::builder()
        .method(Method::GET)
        .uri("/usage-collector/v1/modules/my-module/config")
        .body(Body::empty())
        .unwrap();

    let response = harness.router.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    let metrics = json["allowed_metrics"].as_array().unwrap();
    assert_eq!(metrics.len(), 1);
    assert_eq!(metrics[0]["name"], "cpu.usage");
    assert_eq!(json["max_metadata_bytes"], serde_json::json!(8192));
}

#[tokio::test]
async fn module_config_not_found() {
    // Default harness has no metrics configured → service returns ModuleNotConfigured → 404.
    let harness = AppHarness::new().await;

    let request = Request::builder()
        .method(Method::GET)
        .uri("/usage-collector/v1/modules/unknown-module/config")
        .body(Body::empty())
        .unwrap();

    let response = harness.router.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}
