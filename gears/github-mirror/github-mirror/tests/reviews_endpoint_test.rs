#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::sync::Arc;

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use github_mirror::api::rest::routes::{ConcreteService, register_routes};
use github_mirror::domain::repo::{RepoRecord, ReviewRecord};
use toolkit::api::OpenApiRegistryImpl;
use toolkit_security::SecurityContext;
use tower::ServiceExt;
use uuid::Uuid;

fn repo_record() -> RepoRecord {
    RepoRecord {
        node_id: None,
        id: 960,
        owner: "acme".to_owned(),
        name: "widget".to_owned(),
        full_name: "acme/widget".to_owned(),
        default_branch: "main".to_owned(),
        private: false,
        pushed_at: None,
        stars: 0,
        forks: 0,
        description: None,
        clone_url: None,
    }
}

fn review_record(id: i64, pull_number: i64, state: &str) -> ReviewRecord {
    ReviewRecord {
        id,
        repo_id: 0,
        pull_number,
        author_login: Some("erin".to_owned()),
        state: state.to_owned(),
        body: Some("ship it".to_owned()),
        commit_id: Some("h1".to_owned()),
        submitted_at: Some("2026-08-20T00:00:00Z".to_owned()),
        html_url: None,
    }
}

fn router_for(service: Arc<ConcreteService>, ctx: SecurityContext) -> Router {
    let openapi = OpenApiRegistryImpl::new();
    register_routes(Router::new(), &openapi, service).layer(axum::Extension(ctx))
}

async fn body_json(response: axum::http::Response<Body>) -> serde_json::Value {
    let bytes = to_bytes(response.into_body(), 1_000_000).await.unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

async fn get(router: Router, uri: &str) -> axum::http::Response<Body> {
    let request = Request::builder().uri(uri).body(Body::empty()).unwrap();
    router.oneshot(request).await.unwrap()
}

#[tokio::test]
async fn reviews_are_listed_oldest_first_for_their_pull() {
    let ctx = common::caller_in(Uuid::new_v4());
    let service = common::service("https://api.github.com").await;
    service
        .upsert_repo(&ctx, repo_record())
        .await
        .expect("repo seed must succeed");
    service
        .upsert_review(
            &ctx,
            "acme",
            "widget",
            review_record(2, 7, "CHANGES_REQUESTED"),
        )
        .await
        .expect("review seed must succeed");
    service
        .upsert_review(&ctx, "acme", "widget", review_record(1, 7, "COMMENTED"))
        .await
        .expect("review seed must succeed");
    service
        .upsert_review(&ctx, "acme", "widget", review_record(3, 8, "APPROVED"))
        .await
        .expect("review seed must succeed");

    let router = router_for(service, ctx);
    let response = get(router, "/repos/acme/widget/pulls/7/reviews").await;

    assert_eq!(response.status(), StatusCode::OK);
    let json = body_json(response).await;
    let items = json.as_array().expect("items");
    assert_eq!(items.len(), 2, "only pull 7 reviews must be returned");
    assert_eq!(items[0]["id"], 1);
    assert_eq!(items[0]["state"], "COMMENTED");
    assert_eq!(items[1]["id"], 2);
    assert_eq!(items[1]["state"], "CHANGES_REQUESTED");
}

#[tokio::test]
async fn reviews_of_unknown_repository_return_404() {
    let ctx = common::caller_in(Uuid::new_v4());
    let service = common::service("https://api.github.com").await;

    let router = router_for(service, ctx);
    let response = get(router, "/repos/acme/nope/pulls/7/reviews").await;

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn reviews_are_tenant_scoped() {
    let owner = common::caller_in(Uuid::new_v4());
    let db = common::inmem_db().await;
    let service = common::service_over(db.clone(), "https://api.github.com");
    service
        .upsert_repo(&owner, repo_record())
        .await
        .expect("repo seed must succeed");
    service
        .upsert_review(&owner, "acme", "widget", review_record(4, 7, "APPROVED"))
        .await
        .expect("review seed must succeed");

    let stranger = common::caller_in(Uuid::new_v4());
    let router = router_for(service, stranger);
    let response = get(router, "/repos/acme/widget/pulls/7/reviews").await;

    assert_eq!(
        response.status(),
        StatusCode::NOT_FOUND,
        "another tenant must not even learn the repository is mirrored"
    );
}

#[tokio::test]
async fn review_upsert_is_idempotent_by_id() {
    let ctx = common::caller_in(Uuid::new_v4());
    let service = common::service("https://api.github.com").await;
    service
        .upsert_repo(&ctx, repo_record())
        .await
        .expect("repo seed must succeed");
    service
        .upsert_review(&ctx, "acme", "widget", review_record(5, 7, "COMMENTED"))
        .await
        .expect("first upsert must succeed");

    let updated = review_record(5, 7, "APPROVED");
    service
        .upsert_review(&ctx, "acme", "widget", updated)
        .await
        .expect("second upsert must succeed");

    let router = router_for(service, ctx);
    let response = get(router, "/repos/acme/widget/pulls/7/reviews").await;
    let json = body_json(response).await;
    let items = json.as_array().expect("items");
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["state"], "APPROVED");
}
