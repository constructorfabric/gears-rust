#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::sync::Arc;

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use github_mirror::api::rest::routes::{ConcreteService, register_routes};
use github_mirror::domain::repo::{RepoRecord, ReviewThreadRecord};
use toolkit::api::OpenApiRegistryImpl;
use toolkit_security::SecurityContext;
use tower::ServiceExt;
use uuid::Uuid;

fn repo_record() -> RepoRecord {
    RepoRecord {
        node_id: None,
        id: 1003,
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

fn thread_record(id: &str, pull_number: i64, is_resolved: bool) -> ReviewThreadRecord {
    ReviewThreadRecord {
        id: id.to_owned(),
        repo_id: 0,
        pull_number,
        is_resolved,
        is_outdated: false,
        path: Some("src/lib.rs".to_owned()),
        line: Some(10),
        resolved_by: is_resolved.then(|| "erin".to_owned()),
        comments_count: 2,
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
async fn review_threads_are_listed_for_their_pull() {
    let ctx = common::caller_in(Uuid::new_v4());
    let service = common::service("https://api.github.com").await;
    service
        .upsert_repo(&ctx, repo_record())
        .await
        .expect("repo seed must succeed");
    service
        .upsert_review_thread(&ctx, "acme", "widget", thread_record("PRRT_a", 7, true))
        .await
        .expect("thread seed must succeed");
    service
        .upsert_review_thread(&ctx, "acme", "widget", thread_record("PRRT_b", 7, false))
        .await
        .expect("thread seed must succeed");
    service
        .upsert_review_thread(&ctx, "acme", "widget", thread_record("PRRT_c", 8, true))
        .await
        .expect("thread seed must succeed");

    let router = router_for(service, ctx);
    let response = get(
        router,
        "/github-mirror/v1/repos/acme/widget/pulls/7/threads",
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    let json = body_json(response).await;
    let items = json["items"].as_array().expect("items");
    assert_eq!(items.len(), 2, "only pull 7 threads must be returned");
    assert_eq!(items[0]["id"], "PRRT_a");
    assert_eq!(items[0]["is_resolved"], true);
    assert_eq!(items[0]["resolved_by"], "erin");
    assert_eq!(items[1]["id"], "PRRT_b");
    assert_eq!(items[0]["repo_id"], 1003);
}

#[tokio::test]
async fn review_threads_of_unknown_repository_return_404() {
    let ctx = common::caller_in(Uuid::new_v4());
    let service = common::service("https://api.github.com").await;

    let router = router_for(service, ctx);
    let response = get(router, "/github-mirror/v1/repos/acme/nope/pulls/7/threads").await;

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn review_threads_are_tenant_scoped() {
    let owner = common::caller_in(Uuid::new_v4());
    let db = common::inmem_db().await;
    let service = common::service_over(db.clone(), "https://api.github.com");
    service
        .upsert_repo(&owner, repo_record())
        .await
        .expect("repo seed must succeed");
    service
        .upsert_review_thread(&owner, "acme", "widget", thread_record("PRRT_d", 7, false))
        .await
        .expect("thread seed must succeed");

    let stranger = common::caller_in(Uuid::new_v4());
    let router = router_for(service, stranger);
    let response = get(
        router,
        "/github-mirror/v1/repos/acme/widget/pulls/7/threads",
    )
    .await;

    assert_eq!(
        response.status(),
        StatusCode::NOT_FOUND,
        "another tenant must not even learn the repository is mirrored"
    );
}

#[tokio::test]
async fn review_thread_upsert_is_idempotent_by_id() {
    let ctx = common::caller_in(Uuid::new_v4());
    let service = common::service("https://api.github.com").await;
    service
        .upsert_repo(&ctx, repo_record())
        .await
        .expect("repo seed must succeed");
    service
        .upsert_review_thread(&ctx, "acme", "widget", thread_record("PRRT_e", 7, false))
        .await
        .expect("first upsert must succeed");

    let updated = thread_record("PRRT_e", 7, true);
    service
        .upsert_review_thread(&ctx, "acme", "widget", updated)
        .await
        .expect("second upsert must succeed");

    let router = router_for(service, ctx);
    let response = get(
        router,
        "/github-mirror/v1/repos/acme/widget/pulls/7/threads",
    )
    .await;
    let json = body_json(response).await;
    let items = json["items"].as_array().expect("items");
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["is_resolved"], true);
    assert_eq!(items[0]["resolved_by"], "erin");
}
