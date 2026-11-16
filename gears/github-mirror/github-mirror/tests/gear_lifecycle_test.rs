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
use github_mirror::domain::ports::token_vault::vault_secret_name;
use github_mirror::domain::repo::{TokenSecretRecord, TokenSecretRepository};
use github_mirror::domain::service::Service;
use github_mirror::infra::github::rate_limit::TokenFingerprint;
use github_mirror::infra::storage::sea_orm_repo::SeaOrmTokenSecretRepository;
use httpmock::MockServer;
use toolkit::api::OpenApiRegistryImpl;
use toolkit_security::AccessScope;
use toolkit::{ClientHub, Gear, RestApiCapability};
use tower::ServiceExt;
use uuid::Uuid;

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
            "api_base_url": api_base_url
        }
    })
}

const TOKEN_TENANT_ID: Uuid = uuid::uuid!("00000000-df51-5b42-9538-d2b56b7ee953");

async fn seed_token(ctx: &toolkit::GearCtx, token: &str) -> String {
    let fingerprint = TokenFingerprint::of(token).full().to_owned();
    let record = TokenSecretRecord {
        fingerprint: fingerprint.clone(),
        secret_key: vault_secret_name(&fingerprint),
        scope_note: None,
    };
    let repo = SeaOrmTokenSecretRepository::new(Arc::new(
        ctx.db_required().expect("the gear test has a database"),
    ));
    repo.add_many(
        &AccessScope::for_tenant(TOKEN_TENANT_ID),
        TOKEN_TENANT_ID,
        &[record],
    )
    .await
    .expect("the tenant token reference must be saved");
    fingerprint
}

fn hub_with(credstore: MockCredStoreClient) -> Arc<ClientHub> {
    let hub = Arc::new(ClientHub::new());
    hub.register::<dyn CredStoreClientV1>(Arc::new(credstore));
    hub
}

async fn first_github_request(
    credstore: MockCredStoreClient,
    server: &MockServer,
    token: Option<&str>,
) -> Result<Vec<RateLimitQuota>, DomainError> {
    let gear = GithubMirrorGear::default();
    let hub = hub_with(credstore);
    let ctx = common::gear_ctx(Arc::clone(&hub), Some(token_secret_config(&server.base_url()))).await;
    if let Some(token) = token {
        seed_token(&ctx, token).await;
    }
    gear.init(&ctx).await.expect("init must succeed");
    let service = hub.get::<Service>().expect("init registers the service");
    service.rate_limit(&common::caller_in(TOKEN_TENANT_ID)).await
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
    let token = "ghp_from_credstore";
    let server = MockServer::start_async().await;
    let rate_limit = server
        .mock_async(|when, then| {
            when.method("GET")
                .path("/rate_limit")
                .header("authorization", format!("Bearer {token}"));
            then.status(200).json_body(rate_limit_body());
        })
        .await;
    let credstore = MockCredStoreClient::with_secrets(vec![(
        vault_secret_name(TokenFingerprint::of(token).full()),
        token.to_owned(),
    )]);

    first_github_request(credstore, &server, Some(token))
        .await
        .expect("the request must carry the token from the credential store");
    rate_limit.assert_async().await;
}

#[tokio::test]
async fn a_missing_credstore_secret_is_skipped_and_public_requests_remain_unauthenticated() {
    let server = MockServer::start_async().await;
    let rate_limit = server
        .mock_async(|when, then| {
            when.method("GET")
                .path("/rate_limit")
                .header_missing("authorization");
            then.status(200).json_body(rate_limit_body());
        })
        .await;

    first_github_request(MockCredStoreClient::empty(), &server, Some("ghp_missing"))
        .await
        .expect("a missing token must not block public unauthenticated requests");
    rate_limit.assert_async().await;
}

#[tokio::test]
async fn an_unreadable_credstore_secret_is_skipped_for_public_requests() {
    let server = MockServer::start_async().await;
    let rate_limit = server
        .mock_async(|when, then| {
            when.method("GET")
                .path("/rate_limit")
                .header_missing("authorization");
            then.status(200).json_body(rate_limit_body());
        })
        .await;

    first_github_request(
        MockCredStoreClient::always_failing(),
        &server,
        Some("ghp_unreadable"),
    )
        .await
        .expect("a failed token lookup must leave public requests unauthenticated");
    rate_limit.assert_async().await;
}

#[tokio::test]
async fn an_empty_database_pool_starts_without_a_credential_store() {
    let server = MockServer::start_async().await;
    let rate_limit = server
        .mock_async(|when, then| {
            when.method("GET")
                .path("/rate_limit")
                .header_missing("authorization");
            then.status(200).json_body(rate_limit_body());
        })
        .await;
    first_github_request(MockCredStoreClient::empty(), &server, None)
        .await
        .expect("an empty token must leave the gear unauthenticated, not fail it");
    rate_limit.assert_async().await;
}
