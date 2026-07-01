#![allow(clippy::unwrap_used, clippy::expect_used)]

//! Integration tests for health endpoints (/health, /healthz, /readyz).

use async_trait::async_trait;
use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode},
};
use serde_json::json;
use std::sync::Arc;
use toolkit::{
    ClientHub, Gear, Healthcheck, HealthcheckResult, RestApiCapability, contracts::OpenApiRegistry,
};
use tower::ServiceExt;
use uuid::Uuid;

// ---------- helpers ----------

struct TestConfigProvider;

impl toolkit::config::ConfigProvider for TestConfigProvider {
    fn get_gear_config(&self, _gear: &str) -> Option<&serde_json::Value> {
        None
    }
}

fn test_ctx_with_hub(hub: Arc<ClientHub>) -> toolkit::GearCtx {
    toolkit::GearCtx::new(
        "test-gear",
        Uuid::new_v4(),
        Arc::new(TestConfigProvider),
        hub,
        tokio_util::sync::CancellationToken::new(),
    )
}

/// Build a router via `rest_prepare` with a pre-populated registry in the hub.
fn build_router(setup: impl FnOnce(&toolkit::RestHealthcheckRegistry)) -> Router {
    use api_gateway::ApiGateway;
    use toolkit::contracts::ApiGatewayCapability;

    let hub = Arc::new(ClientHub::new());
    let registry = Arc::new(toolkit::RestHealthcheckRegistry::new());
    setup(&registry);
    hub.register::<toolkit::RestHealthcheckRegistry>(registry);

    let ctx = test_ctx_with_hub(hub);
    let gw = ApiGateway::default();
    gw.rest_prepare(&ctx, Router::new())
        .expect("rest_prepare failed")
}

async fn get(router: Router, path: &str) -> axum::http::Response<axum::body::Body> {
    router
        .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
        .await
        .unwrap()
}

async fn body_json(resp: axum::http::Response<axum::body::Body>) -> serde_json::Value {
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

// ---------- check stubs ----------

struct HealthyCheck;
#[async_trait]
impl Healthcheck for HealthyCheck {
    fn name(&self) -> &'static str {
        "always-healthy"
    }
}

struct DegradedCheck;
#[async_trait]
impl Healthcheck for DegradedCheck {
    fn name(&self) -> &'static str {
        "always-degraded"
    }
    async fn check(&self) -> HealthcheckResult {
        HealthcheckResult::degraded("cache warming up")
    }
}

struct UnhealthyCheck;
#[async_trait]
impl Healthcheck for UnhealthyCheck {
    fn name(&self) -> &'static str {
        "always-unhealthy"
    }
    async fn check(&self) -> HealthcheckResult {
        HealthcheckResult::unhealthy("database unreachable")
    }
}

// ---------- /healthz ----------

#[tokio::test]
async fn healthz_returns_200_with_no_checks() {
    let router = build_router(|_| {});
    let resp = get(router, "/healthz").await;
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn healthz_returns_200_even_when_unhealthy_check_registered() {
    let router = build_router(|reg| {
        reg.register("gear", Arc::new(UnhealthyCheck));
    });
    let resp = get(router, "/healthz").await;
    assert_eq!(resp.status(), StatusCode::OK);
}

// ---------- /readyz ----------

#[tokio::test]
async fn readyz_returns_200_with_no_checks() {
    let router = build_router(|_| {});
    let resp = get(router, "/readyz").await;
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn readyz_returns_200_when_all_healthy() {
    let router = build_router(|reg| {
        reg.register("gear", Arc::new(HealthyCheck));
    });
    let resp = get(router, "/readyz").await;
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn readyz_returns_200_when_degraded() {
    let router = build_router(|reg| {
        reg.register("gear", Arc::new(DegradedCheck));
    });
    let resp = get(router, "/readyz").await;
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn readyz_returns_503_when_unhealthy() {
    let router = build_router(|reg| {
        reg.register("gear", Arc::new(UnhealthyCheck));
    });
    let resp = get(router, "/readyz").await;
    assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
}

// ---------- /health ----------

#[tokio::test]
async fn health_returns_json_with_components() {
    let router = build_router(|reg| {
        reg.register("my-gear", Arc::new(HealthyCheck));
    });
    let resp = get(router, "/health").await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    assert_eq!(body["status"], "healthy");
    assert!(body["timestamp"].is_string());
    let components = body["components"].as_array().unwrap();
    assert_eq!(components.len(), 1);
    assert_eq!(components[0]["gear"], "my-gear");
    assert_eq!(components[0]["status"], "healthy");
}

#[tokio::test]
async fn health_returns_503_when_unhealthy() {
    let router = build_router(|reg| {
        reg.register("gear", Arc::new(UnhealthyCheck));
    });
    let resp = get(router, "/health").await;
    assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    let body = body_json(resp).await;
    assert_eq!(body["status"], "unhealthy");
}

#[tokio::test]
async fn health_returns_degraded_status_when_degraded() {
    let router = build_router(|reg| {
        reg.register("gear", Arc::new(DegradedCheck));
    });
    let resp = get(router, "/health").await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    assert_eq!(body["status"], "degraded");
}

// ---------- outside prefix ----------

#[tokio::test]
async fn health_endpoints_accessible_outside_prefix() {
    use api_gateway::ApiGateway;
    use toolkit::contracts::ApiGatewayCapability;

    struct ConfigMap(serde_json::Value);
    impl toolkit::config::ConfigProvider for ConfigMap {
        fn get_gear_config(&self, gear: &str) -> Option<&serde_json::Value> {
            self.0.get(gear)
        }
    }

    let config = json!({
        "api-gateway": {
            "config": {
                "bind_addr": "0.0.0.0:18081",
                "auth_disabled": true,
                "prefix_path": "/cf",
            }
        }
    });

    let hub = Arc::new(ClientHub::new());
    let registry = Arc::new(toolkit::RestHealthcheckRegistry::new());
    hub.register::<toolkit::RestHealthcheckRegistry>(registry);

    let gw = ApiGateway::default();
    let ctx = toolkit::GearCtx::new(
        "api-gateway",
        Uuid::new_v4(),
        Arc::new(ConfigMap(config)),
        hub,
        tokio_util::sync::CancellationToken::new(),
    );
    gw.init(&ctx).await.expect("init failed");

    let router = gw
        .rest_prepare(&ctx, Router::new())
        .expect("rest_prepare failed");
    let router = gw
        .rest_finalize(&ctx, router)
        .expect("rest_finalize failed");

    // Health endpoints must be reachable at root paths, not nested under /cf
    let resp = router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/healthz")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let resp = router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/readyz")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let resp = router
        .oneshot(
            Request::builder()
                .uri("/health")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

// ---------- RestApiCapability default ----------

struct MinimalGear;

#[async_trait]
impl toolkit::Gear for MinimalGear {
    async fn init(&self, _ctx: &toolkit::GearCtx) -> anyhow::Result<()> {
        Ok(())
    }
}

impl RestApiCapability for MinimalGear {
    fn register_rest(
        &self,
        _ctx: &toolkit::GearCtx,
        router: Router,
        _openapi: &dyn OpenApiRegistry,
    ) -> anyhow::Result<Router> {
        Ok(router)
    }
    // healthcheck() is intentionally NOT overridden — tests the default
}

#[test]
fn existing_rest_gear_compiles_without_healthcheck_impl() {
    let gear = MinimalGear;
    let ctx = toolkit::GearCtx::new(
        "test",
        Uuid::new_v4(),
        Arc::new(TestConfigProvider),
        Arc::new(ClientHub::new()),
        tokio_util::sync::CancellationToken::new(),
    );
    assert!(gear.healthcheck(&ctx).is_none());
}

struct GearWithHealthcheck;

#[async_trait]
impl toolkit::Gear for GearWithHealthcheck {
    async fn init(&self, _ctx: &toolkit::GearCtx) -> anyhow::Result<()> {
        Ok(())
    }
}

impl RestApiCapability for GearWithHealthcheck {
    fn register_rest(
        &self,
        _ctx: &toolkit::GearCtx,
        router: Router,
        _openapi: &dyn OpenApiRegistry,
    ) -> anyhow::Result<Router> {
        Ok(router)
    }

    fn healthcheck(&self, _ctx: &toolkit::GearCtx) -> Option<Arc<dyn Healthcheck>> {
        Some(Arc::new(HealthyCheck))
    }
}

#[test]
fn gear_can_return_some_healthcheck() {
    let gear = GearWithHealthcheck;
    let ctx = toolkit::GearCtx::new(
        "test",
        Uuid::new_v4(),
        Arc::new(TestConfigProvider),
        Arc::new(ClientHub::new()),
        tokio_util::sync::CancellationToken::new(),
    );
    assert!(gear.healthcheck(&ctx).is_some());
}
