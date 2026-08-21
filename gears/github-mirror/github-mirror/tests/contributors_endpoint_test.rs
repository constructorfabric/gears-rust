#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::sync::Arc;

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use github_mirror::api::rest::routes::{ConcreteService, register_routes};
use github_mirror::domain::repo::{ContributorRecord, RepositoryRecord};
use toolkit::api::OpenApiRegistryImpl;
use toolkit_security::SecurityContext;
use tower::ServiceExt;
use uuid::Uuid;

fn repo_record() -> RepositoryRecord {
    RepositoryRecord {
        id: 997,
        owner: "acme".to_owned(),
        name: "widget".to_owned(),
        full_name: "acme/widget".to_owned(),
        default_branch: "main".to_owned(),
        private: false,
        pushed_at: None,
        stars: 0,
        forks: 0,
        description: None,
    }
}

fn contributor_record(user_id: i64, login: &str, contributions: i64) -> ContributorRecord {
    ContributorRecord {
        repo_id: 0,
        user_id,
        login: login.to_owned(),
        contributions,
        user_type: "User".to_owned(),
        avatar_url: None,
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
async fn contributors_are_listed_most_contributions_first() {
    let ctx = common::caller_in(Uuid::new_v4());
    let service = common::service("https://api.github.com").await;
    service
        .upsert_repository(&ctx, repo_record())
        .await
        .expect("repo seed must succeed");
    service
        .upsert_contributor(&ctx, "acme", "widget", contributor_record(1, "bob", 5))
        .await
        .expect("contributor seed must succeed");
    service
        .upsert_contributor(&ctx, "acme", "widget", contributor_record(2, "alice", 120))
        .await
        .expect("contributor seed must succeed");

    let router = router_for(service, ctx);
    let response = get(router, "/github-mirror/v1/repos/acme/widget/contributors").await;

    assert_eq!(response.status(), StatusCode::OK);
    let json = body_json(response).await;
    let items = json["items"].as_array().expect("items");
    assert_eq!(items.len(), 2);
    assert_eq!(items[0]["login"], "alice");
    assert_eq!(items[0]["contributions"], 120);
    assert_eq!(items[1]["login"], "bob");
    assert_eq!(items[0]["repo_id"], 997);
}

#[tokio::test]
async fn contributors_of_unknown_repository_return_404() {
    let ctx = common::caller_in(Uuid::new_v4());
    let service = common::service("https://api.github.com").await;

    let router = router_for(service, ctx);
    let response = get(router, "/github-mirror/v1/repos/acme/nope/contributors").await;

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn contributors_are_tenant_scoped() {
    let owner = common::caller_in(Uuid::new_v4());
    let db = common::inmem_db().await;
    let service = common::service_over(db.clone(), "https://api.github.com");
    service
        .upsert_repository(&owner, repo_record())
        .await
        .expect("repo seed must succeed");
    service
        .upsert_contributor(&owner, "acme", "widget", contributor_record(3, "carol", 9))
        .await
        .expect("contributor seed must succeed");

    let stranger = common::caller_in(Uuid::new_v4());
    let router = router_for(service, stranger);
    let response = get(router, "/github-mirror/v1/repos/acme/widget/contributors").await;

    assert_eq!(
        response.status(),
        StatusCode::NOT_FOUND,
        "another tenant must not even learn the repository is mirrored"
    );
}

#[tokio::test]
async fn contributor_upsert_is_idempotent_by_user() {
    let ctx = common::caller_in(Uuid::new_v4());
    let service = common::service("https://api.github.com").await;
    service
        .upsert_repository(&ctx, repo_record())
        .await
        .expect("repo seed must succeed");
    service
        .upsert_contributor(&ctx, "acme", "widget", contributor_record(4, "dave", 10))
        .await
        .expect("first upsert must succeed");

    let updated = contributor_record(4, "dave", 25);
    service
        .upsert_contributor(&ctx, "acme", "widget", updated)
        .await
        .expect("second upsert must succeed");

    let router = router_for(service, ctx);
    let response = get(router, "/github-mirror/v1/repos/acme/widget/contributors").await;
    let json = body_json(response).await;
    let items = json["items"].as_array().expect("items");
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["contributions"], 25);
}
