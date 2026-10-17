#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::sync::Arc;

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use credstore_sdk::CredStoreClientV1;
use credstore_sdk::test_util::MockCredStoreClient;
use github_mirror::GithubMirrorGear;
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

fn token_secret_config() -> serde_json::Value {
    serde_json::json!({
        "config": {
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

#[tokio::test]
async fn init_reads_the_github_token_from_the_credential_store() {
    let gear = GithubMirrorGear::default();
    let hub = hub_with(MockCredStoreClient::with_secrets(vec![(
        "github-token".to_owned(),
        "ghp_from_credstore".to_owned(),
    )]));
    let ctx = common::gear_ctx(hub, Some(token_secret_config())).await;

    gear.init(&ctx)
        .await
        .expect("init must succeed when the secret is there");
}

#[tokio::test]
async fn init_fails_when_the_configured_secret_is_missing() {
    let gear = GithubMirrorGear::default();
    let ctx = common::gear_ctx(
        hub_with(MockCredStoreClient::empty()),
        Some(token_secret_config()),
    )
    .await;

    let message = gear
        .init(&ctx)
        .await
        .expect_err("a configured secret that is not there must stop init")
        .to_string();
    assert!(message.contains("github-token"), "{message}");
}

#[tokio::test]
async fn init_fails_when_the_credential_store_cannot_be_read() {
    let gear = GithubMirrorGear::default();
    let ctx = common::gear_ctx(
        hub_with(MockCredStoreClient::always_failing()),
        Some(token_secret_config()),
    )
    .await;

    let message = gear
        .init(&ctx)
        .await
        .expect_err("a failing credential store must stop init")
        .to_string();
    assert!(message.contains("credential store"), "{message}");
}

#[tokio::test]
async fn init_fails_when_a_secret_is_configured_but_no_credential_store_is_there() {
    let gear = GithubMirrorGear::default();
    let ctx = common::gear_ctx(Arc::new(ClientHub::new()), Some(token_secret_config())).await;

    let message = gear
        .init(&ctx)
        .await
        .expect_err("a configured secret without a credential store must stop init")
        .to_string();
    assert!(message.contains("credential store"), "{message}");
}

#[tokio::test]
async fn an_empty_token_secret_means_an_unauthenticated_gear() {
    let gear = GithubMirrorGear::default();
    let hub = hub_with(MockCredStoreClient::with_secrets(vec![(
        "github-token".to_owned(),
        String::new(),
    )]));
    let ctx = common::gear_ctx(hub, Some(token_secret_config())).await;

    gear.init(&ctx)
        .await
        .expect("an empty token must start the gear unauthenticated, not fail it");
}
