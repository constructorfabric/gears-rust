#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::sync::Arc;

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use credstore_sdk::CredStoreClientV1;
use credstore_sdk::test_util::MockCredStoreClient;
use github_mirror::GithubMirrorGear;
use github_mirror::domain::error::DomainError;
use github_mirror::domain::ports::github::RateLimitQuota;
use github_mirror::domain::service::Service;
use httpmock::MockServer;
use toolkit::api::OpenApiRegistryImpl;
use toolkit::{ClientHub, Gear, RestApiCapability};
use tower::ServiceExt;

#[tokio::test]
async fn init_then_register_rest_serves_health_without_the_upstream_url() {
    let gear = GithubMirrorGear::default();
    let ctx = common::gear_ctx(
        Arc::new(ClientHub::new()),
        Some(serde_json::json!({
            "config": { "api_base_url": "https://ghe.corp/api/v3" }
        })),
    )
    .await;

    gear.init(&ctx).await.expect("init must succeed");

    let openapi = OpenApiRegistryImpl::new();
    let router = gear
        .register_rest(&ctx, Router::new(), &openapi)
        .expect("register_rest must succeed after init");

    let request = Request::builder()
        .uri("/github-mirror/v1/health")
        .body(Body::empty())
        .unwrap();
    let response = router.oneshot(request).await.unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let bytes = to_bytes(response.into_body(), 1_000_000).await.unwrap();
    let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(json["gear"], "github-mirror");
    assert!(json.get("api_base_url").is_none());
}

#[tokio::test]
async fn init_without_config_section_uses_defaults() {
    let gear = GithubMirrorGear::default();
    let ctx = common::gear_ctx(Arc::new(ClientHub::new()), None).await;

    let result = gear.init(&ctx).await;

    assert!(result.is_ok());
}

#[tokio::test]
async fn second_init_fails_with_already_initialized() {
    let gear = GithubMirrorGear::default();
    let ctx = common::gear_ctx(Arc::new(ClientHub::new()), None).await;

    gear.init(&ctx).await.expect("first init must succeed");
    let second = gear.init(&ctx).await;

    let message = second.expect_err("second init must fail").to_string();
    assert!(message.contains("already initialized"));
}

#[tokio::test]
async fn register_rest_before_init_fails() {
    let gear = GithubMirrorGear::default();
    let ctx = common::gear_ctx(Arc::new(ClientHub::new()), None).await;

    let openapi = OpenApiRegistryImpl::new();
    let result = gear.register_rest(&ctx, Router::new(), &openapi);

    let message = result
        .expect_err("register_rest before init must fail")
        .to_string();
    assert!(message.contains("Service not initialized"));
}

fn token_secret_config(api_base_url: &str) -> serde_json::Value {
    serde_json::json!({
        "config": {
            "api_base_url": api_base_url,
            "github_token_secret": {
                "tenant_id": "00000000-df51-5b42-9538-d2b56b7ee953",
                "key": "github-token"
            }
        }
    })
}

fn hub_with(credstore: MockCredStoreClient) -> Arc<ClientHub> {
    let hub = Arc::new(ClientHub::new());
    hub.register::<dyn CredStoreClientV1>(Arc::new(credstore));
    hub
}

async fn first_github_request(
    credstore: MockCredStoreClient,
    server: &MockServer,
) -> Result<Vec<RateLimitQuota>, DomainError> {
    let gear = GithubMirrorGear::default();
    let hub = hub_with(credstore);
    let ctx = common::gear_ctx(
        Arc::clone(&hub),
        Some(token_secret_config(&server.base_url())),
    )
    .await;
    gear.init(&ctx).await.expect("init must succeed");
    let service = hub.get::<Service>().expect("init registers the service");
    service.rate_limit(&common::caller()).await
}

fn rate_limit_body() -> serde_json::Value {
    serde_json::json!({
        "resources": {
            "core": {"limit": 5000, "used": 1, "remaining": 4999, "reset": 1_700_003_600}
        }
    })
}

#[tokio::test]
async fn the_first_github_request_reads_the_token_from_the_credential_store() {
    let server = MockServer::start_async().await;
    let rate_limit = server
        .mock_async(|when, then| {
            when.method("GET")
                .path("/rate_limit")
                .header("authorization", "Bearer ghp_from_credstore");
            then.status(200).json_body(rate_limit_body());
        })
        .await;
    let credstore = MockCredStoreClient::with_secrets(vec![(
        "github-token".to_owned(),
        "ghp_from_credstore".to_owned(),
    )]);

    first_github_request(credstore, &server)
        .await
        .expect("the request must carry the token from the credential store");
    rate_limit.assert_async().await;
}

#[tokio::test]
async fn the_first_github_request_fails_when_the_configured_secret_is_missing() {
    let server = MockServer::start_async().await;

    let message = first_github_request(MockCredStoreClient::empty(), &server)
        .await
        .expect_err("a configured secret that is not there must fail the request")
        .to_string();
    assert!(message.contains("github-token"), "{message}");
}

#[tokio::test]
async fn the_first_github_request_fails_when_the_credential_store_cannot_be_read() {
    let server = MockServer::start_async().await;

    let message = first_github_request(MockCredStoreClient::always_failing(), &server)
        .await
        .expect_err("a failing credential store must fail the request")
        .to_string();
    assert!(message.contains("credential store"), "{message}");
}

#[tokio::test]
async fn init_fails_when_a_secret_is_configured_but_no_credential_store_is_there() {
    let gear = GithubMirrorGear::default();
    let ctx = common::gear_ctx(
        Arc::new(ClientHub::new()),
        Some(token_secret_config("https://api.github.com")),
    )
    .await;

    let message = gear
        .init(&ctx)
        .await
        .expect_err("a configured secret without a credential store must stop init")
        .to_string();
    assert!(message.contains("credential store"), "{message}");
}

#[tokio::test]
async fn an_empty_token_secret_means_an_unauthenticated_gear() {
    let server = MockServer::start_async().await;
    let rate_limit = server
        .mock_async(|when, then| {
            when.method("GET")
                .path("/rate_limit")
                .header_missing("authorization");
            then.status(200).json_body(rate_limit_body());
        })
        .await;
    let credstore =
        MockCredStoreClient::with_secrets(vec![("github-token".to_owned(), String::new())]);

    first_github_request(credstore, &server)
        .await
        .expect("an empty token must leave the gear unauthenticated, not fail it");
    rate_limit.assert_async().await;
}
