#![allow(clippy::unwrap_used, clippy::expect_used)]

//! Integration tests for CORS preflight and actual request handling

use anyhow::Result;
use async_trait::async_trait;
use axum::{Router, extract::Json, routing::get};
use std::sync::Arc;
use toolkit::{
    Gear, GearCtx, RestApiCapability,
    api::OperationBuilder,
    config::ConfigProvider,
    contracts::{ApiGatewayCapability, OpenApiRegistry},
};
use uuid::Uuid;

/// Helper to create a test `GearCtx` with CORS config
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

fn create_test_gear_ctx_with_cors() -> GearCtx {
    let config = wrap_config(&serde_json::json!({
        "bind_addr": "127.0.0.1:0",
        "cors_enabled": true,
        "cors": {
            "allowed_origins": ["https://example.com"],
            "allowed_methods": ["GET", "POST", "PUT", "DELETE", "OPTIONS"],
            "allowed_headers": ["Content-Type", "Authorization"],
            "allow_credentials": true,
            "max_age_seconds": 3600
        },
        "auth_disabled": true
    }));

    let hub = Arc::new(toolkit::ClientHub::new());

    GearCtx::new(
        "api-gateway",
        Uuid::new_v4(),
        Arc::new(TestConfigProvider { config }),
        hub,
        tokio_util::sync::CancellationToken::new(),
    )
}

fn create_test_gear_ctx_permissive_cors() -> GearCtx {
    let config = wrap_config(&serde_json::json!({
        "bind_addr": "127.0.0.1:0",
        "cors_enabled": true,
        "auth_disabled": true
    }));

    let hub = Arc::new(toolkit::ClientHub::new());

    GearCtx::new(
        "api-gateway",
        Uuid::new_v4(),
        Arc::new(TestConfigProvider { config }),
        hub,
        tokio_util::sync::CancellationToken::new(),
    )
}

#[derive(Debug, Clone)]
#[toolkit_macros::api_dto(request, response)]
struct TestData {
    value: String,
}

pub struct CorsTestGear;

#[async_trait]
impl Gear for CorsTestGear {
    async fn init(&self, _ctx: &toolkit::GearCtx) -> Result<()> {
        Ok(())
    }
}

impl RestApiCapability for CorsTestGear {
    fn register_rest(
        &self,
        _ctx: &toolkit::GearCtx,
        router: axum::Router,
        openapi: &dyn OpenApiRegistry,
    ) -> Result<axum::Router> {
        let router = OperationBuilder::get("/tests/v1/cors/v1/cors-test")
            .operation_id("cors:test")
            .summary("CORS test endpoint")
            .public()
            .json_response(http::StatusCode::OK, "Success")
            .handler(get(test_handler))
            .register(router, openapi);

        let router = OperationBuilder::post("/tests/v1/cors/v1/cors-post")
            .operation_id("cors:post")
            .summary("CORS POST endpoint")
            .json_request::<TestData>(openapi, "Test data")
            .public()
            .json_response(http::StatusCode::OK, "Success")
            .handler(axum::routing::post(post_handler))
            .register(router, openapi);

        Ok(router)
    }
}

async fn test_handler() -> Json<TestData> {
    Json(TestData {
        value: "cors-test".to_owned(),
    })
}

async fn post_handler(Json(data): Json<TestData>) -> Json<TestData> {
    Json(data)
}

#[tokio::test]
async fn test_cors_layer_builds_with_config() {
    let api_gateway = api_gateway::ApiGateway::default();
    let ctx = create_test_gear_ctx_with_cors();
    api_gateway.init(&ctx).await.expect("Failed to init");

    let gear = CorsTestGear;
    let router = Router::new();
    let router = gear
        .register_rest(&ctx, router, &api_gateway)
        .expect("Failed to register routes");

    // Build the final router with CORS middleware
    let _final_router = api_gateway
        .rest_finalize(
            &ctx,
            router,
            Arc::new(toolkit::RestHealthcheckRegistry::new()),
        )
        .expect("Failed to finalize router");

    // Verify router builds successfully with CORS enabled
    // In a full test, we would start the server and make OPTIONS requests
}

#[tokio::test]
async fn test_cors_permissive_mode() {
    let api_gateway = api_gateway::ApiGateway::default();
    let ctx = create_test_gear_ctx_permissive_cors();
    api_gateway.init(&ctx).await.expect("Failed to init");

    let gear = CorsTestGear;
    let router = Router::new();
    let router = gear
        .register_rest(&ctx, router, &api_gateway)
        .expect("Failed to register routes");

    let _final_router = api_gateway
        .rest_finalize(
            &ctx,
            router,
            Arc::new(toolkit::RestHealthcheckRegistry::new()),
        )
        .expect("Failed to finalize router");

    // Verify permissive CORS builds successfully
}

#[tokio::test]
async fn test_cors_disabled() {
    let config = wrap_config(&serde_json::json!({
        "bind_addr": "127.0.0.1:0",
        "cors_enabled": false,
        "auth_disabled": true,
    }));

    let hub = Arc::new(toolkit::ClientHub::new());

    let ctx = GearCtx::new(
        "api-gateway",
        Uuid::new_v4(),
        Arc::new(TestConfigProvider { config }),
        hub,
        tokio_util::sync::CancellationToken::new(),
    );

    let api_gateway = api_gateway::ApiGateway::default();
    api_gateway.init(&ctx).await.expect("Failed to init");

    let gear = CorsTestGear;
    let router = Router::new();
    let router = gear
        .register_rest(&ctx, router, &api_gateway)
        .expect("Failed to register routes");

    let _final_router = api_gateway
        .rest_finalize(
            &ctx,
            router,
            Arc::new(toolkit::RestHealthcheckRegistry::new()),
        )
        .expect("Failed to finalize router");

    // Verify router builds without CORS layer
}

// Regression guard: the RBAC/AM write contract is ETag-guarded
// (`If-Match`), and browsers may only read non-safelisted response headers
// when the server sends `Access-Control-Expose-Headers`. The gateway used to
// emit no expose-headers at all, so cross-origin JS saw every `ETag` as
// absent and ETag-guarded writes were impossible from the UI. Assert the
// header is present on an actual cross-origin response — with the DEFAULT
// cors config (no explicit `cors` block), which must expose `ETag` out of
// the box.
#[tokio::test]
async fn test_cors_default_exposes_etag_header() {
    use tower::ServiceExt as _;

    let api_gateway = api_gateway::ApiGateway::default();
    let ctx = create_test_gear_ctx_permissive_cors();
    api_gateway.init(&ctx).await.expect("Failed to init");

    let gear = CorsTestGear;
    let router = gear
        .register_rest(&ctx, Router::new(), &api_gateway)
        .expect("Failed to register routes");
    let app = api_gateway
        .rest_finalize(
            &ctx,
            router,
            Arc::new(toolkit::RestHealthcheckRegistry::new()),
        )
        .expect("Failed to finalize router");

    let response = app
        .oneshot(
            http::Request::builder()
                .method(http::Method::GET)
                .uri("/tests/v1/cors/v1/cors-test")
                .header(http::header::ORIGIN, "https://ui.example.com")
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    let exposed = response
        .headers()
        .get(http::header::ACCESS_CONTROL_EXPOSE_HEADERS)
        .expect("Access-Control-Expose-Headers must be present on CORS responses")
        .to_str()
        .unwrap()
        .to_ascii_lowercase();
    assert!(
        exposed.split(',').map(str::trim).any(|h| h == "etag"),
        "ETag must be CORS-exposed so browser clients can perform \
         ETag-guarded writes, got: {exposed}"
    );
}

// Same guard for an explicitly configured `cors` block that names its own
// `exposed_headers` list — the configured values must reach the wire.
#[tokio::test]
async fn test_cors_configured_exposed_headers_reach_response() {
    use tower::ServiceExt as _;

    let config = wrap_config(&serde_json::json!({
        "bind_addr": "127.0.0.1:0",
        "cors_enabled": true,
        "cors": {
            "allowed_origins": ["https://example.com"],
            "allowed_methods": ["GET", "POST"],
            "allowed_headers": ["Content-Type"],
            "exposed_headers": ["ETag", "X-Request-Id"],
            "allow_credentials": true,
            "max_age_seconds": 600
        },
        "auth_disabled": true
    }));
    let hub = Arc::new(toolkit::ClientHub::new());
    let ctx = GearCtx::new(
        "api-gateway",
        Uuid::new_v4(),
        Arc::new(TestConfigProvider { config }),
        hub,
        tokio_util::sync::CancellationToken::new(),
    );

    let api_gateway = api_gateway::ApiGateway::default();
    api_gateway.init(&ctx).await.expect("Failed to init");

    let gear = CorsTestGear;
    let router = gear
        .register_rest(&ctx, Router::new(), &api_gateway)
        .expect("Failed to register routes");
    let app = api_gateway
        .rest_finalize(
            &ctx,
            router,
            Arc::new(toolkit::RestHealthcheckRegistry::new()),
        )
        .expect("Failed to finalize router");

    let response = app
        .oneshot(
            http::Request::builder()
                .method(http::Method::GET)
                .uri("/tests/v1/cors/v1/cors-test")
                .header(http::header::ORIGIN, "https://example.com")
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    let exposed = response
        .headers()
        .get(http::header::ACCESS_CONTROL_EXPOSE_HEADERS)
        .expect("Access-Control-Expose-Headers must be present")
        .to_str()
        .unwrap()
        .to_ascii_lowercase();
    for want in ["etag", "x-request-id"] {
        assert!(
            exposed.split(',').map(str::trim).any(|h| h == want),
            "expected {want} in Access-Control-Expose-Headers, got: {exposed}"
        );
    }
}

#[tokio::test]
async fn test_cors_config_validation() {
    // Test that CORS config is properly loaded
    let config = wrap_config(&serde_json::json!({
        "bind_addr": "127.0.0.1:0",
        "cors_enabled": true,
        "cors": {
            "allowed_origins": ["https://example.com"],
            "allowed_methods": ["GET", "POST"],
            "allowed_headers": ["Content-Type"],
            "allow_credentials": true,
            "max_age_seconds": 600
        },
        "auth_disabled": true
    }));

    let hub = Arc::new(toolkit::ClientHub::new());

    let ctx = GearCtx::new(
        "api-gateway",
        Uuid::new_v4(),
        Arc::new(TestConfigProvider { config }),
        hub,
        tokio_util::sync::CancellationToken::new(),
    );

    let api_gateway = api_gateway::ApiGateway::default();
    api_gateway.init(&ctx).await.expect("Failed to init");

    let loaded_config = api_gateway.get_config();
    assert!(loaded_config.cors_enabled, "CORS should be enabled");
    assert!(
        loaded_config.cors.is_some(),
        "CORS config should be present"
    );
}
