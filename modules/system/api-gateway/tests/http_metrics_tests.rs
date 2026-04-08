#![allow(clippy::unwrap_used, clippy::expect_used)]

//! Integration tests for HTTP metrics middleware.
//!
//! Verifies that the metrics middleware (now a `layer()` instead of `route_layer()`)
//! captures responses from all middleware layers — including auth, rate-limit, MIME,
//! and timeout — and that `http.route` uses the route template (not raw path).

use anyhow::Result;
use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode},
    response::IntoResponse,
};
use modkit::{
    Module, api::OperationBuilder, config::ConfigProvider, context::ModuleCtx,
    contracts::ApiGatewayCapability,
};
use opentelemetry_sdk::metrics::{
    InMemoryMetricExporter, Instrument, PeriodicReader, SdkMeterProvider, Stream,
    data::{AggregatedMetrics, MetricData},
};
use serde_json::json;
use std::sync::Arc;
use tokio::sync::Mutex;
use tower::ServiceExt;
use uuid::Uuid;

/// Global lock to serialize tests that mutate the OpenTelemetry global meter provider.
static METER_LOCK: Mutex<()> = Mutex::const_new(());

struct TestConfigProvider {
    config: serde_json::Value,
}

impl ConfigProvider for TestConfigProvider {
    fn get_module_config(&self, module: &str) -> Option<&serde_json::Value> {
        self.config.get(module)
    }
}

fn create_api_gateway_ctx(config: serde_json::Value) -> ModuleCtx {
    let hub = Arc::new(modkit::ClientHub::new());
    ModuleCtx::new(
        "api-gateway",
        Uuid::new_v4(),
        Arc::new(TestConfigProvider { config }),
        hub,
        tokio_util::sync::CancellationToken::new(),
        None,
    )
}

/// Install an in-memory meter provider as the OpenTelemetry global so that
/// `HttpMetrics::new` (which uses `opentelemetry::global::meter_with_scope`)
/// records into our exporter.
fn install_test_meter_provider() -> (SdkMeterProvider, InMemoryMetricExporter) {
    let exporter = InMemoryMetricExporter::default();
    let provider = SdkMeterProvider::builder()
        .with_reader(PeriodicReader::builder(exporter.clone()).build())
        .with_view(|_: &Instrument| Stream::builder().with_cardinality_limit(2000).build().ok())
        .build();
    opentelemetry::global::set_meter_provider(provider.clone());
    (provider, exporter)
}

/// Check whether a metric with the given name exists in the exported data (any type).
fn metric_exists(exporter: &InMemoryMetricExporter, name: &str) -> bool {
    let metrics = exporter.get_finished_metrics().unwrap();
    metrics.iter().any(|rm| {
        rm.scope_metrics()
            .any(|sm| sm.metrics().any(|m| m.name() == name))
    })
}

/// Extract the sum of all histogram data point counts for the named metric.
fn histogram_count(exporter: &InMemoryMetricExporter, name: &str) -> u64 {
    let metrics = exporter.get_finished_metrics().unwrap();
    let mut total = 0u64;
    for resource_metrics in &metrics {
        for scope_metrics in resource_metrics.scope_metrics() {
            for metric in scope_metrics.metrics() {
                if metric.name() == name
                    && let AggregatedMetrics::F64(MetricData::Histogram(hist)) = metric.data()
                {
                    for dp in hist.data_points() {
                        total += dp.count();
                    }
                }
            }
        }
    }
    total
}

/// Check whether a histogram data point exists with the given attribute values.
fn histogram_has_attributes(
    exporter: &InMemoryMetricExporter,
    name: &str,
    expected_attrs: &[(&str, &str)],
) -> bool {
    let metrics = exporter.get_finished_metrics().unwrap();
    for resource_metrics in &metrics {
        for scope_metrics in resource_metrics.scope_metrics() {
            for metric in scope_metrics.metrics() {
                if metric.name() == name
                    && let AggregatedMetrics::F64(MetricData::Histogram(hist)) = metric.data()
                {
                    for dp in hist.data_points() {
                        let attrs: Vec<_> = dp.attributes().collect();
                        let all_match = expected_attrs.iter().all(|(key, val)| {
                            attrs
                                .iter()
                                .any(|kv| kv.key.as_str() == *key && kv.value.as_str() == *val)
                        });
                        if all_match {
                            return true;
                        }
                    }
                }
            }
        }
    }
    false
}

async fn ok_handler() -> impl IntoResponse {
    StatusCode::OK
}

fn base_config() -> serde_json::Value {
    json!({
        "api-gateway": {
            "config": {
                "bind_addr": "127.0.0.1:0",
                "cors_enabled": false,
                "auth_disabled": true,
                "defaults": {
                    "rate_limit": { "rps": 1000, "burst": 1000, "in_flight": 64 }
                },
            }
        }
    })
}

#[tokio::test]
async fn metrics_capture_successful_request() -> Result<()> {
    let _lock = METER_LOCK.lock().await;
    let (provider, exporter) = install_test_meter_provider();
    let ctx = create_api_gateway_ctx(base_config());
    let api = api_gateway::ApiGateway::default();
    api.init(&ctx).await?;

    let router = OperationBuilder::get("/tests/v1/items")
        .operation_id("test:list-items")
        .summary("List items")
        .public()
        .json_response(StatusCode::OK, "OK")
        .handler(axum::routing::get(ok_handler))
        .register(Router::new(), &api);
    let app = api.rest_finalize(&ctx, router)?;

    let res = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/tests/v1/items")
                .body(Body::empty())?,
        )
        .await?;
    assert_eq!(res.status(), StatusCode::OK);

    provider.force_flush().unwrap();

    let count = histogram_count(&exporter, "http.server.request.duration");
    assert!(
        count >= 1,
        "expected at least 1 duration data point, got {count}"
    );

    assert!(
        histogram_has_attributes(
            &exporter,
            "http.server.request.duration",
            &[
                ("http.request.method", "GET"),
                ("http.route", "/tests/v1/items"),
                ("http.response.status_code", "200"),
            ]
        ),
        "duration histogram should have correct method/route/status attributes"
    );

    Ok(())
}

#[tokio::test]
async fn metrics_capture_mime_rejection() -> Result<()> {
    let _lock = METER_LOCK.lock().await;
    let (provider, exporter) = install_test_meter_provider();
    let ctx = create_api_gateway_ctx(base_config());
    let api = api_gateway::ApiGateway::default();
    api.init(&ctx).await?;

    let mut builder = OperationBuilder::post("/tests/v1/items");
    builder.require_rate_limit(1000, 1000, 64);
    let router = builder
        .operation_id("test:create-item")
        .summary("Create item")
        .public()
        .allow_content_types(&["application/json"])
        .json_response(StatusCode::OK, "OK")
        .handler(axum::routing::post(ok_handler))
        .register(Router::new(), &api);
    let app = api.rest_finalize(&ctx, router)?;

    let res = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/tests/v1/items")
                .header("content-type", "text/plain")
                .body(Body::from("hi"))?,
        )
        .await?;
    assert_eq!(res.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);

    provider.force_flush().unwrap();

    let count = histogram_count(&exporter, "http.server.request.duration");
    assert!(
        count >= 1,
        "MIME rejection (415) must be captured by metrics, got {count} data points"
    );

    assert!(
        histogram_has_attributes(
            &exporter,
            "http.server.request.duration",
            &[("http.response.status_code", "415"),]
        ),
        "duration histogram should record 415 status from MIME rejection"
    );

    Ok(())
}

#[tokio::test]
async fn metrics_capture_rate_limit() -> Result<()> {
    let _lock = METER_LOCK.lock().await;
    let cfg = json!({
        "api-gateway": {
            "config": {
                "bind_addr": "127.0.0.1:0",
                "cors_enabled": false,
                "auth_disabled": true,
                "defaults": {
                    "rate_limit": { "rps": 1, "burst": 1, "in_flight": 64 }
                },
            }
        }
    });

    let (provider, exporter) = install_test_meter_provider();
    let ctx = create_api_gateway_ctx(cfg);
    let api = api_gateway::ApiGateway::default();
    api.init(&ctx).await?;

    let mut builder = OperationBuilder::get("/tests/v1/limited");
    builder.require_rate_limit(1, 1, 64);
    let router = builder
        .operation_id("test:limited")
        .summary("Rate-limited endpoint")
        .public()
        .json_response(StatusCode::OK, "OK")
        .handler(axum::routing::get(ok_handler))
        .register(Router::new(), &api);
    let app = api.rest_finalize(&ctx, router)?;

    // First request — succeeds and consumes the token
    let res1 = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/tests/v1/limited")
                .body(Body::empty())?,
        )
        .await?;
    assert_eq!(res1.status(), StatusCode::OK);

    // Second request immediately — rate-limited
    let res2 = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/tests/v1/limited")
                .body(Body::empty())?,
        )
        .await?;
    assert_eq!(res2.status(), StatusCode::TOO_MANY_REQUESTS);

    provider.force_flush().unwrap();

    let count = histogram_count(&exporter, "http.server.request.duration");
    assert!(
        count >= 2,
        "both 200 and 429 must be captured by metrics, got {count} data points"
    );

    assert!(
        histogram_has_attributes(
            &exporter,
            "http.server.request.duration",
            &[("http.response.status_code", "429"),]
        ),
        "duration histogram should record 429 from rate limiting"
    );

    Ok(())
}

#[tokio::test]
async fn metrics_route_attribute_uses_template() -> Result<()> {
    let _lock = METER_LOCK.lock().await;
    let (provider, exporter) = install_test_meter_provider();
    let ctx = create_api_gateway_ctx(base_config());
    let api = api_gateway::ApiGateway::default();
    api.init(&ctx).await?;

    let router = OperationBuilder::get("/tests/v1/items/{id}")
        .operation_id("test:get-item")
        .summary("Get item")
        .public()
        .json_response(StatusCode::OK, "OK")
        .handler(axum::routing::get(ok_handler))
        .register(Router::new(), &api);
    let app = api.rest_finalize(&ctx, router)?;

    let res = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/tests/v1/items/42")
                .body(Body::empty())?,
        )
        .await?;
    assert_eq!(res.status(), StatusCode::OK);

    provider.force_flush().unwrap();

    // Must use the template "/tests/v1/items/{id}", NOT the raw path "/tests/v1/items/42"
    assert!(
        histogram_has_attributes(
            &exporter,
            "http.server.request.duration",
            &[("http.route", "/tests/v1/items/{id}"),]
        ),
        "http.route must be the template, not the concrete path"
    );

    // Verify it does NOT record the raw path
    assert!(
        !histogram_has_attributes(
            &exporter,
            "http.server.request.duration",
            &[("http.route", "/tests/v1/items/42"),]
        ),
        "http.route must NOT contain the concrete path (cardinality explosion)"
    );

    Ok(())
}

#[tokio::test]
async fn metrics_unmatched_route() -> Result<()> {
    let _lock = METER_LOCK.lock().await;
    let (provider, exporter) = install_test_meter_provider();
    let ctx = create_api_gateway_ctx(base_config());
    let api = api_gateway::ApiGateway::default();
    api.init(&ctx).await?;

    let router = OperationBuilder::get("/tests/v1/items")
        .operation_id("test:list-items")
        .summary("List items")
        .public()
        .json_response(StatusCode::OK, "OK")
        .handler(axum::routing::get(ok_handler))
        .register(Router::new(), &api);
    let app = api.rest_finalize(&ctx, router)?;

    let res = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/no/such/route")
                .body(Body::empty())?,
        )
        .await?;
    assert_eq!(res.status(), StatusCode::NOT_FOUND);

    provider.force_flush().unwrap();

    let count = histogram_count(&exporter, "http.server.request.duration");
    assert!(
        count >= 1,
        "unmatched route 404 must be captured by metrics, got {count} data points"
    );

    assert!(
        histogram_has_attributes(
            &exporter,
            "http.server.request.duration",
            &[
                ("http.route", "unmatched"),
                ("http.response.status_code", "404"),
            ]
        ),
        "unmatched route should have http.route='unmatched' and status 404"
    );

    Ok(())
}

fn prefixed_config() -> serde_json::Value {
    json!({
        "api-gateway": {
            "config": {
                "bind_addr": "127.0.0.1:0",
                "cors_enabled": false,
                "auth_disabled": true,
                "metrics": { "prefix": "myapp" },
                "defaults": {
                    "rate_limit": { "rps": 1000, "burst": 1000, "in_flight": 64 }
                },
            }
        }
    })
}

#[tokio::test]
async fn metrics_prefix_applied_to_instrument_names() -> Result<()> {
    let _lock = METER_LOCK.lock().await;
    let (provider, exporter) = install_test_meter_provider();
    let ctx = create_api_gateway_ctx(prefixed_config());
    let api = api_gateway::ApiGateway::default();
    api.init(&ctx).await?;

    let router = OperationBuilder::get("/tests/v1/items")
        .operation_id("test:list-items")
        .summary("List items")
        .public()
        .json_response(StatusCode::OK, "OK")
        .handler(axum::routing::get(ok_handler))
        .register(Router::new(), &api);
    let app = api.rest_finalize(&ctx, router)?;

    let res = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/tests/v1/items")
                .body(Body::empty())?,
        )
        .await?;
    assert_eq!(res.status(), StatusCode::OK);

    provider.force_flush().unwrap();

    let count = histogram_count(&exporter, "myapp.http.server.request.duration");
    assert!(
        count >= 1,
        "expected at least 1 data point for prefixed metric name, got {count}"
    );

    // Verify active_requests counter is also prefixed
    assert!(
        metric_exists(&exporter, "myapp.http.server.active_requests"),
        "active_requests counter should use the configured prefix"
    );

    // The unprefixed names should NOT exist when prefix is configured
    assert!(
        !metric_exists(&exporter, "http.server.request.duration"),
        "unprefixed duration should not exist when prefix is configured"
    );
    assert!(
        !metric_exists(&exporter, "http.server.active_requests"),
        "unprefixed active_requests should not exist when prefix is configured"
    );

    Ok(())
}
