#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use github_mirror::api::rest::routes::register_routes;
use toolkit::api::OpenApiRegistryImpl;
use tower::ServiceExt;

async fn test_router(api_base_url: &str) -> Router {
    let service = common::service(api_base_url).await;
    let openapi = OpenApiRegistryImpl::new();
    register_routes(Router::new(), &openapi, service)
}

async fn body_json(response: axum::http::Response<Body>) -> serde_json::Value {
    let bytes = to_bytes(response.into_body(), 1_000_000).await.unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

#[tokio::test]
async fn health_returns_200_with_gear_identity() {
    let router = test_router("https://api.github.com").await;

    let request = Request::builder()
        .uri("/github-mirror/v1/health")
        .body(Body::empty())
        .unwrap();
    let response = router.oneshot(request).await.unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let json = body_json(response).await;
    assert_eq!(json["gear"], "github-mirror");
    assert_eq!(json["api_base_url"], "https://api.github.com");
    assert_eq!(json["version"], env!("CARGO_PKG_VERSION"));
}

#[tokio::test]
async fn health_reflects_configured_base_url() {
    let router = test_router("https://github.example.corp/api/v3").await;

    let request = Request::builder()
        .uri("/github-mirror/v1/health")
        .body(Body::empty())
        .unwrap();
    let response = router.oneshot(request).await.unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let json = body_json(response).await;
    assert_eq!(json["api_base_url"], "https://github.example.corp/api/v3");
}

#[tokio::test]
async fn unknown_route_returns_404() {
    let router = test_router("https://api.github.com").await;

    let request = Request::builder()
        .uri("/github-mirror/v1/nope")
        .body(Body::empty())
        .unwrap();
    let response = router.oneshot(request).await.unwrap();

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}
