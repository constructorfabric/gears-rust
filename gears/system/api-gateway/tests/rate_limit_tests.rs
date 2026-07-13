#![allow(clippy::unwrap_used, clippy::expect_used)]

//! Integration tests for per-route rate limiting and in-flight concurrency limits

use anyhow::Result;
use async_trait::async_trait;
use axum::{
    Router,
    body::Body,
    extract::Json,
    http::{Request, StatusCode, header},
    routing::get,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::time::{Duration, sleep};
use toolkit::{
    Gear, GearCtx, RestApiCapability,
    api::OperationBuilder,
    config::ConfigProvider,
    contracts::{ApiGatewayCapability, OpenApiRegistry},
};
use toolkit_canonical_errors::Problem;
use toolkit_gts::gts_uri;
use tower::ServiceExt;
use utoipa::ToSchema;
use uuid::Uuid;

const RESOURCE_EXHAUSTED_TYPE: &str =
    gts_uri!("cf.core.errors.err.v1~cf.core.err.resource_exhausted.v1~");
const SERVICE_UNAVAILABLE_TYPE: &str =
    gts_uri!("cf.core.errors.err.v1~cf.core.err.service_unavailable.v1~");
const PROBLEM_JSON: &str = "application/problem+json";

/// Helper to create a test `GearCtx`
struct TestConfigProvider {
    config: serde_json::Value,
}

impl ConfigProvider for TestConfigProvider {
    fn get_gear_config(&self, gear: &str) -> Option<&serde_json::Value> {
        if gear == "api-gateway" {
            Some(&self.config)
        } else {
            None
        }
    }
}

fn wrap_config(config: &serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "config": config
    })
}

fn create_test_gear_ctx_with_config(config: &serde_json::Value) -> GearCtx {
    let wrapped_config = wrap_config(config);
    let hub = Arc::new(toolkit::ClientHub::new());

    GearCtx::new(
        "api-gateway",
        Uuid::new_v4(),
        Arc::new(TestConfigProvider {
            config: wrapped_config,
        }),
        hub,
        tokio_util::sync::CancellationToken::new(),
    )
}

#[derive(Serialize, Deserialize, ToSchema, Debug, Clone)]
struct TestResponse {
    message: String,
}

/// Test gear with rate-limited routes
pub struct RateLimitedGear;

#[async_trait]
impl Gear for RateLimitedGear {
    async fn init(&self, _ctx: &toolkit::GearCtx) -> Result<()> {
        Ok(())
    }
}

impl RestApiCapability for RateLimitedGear {
    fn register_rest(
        &self,
        _ctx: &toolkit::GearCtx,
        router: axum::Router,
        openapi: &dyn OpenApiRegistry,
    ) -> Result<axum::Router> {
        // Route with strict rate limit: 1 RPS, burst 1
        let mut builder = OperationBuilder::get("/tests/v1/limited");
        builder.require_rate_limit(1, 1, 2);
        let router = builder
            .operation_id("test:limited")
            .summary("Strictly rate-limited endpoint")
            .public()
            .json_response(http::StatusCode::OK, "Success")
            .handler(get(limited_handler))
            .register(router, openapi);

        // Route with low in-flight limit
        let mut builder = OperationBuilder::get("/tests/v1/slow");
        builder.require_rate_limit(100, 100, 2);
        let router = builder
            .operation_id("test:slow")
            .summary("Slow endpoint with low in-flight limit")
            .public()
            .json_response(http::StatusCode::OK, "Success")
            .handler(get(slow_handler))
            .register(router, openapi);

        // Normal route without explicit limits (uses defaults)
        let router = OperationBuilder::get("/tests/v1/normal")
            .operation_id("test:normal")
            .summary("Normal endpoint")
            .public()
            .json_response(http::StatusCode::OK, "Success")
            .handler(get(normal_handler))
            .register(router, openapi);

        Ok(router)
    }
}

async fn limited_handler() -> Json<TestResponse> {
    Json(TestResponse {
        message: "limited".to_owned(),
    })
}

async fn slow_handler() -> Json<TestResponse> {
    // Simulate slow processing
    sleep(Duration::from_millis(200)).await;
    Json(TestResponse {
        message: "slow".to_owned(),
    })
}

async fn normal_handler() -> Json<TestResponse> {
    Json(TestResponse {
        message: "normal".to_owned(),
    })
}

#[tokio::test]
async fn test_rate_limit_enforcement() {
    // Create API gateway with rate limiting enabled
    let config = serde_json::json!({
        "bind_addr": "127.0.0.1:0",
        "cors_enabled": false,
        "auth_disabled": true,
        "defaults": {
            "rate_limit": {
                "rps": 50,
                "burst": 100,
                "in_flight": 64
            }
        }
    });

    let api_gateway = api_gateway::ApiGateway::default();
    let ctx = create_test_gear_ctx_with_config(&config);
    api_gateway.init(&ctx).await.expect("Failed to init");

    let gear = RateLimitedGear;
    let router = Router::new();
    let router = gear
        .register_rest(&ctx, router, &api_gateway)
        .expect("Failed to register routes");

    // Build the final router with middleware
    let _final_router = api_gateway
        .rest_finalize(&ctx, router)
        .expect("Failed to finalize router");

    // Note: Full HTTP testing would require starting a server and making real requests
    // This test verifies the router builds successfully with rate limit metadata
}

#[tokio::test]
async fn test_openapi_includes_rate_limit_extensions() {
    let config = serde_json::json!({
        "bind_addr": "127.0.0.1:0",
        "cors_enabled": false,
        "auth_disabled": true
    });

    let api_gateway = api_gateway::ApiGateway::default();
    let ctx = create_test_gear_ctx_with_config(&config);
    api_gateway.init(&ctx).await.expect("Failed to init");

    let gear = RateLimitedGear;
    let router = Router::new();
    let _router = gear
        .register_rest(&ctx, router, &api_gateway)
        .expect("Failed to register routes");

    // Build OpenAPI spec
    let openapi = api_gateway
        .build_openapi()
        .expect("Failed to build OpenAPI");
    let json = serde_json::to_value(&openapi).expect("Failed to serialize OpenAPI");

    // Verify rate limit extensions are present for the limited endpoint
    // Path is /tests/v1/limited, JSON pointer escapes / as ~1
    let limited_op = json
        .pointer("/paths/~1tests~1v1~1limited/get")
        .expect("Limited endpoint not found in OpenAPI");

    // Check for vendor extensions
    if let Some(rps) = limited_op.get("x-rate-limit-rps") {
        assert_eq!(rps.as_u64(), Some(1), "RPS should be 1");
    } else {
        panic!("x-rate-limit-rps extension not found");
    }

    if let Some(burst) = limited_op.get("x-rate-limit-burst") {
        assert_eq!(burst.as_u64(), Some(1), "Burst should be 1");
    } else {
        panic!("x-rate-limit-burst extension not found");
    }

    if let Some(in_flight) = limited_op.get("x-in-flight-limit") {
        assert_eq!(in_flight.as_u64(), Some(2), "In-flight should be 2");
    } else {
        panic!("x-in-flight-limit extension not found");
    }
}

#[tokio::test]
async fn test_rate_limit_metadata_stored() {
    let api_gateway = api_gateway::ApiGateway::default();
    let router = Router::<()>::new();

    let mut builder = OperationBuilder::get("/tests/v1/test");
    builder.require_rate_limit(10, 20, 5);

    let spec = builder.spec();
    assert!(spec.rate_limit.is_some(), "Rate limit should be set");
    let rl = spec.rate_limit.as_ref().unwrap();
    assert_eq!(rl.rps, 10);
    assert_eq!(rl.burst, 20);
    assert_eq!(rl.in_flight, 5);

    // Register and verify it's stored
    let _router = builder
        .operation_id("test")
        .public()
        .json_response(http::StatusCode::OK, "OK")
        .handler(get(normal_handler))
        .register(router, &api_gateway);

    // The operation should be registered with rate limit metadata
    let openapi = api_gateway
        .build_openapi()
        .expect("Failed to build OpenAPI");
    let json = serde_json::to_value(&openapi).expect("Failed to serialize");

    // Path is /tests/v1/test, JSON pointer escapes / as ~1
    let test_op = json.pointer("/paths/~1tests~1v1~1test/get");
    assert!(test_op.is_some(), "Test endpoint should be in OpenAPI");
}

#[tokio::test]
async fn test_rate_limit_returns_canonical_problem_with_headers() {
    // Configure a strict rate limit so the second request is rejected.
    let config = serde_json::json!({
        "bind_addr": "127.0.0.1:0",
        "cors_enabled": false,
        "auth_disabled": true,
        "defaults": {
            "rate_limit": {
                "rps": 1,
                "burst": 1,
                "in_flight": 64
            }
        }
    });

    let api_gateway = api_gateway::ApiGateway::default();
    let ctx = create_test_gear_ctx_with_config(&config);
    api_gateway.init(&ctx).await.expect("Failed to init");

    let gear = RateLimitedGear;
    let router = Router::new();
    let router = gear
        .register_rest(&ctx, router, &api_gateway)
        .expect("Failed to register routes");

    let app = api_gateway
        .rest_finalize(&ctx, router)
        .expect("Failed to finalize router");

    // First request consumes the only token.
    let res1 = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/tests/v1/limited")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("Request failed");
    assert_eq!(res1.status(), StatusCode::OK);

    // Second request should be rejected with canonical resource_exhausted Problem.
    let res2 = app
        .oneshot(
            Request::builder()
                .uri("/tests/v1/limited")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("Request failed");

    assert_eq!(res2.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(
        res2.headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or(""),
        PROBLEM_JSON
    );

    // Preserve existing rate-limit metadata headers + Retry-After
    assert!(
        res2.headers().get("RateLimit-Policy").is_some(),
        "RateLimit-Policy header must be present on 429"
    );
    assert!(
        res2.headers().get("RateLimit-Limit").is_some(),
        "RateLimit-Limit header must be present on 429"
    );
    assert!(
        res2.headers().get("X-RateLimit-Limit").is_some(),
        "X-RateLimit-Limit header must be present on 429"
    );
    assert!(
        res2.headers().get(header::RETRY_AFTER).is_some(),
        "Retry-After header must be present on 429"
    );

    let body = axum::body::to_bytes(res2.into_body(), usize::MAX)
        .await
        .expect("read body");
    let problem: Problem = serde_json::from_slice(&body).expect("parse Problem JSON");
    assert_eq!(problem.problem_type, RESOURCE_EXHAUSTED_TYPE);
    let violations = problem
        .context
        .get("violations")
        .and_then(|v| v.as_array())
        .expect("violations must be present");
    assert_eq!(violations.len(), 1);
    assert_eq!(violations[0]["subject"], "rate_limit");
}

#[tokio::test]
async fn test_in_flight_limit_returns_canonical_service_unavailable() {
    // Configure a tiny in-flight cap (1) so the second concurrent request hits 503.
    let config = serde_json::json!({
        "bind_addr": "127.0.0.1:0",
        "cors_enabled": false,
        "auth_disabled": true,
        "defaults": {
            "rate_limit": {
                "rps": 1000,
                "burst": 1000,
                "in_flight": 1
            }
        }
    });

    let api_gateway = api_gateway::ApiGateway::default();
    let ctx = create_test_gear_ctx_with_config(&config);
    api_gateway.init(&ctx).await.expect("Failed to init");

    // Register a route that uses the gateway defaults (no per-route override).
    let router = OperationBuilder::get("/tests/v1/inflight")
        .operation_id("test:inflight")
        .summary("In-flight cap test endpoint")
        .public()
        .json_response(http::StatusCode::OK, "Success")
        .handler(get(slow_handler))
        .register(Router::new(), &api_gateway);

    let app = api_gateway
        .rest_finalize(&ctx, router)
        .expect("Failed to finalize router");

    // Start one slow request and immediately fire a second one while the first holds the only permit.
    let app_clone = app.clone();
    let first = tokio::spawn(async move {
        app_clone
            .oneshot(
                Request::builder()
                    .uri("/tests/v1/inflight")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .expect("first request failed")
    });

    // Yield once to let the first request acquire the permit before we send the second.
    tokio::time::sleep(Duration::from_millis(20)).await;

    let res2 = app
        .oneshot(
            Request::builder()
                .uri("/tests/v1/inflight")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("second request failed");

    let _ = first.await.expect("first task panicked");

    assert_eq!(res2.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        res2.headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or(""),
        PROBLEM_JSON
    );

    let body = axum::body::to_bytes(res2.into_body(), usize::MAX)
        .await
        .expect("read body");
    let problem: Problem = serde_json::from_slice(&body).expect("parse Problem JSON");
    assert_eq!(problem.problem_type, SERVICE_UNAVAILABLE_TYPE);
    assert_eq!(problem.context["retry_after_seconds"], 5);
}
