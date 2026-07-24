#![allow(clippy::unwrap_used, clippy::expect_used)]

//! Integration tests for health endpoints (`/health`, `/healthz`, `/readyz`).
//!
//! Health endpoints are unauthenticated in every serving mode. In `main`/`both` mode they are
//! merged onto the main gateway router at stable, unprefixed paths and outside the auth layer;
//! in `separate`/`both` mode the standalone health router (`health_router()`) is served on a
//! dedicated listener.

use async_trait::async_trait;
use authn_resolver_sdk::{
    AuthNResolverClient, AuthNResolverError, AuthenticationResult, ClientCredentialsRequest,
};
use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode, header},
};
use serde_json::json;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;
use toolkit::{
    ClientHub, GearCtx, Healthcheck, HealthcheckResult, RestHealthcheckRegistry,
    contracts::ApiGatewayCapability,
};
use tower::ServiceExt;
use uuid::Uuid;

// ---------- helpers ----------

/// A `ConfigProvider` backed by a single JSON blob, keyed by gear name.
struct JsonConfigProvider(serde_json::Value);

impl toolkit::config::ConfigProvider for JsonConfigProvider {
    fn get_gear_config(&self, gear: &str) -> Option<&serde_json::Value> {
        self.0.get(gear)
    }
}

/// Mock `AuthN` client that rejects every token. Registered for auth-enabled tests to prove
/// health endpoints never reach the auth layer (a real request would be rejected).
struct RejectingAuthN;

#[async_trait]
impl AuthNResolverClient for RejectingAuthN {
    async fn authenticate(&self, _token: &str) -> Result<AuthenticationResult, AuthNResolverError> {
        Err(AuthNResolverError::Unauthorized(
            "health tests: auth must never be invoked for health endpoints".to_owned(),
        ))
    }

    async fn exchange_client_credentials(
        &self,
        _request: &ClientCredentialsRequest,
    ) -> Result<AuthenticationResult, AuthNResolverError> {
        Err(AuthNResolverError::Internal("not implemented".to_owned()))
    }
}

/// `init` + `rest_prepare` + `rest_finalize` the gateway with the given `api-gateway.config`
/// object, an optional `AuthN` client, and a registry populated by `setup`. Returns the
/// finalized main router, or the first error (used by fail-fast config tests).
async fn finalize_main_router(
    config_obj: serde_json::Value,
    authn: Option<Arc<dyn AuthNResolverClient>>,
    setup: impl FnOnce(&RestHealthcheckRegistry),
) -> anyhow::Result<Router> {
    use api_gateway::ApiGateway;
    use toolkit::Gear;

    let registry = Arc::new(RestHealthcheckRegistry::new());
    setup(&registry);

    let config = json!({ "api-gateway": { "config": config_obj } });
    let hub = Arc::new(ClientHub::new());
    if let Some(client) = authn {
        hub.register::<dyn AuthNResolverClient>(client);
    }
    let ctx = GearCtx::new(
        "api-gateway",
        Uuid::new_v4(),
        Arc::new(JsonConfigProvider(config)),
        hub,
        CancellationToken::new(),
    );

    let gw = ApiGateway::default();
    gw.init(&ctx).await?;
    let router = gw.rest_prepare(&ctx, Router::new(), registry.clone())?;
    gw.rest_finalize(&ctx, router, registry)
}

/// Build the standalone health router (`rest_prepare` + `health_router`). This is the router
/// the framework serves on the separate health listener in `separate`/`both` mode.
async fn build_standalone_health_router(setup: impl FnOnce(&RestHealthcheckRegistry)) -> Router {
    use api_gateway::ApiGateway;
    use toolkit::Gear;

    let registry = Arc::new(RestHealthcheckRegistry::new());
    setup(&registry);

    let config =
        json!({ "api-gateway": { "config": { "bind_addr": "0.0.0.0:0", "auth_disabled": true } } });
    let ctx = GearCtx::new(
        "api-gateway",
        Uuid::new_v4(),
        Arc::new(JsonConfigProvider(config)),
        Arc::new(ClientHub::new()),
        CancellationToken::new(),
    );

    let gw = ApiGateway::default();
    gw.init(&ctx).await.expect("init failed");
    let _base = gw
        .rest_prepare(&ctx, Router::new(), registry)
        .expect("rest_prepare failed");
    gw.health_router().expect("health_router failed")
}

async fn get(router: Router, path: &str) -> axum::http::Response<axum::body::Body> {
    router
        .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
        .await
        .unwrap()
}

async fn get_with_bearer(
    router: Router,
    path: &str,
    token: &str,
) -> axum::http::Response<axum::body::Body> {
    router
        .oneshot(
            Request::builder()
                .uri(path)
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap()
}

async fn body_json(resp: axum::http::Response<axum::body::Body>) -> serde_json::Value {
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

const HEALTH_PATHS: [&str; 3] = ["/healthz", "/readyz", "/health"];

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

struct CodedUnhealthyCheck;
#[async_trait]
impl Healthcheck for CodedUnhealthyCheck {
    fn name(&self) -> &'static str {
        "coded-unhealthy"
    }
    async fn check(&self) -> HealthcheckResult {
        HealthcheckResult::unhealthy("database unreachable").with_code("db_unreachable")
    }
}

// ---------- default (main) mode: health on the main router, no auth ----------

#[tokio::test]
async fn default_main_serves_all_health_endpoints() {
    let router = finalize_main_router(
        json!({ "bind_addr": "0.0.0.0:0", "auth_disabled": true }),
        None,
        |reg| reg.register("my-gear", Arc::new(HealthyCheck)),
    )
    .await
    .expect("finalize failed");

    assert_eq!(
        get(router.clone(), "/healthz").await.status(),
        StatusCode::OK
    );
    assert_eq!(
        get(router.clone(), "/readyz").await.status(),
        StatusCode::OK
    );

    let resp = get(router, "/health").await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    assert_eq!(body["status"], "healthy");
    assert_eq!(body["components"][0]["gear"], "my-gear");
}

#[tokio::test]
async fn main_readyz_returns_503_when_unhealthy() {
    let router = finalize_main_router(
        json!({ "bind_addr": "0.0.0.0:0", "auth_disabled": true }),
        None,
        |reg| reg.register("gear", Arc::new(UnhealthyCheck)),
    )
    .await
    .expect("finalize failed");

    assert_eq!(
        get(router.clone(), "/readyz").await.status(),
        StatusCode::SERVICE_UNAVAILABLE
    );
    // /healthz is liveness only — stays 200 even when a readiness check is unhealthy.
    assert_eq!(get(router, "/healthz").await.status(), StatusCode::OK);
}

#[tokio::test]
async fn main_health_component_exposes_stable_code() {
    let router = finalize_main_router(
        json!({ "bind_addr": "0.0.0.0:0", "auth_disabled": true }),
        None,
        |reg| reg.register("gear", Arc::new(CodedUnhealthyCheck)),
    )
    .await
    .expect("finalize failed");

    let resp = get(router, "/health").await;
    assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    let body = body_json(resp).await;
    assert_eq!(body["components"][0]["code"], "db_unreachable");
}

#[tokio::test]
async fn main_health_paths_inherit_prefix() {
    // In main/both mode health rides the gateway's unified surface, so it inherits prefix_path
    // like every other route.
    let router = finalize_main_router(
        json!({ "bind_addr": "0.0.0.0:0", "auth_disabled": true, "prefix_path": "/cf" }),
        None,
        |reg| reg.register("gear", Arc::new(HealthyCheck)),
    )
    .await
    .expect("finalize failed");

    for path in HEALTH_PATHS {
        let prefixed = format!("/cf{path}");
        assert_eq!(
            get(router.clone(), &prefixed).await.status(),
            StatusCode::OK,
            "{prefixed} must be served under the configured prefix"
        );
        assert_eq!(
            get(router.clone(), path).await.status(),
            StatusCode::NOT_FOUND,
            "unprefixed {path} must be absent when prefix_path is set"
        );
    }
}

// ---------- auth enabled: health bypasses auth ----------

async fn auth_enabled_main_router() -> Router {
    finalize_main_router(
        json!({ "bind_addr": "0.0.0.0:0", "auth_disabled": false }),
        Some(Arc::new(RejectingAuthN)),
        |reg| reg.register("gear", Arc::new(HealthyCheck)),
    )
    .await
    .expect("finalize failed")
}

#[tokio::test]
async fn auth_enabled_health_accepts_missing_bearer() {
    let router = auth_enabled_main_router().await;
    for path in HEALTH_PATHS {
        assert_eq!(
            get(router.clone(), path).await.status(),
            StatusCode::OK,
            "{path} must be reachable without a bearer token when auth is enabled"
        );
    }
}

#[tokio::test]
async fn auth_enabled_health_ignores_invalid_bearer() {
    // A malformed/invalid bearer would be rejected by RejectingAuthN on any authed route;
    // health must never reach it.
    let router = auth_enabled_main_router().await;
    for path in HEALTH_PATHS {
        assert_eq!(
            get_with_bearer(router.clone(), path, "garbage-token")
                .await
                .status(),
            StatusCode::OK,
            "{path} must ignore an invalid bearer token"
        );
    }
}

// ---------- separate mode ----------

#[tokio::test]
async fn separate_mode_omits_health_from_main_router() {
    let router = finalize_main_router(
        json!({
            "bind_addr": "0.0.0.0:0",
            "auth_disabled": true,
            "health": { "serve": "separate", "bind_addr": "0.0.0.0:0" }
        }),
        None,
        |reg| reg.register("gear", Arc::new(HealthyCheck)),
    )
    .await
    .expect("finalize failed");

    for path in HEALTH_PATHS {
        assert_eq!(
            get(router.clone(), path).await.status(),
            StatusCode::NOT_FOUND,
            "{path} must not be on the main router in separate mode"
        );
    }
}

#[tokio::test]
async fn separate_health_router_serves_all_endpoints_without_auth() {
    let router = build_standalone_health_router(|reg| {
        reg.register("gear", Arc::new(HealthyCheck));
    })
    .await;

    assert_eq!(
        get(router.clone(), "/healthz").await.status(),
        StatusCode::OK
    );
    assert_eq!(
        get(router.clone(), "/readyz").await.status(),
        StatusCode::OK
    );
    assert_eq!(get(router, "/health").await.status(), StatusCode::OK);
}

#[tokio::test]
async fn separate_health_router_readyz_reflects_degraded_and_unhealthy() {
    let degraded = build_standalone_health_router(|reg| {
        reg.register("gear", Arc::new(DegradedCheck));
    })
    .await;
    // Degraded keeps the pod in rotation.
    assert_eq!(get(degraded, "/readyz").await.status(), StatusCode::OK);

    let unhealthy = build_standalone_health_router(|reg| {
        reg.register("gear", Arc::new(UnhealthyCheck));
    })
    .await;
    assert_eq!(
        get(unhealthy, "/readyz").await.status(),
        StatusCode::SERVICE_UNAVAILABLE
    );
}

// ---------- both mode ----------

#[tokio::test]
async fn both_mode_serves_health_on_main_and_separate_router() {
    use api_gateway::ApiGateway;
    use toolkit::Gear;

    let registry = Arc::new(RestHealthcheckRegistry::new());
    registry.register("gear", Arc::new(HealthyCheck));

    let config = json!({
        "api-gateway": { "config": {
            "bind_addr": "0.0.0.0:0",
            "auth_disabled": true,
            "health": { "serve": "both", "bind_addr": "0.0.0.0:0" }
        } }
    });
    let ctx = GearCtx::new(
        "api-gateway",
        Uuid::new_v4(),
        Arc::new(JsonConfigProvider(config)),
        Arc::new(ClientHub::new()),
        CancellationToken::new(),
    );

    let gw = ApiGateway::default();
    gw.init(&ctx).await.expect("init failed");
    let base = gw
        .rest_prepare(&ctx, Router::new(), registry.clone())
        .expect("rest_prepare failed");
    let main = gw
        .rest_finalize(&ctx, base, registry.clone())
        .expect("rest_finalize failed");
    let separate = gw.health_router().expect("health_router failed");

    for (label, router) in [("main", main), ("separate", separate)] {
        for path in HEALTH_PATHS {
            assert_eq!(
                get(router.clone(), path).await.status(),
                StatusCode::OK,
                "{label} listener must serve {path}"
            );
        }
    }
}

// ---------- fail-fast config validation ----------

#[tokio::test]
async fn separate_mode_without_bind_addr_fails_init() {
    let result = finalize_main_router(
        json!({ "bind_addr": "0.0.0.0:0", "auth_disabled": true, "health": { "serve": "separate" } }),
        None,
        |_| {},
    )
    .await;
    assert!(
        result.is_err(),
        "serve = separate without health.bind_addr must fail init"
    );
}

#[tokio::test]
async fn both_mode_without_bind_addr_fails_init() {
    let result = finalize_main_router(
        json!({ "bind_addr": "0.0.0.0:0", "auth_disabled": true, "health": { "serve": "both" } }),
        None,
        |_| {},
    )
    .await;
    assert!(
        result.is_err(),
        "serve = both without health.bind_addr must fail init"
    );
}

#[tokio::test]
async fn separate_mode_with_invalid_bind_addr_fails_init() {
    let result = finalize_main_router(
        json!({
            "bind_addr": "0.0.0.0:0",
            "auth_disabled": true,
            "health": { "serve": "separate", "bind_addr": "not-a-socket-addr" }
        }),
        None,
        |_| {},
    )
    .await;
    assert!(
        result.is_err(),
        "serve = separate with an unparseable health.bind_addr must fail init"
    );
}

// ---------- absent from OpenAPI ----------

#[tokio::test]
async fn health_endpoints_absent_from_openapi() {
    let router = finalize_main_router(
        json!({ "bind_addr": "0.0.0.0:0", "auth_disabled": true, "enable_docs": true }),
        None,
        |reg| reg.register("gear", Arc::new(HealthyCheck)),
    )
    .await
    .expect("finalize failed");

    let resp = get(router, "/openapi.json").await;
    assert_eq!(resp.status(), StatusCode::OK);

    let doc = body_json(resp).await;
    let paths = doc["paths"].as_object().expect("openapi paths object");
    for path in HEALTH_PATHS {
        assert!(
            !paths.contains_key(path),
            "{path} must not appear in the OpenAPI document"
        );
    }
}
