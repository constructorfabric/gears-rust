#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::sync::Arc;

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use github_mirror::api::rest::routes::{ConcreteService, register_routes};
use github_mirror::domain::repo::{CommitStatusRecord, RepoRecord};
use toolkit::api::OpenApiRegistryImpl;
use toolkit_security::SecurityContext;
use tower::ServiceExt;
use uuid::Uuid;

fn repo_record() -> RepoRecord {
    RepoRecord {
        node_id: None,
        id: 8001,
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

fn status_record(id: i64, commit_sha: &str, context: &str, created_at: &str) -> CommitStatusRecord {
    CommitStatusRecord {
        id,
        repo_id: 0,
        commit_sha: commit_sha.to_owned(),
        state: "success".to_owned(),
        context: context.to_owned(),
        description: Some("build passed".to_owned()),
        target_url: Some("https://ci.example.com/1".to_owned()),
        creator_login: Some("judy".to_owned()),
        created_at: created_at.to_owned(),
        updated_at: created_at.to_owned(),
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
async fn commit_statuses_are_listed_newest_first_for_their_commit() {
    let ctx = common::caller_in(Uuid::new_v4());
    let service = common::service("https://api.github.com").await;
    service
        .upsert_repo(&ctx, repo_record())
        .await
        .expect("repo seed must succeed");
    service
        .upsert_commit_status(
            &ctx,
            "acme",
            "widget",
            status_record(1, "aaa", "ci/build", "2026-08-18T00:00:00Z"),
        )
        .await
        .expect("status seed must succeed");
    service
        .upsert_commit_status(
            &ctx,
            "acme",
            "widget",
            status_record(2, "aaa", "ci/lint", "2026-08-20T00:00:00Z"),
        )
        .await
        .expect("status seed must succeed");
    service
        .upsert_commit_status(
            &ctx,
            "acme",
            "widget",
            status_record(3, "bbb", "ci/build", "2026-08-19T00:00:00Z"),
        )
        .await
        .expect("status seed must succeed");

    let router = router_for(service, ctx);
    let response = get(router, "/repos/acme/widget/commits/aaa/statuses").await;

    assert_eq!(response.status(), StatusCode::OK);
    let json = body_json(response).await;
    let items = json.as_array().expect("items");
    assert_eq!(items.len(), 2, "only commit aaa statuses must be returned");
    assert_eq!(items[0]["context"], "ci/lint");
    assert_eq!(items[1]["context"], "ci/build");
    assert_eq!(items[0]["state"], "success");
    assert_eq!(items[0]["creator"]["login"], "judy");
}

#[tokio::test]
async fn commit_statuses_of_unknown_repository_return_404() {
    let ctx = common::caller_in(Uuid::new_v4());
    let service = common::service("https://api.github.com").await;

    let router = router_for(service, ctx);
    let response = get(router, "/repos/acme/nope/commits/aaa/statuses").await;

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn commit_statuses_are_tenant_scoped() {
    let owner = common::caller_in(Uuid::new_v4());
    let db = common::inmem_db().await;
    let service = common::service_over(db.clone(), "https://api.github.com");
    service
        .upsert_repo(&owner, repo_record())
        .await
        .expect("repo seed must succeed");
    service
        .upsert_commit_status(
            &owner,
            "acme",
            "widget",
            status_record(4, "aaa", "ci/secret", "2026-08-20T00:00:00Z"),
        )
        .await
        .expect("status seed must succeed");

    let stranger = common::caller_in(Uuid::new_v4());
    let router = router_for(service, stranger);
    let response = get(router, "/repos/acme/widget/commits/aaa/statuses").await;

    assert_eq!(
        response.status(),
        StatusCode::NOT_FOUND,
        "another tenant must not even learn the repository is mirrored"
    );
}

#[tokio::test]
async fn commit_status_upsert_is_idempotent_by_id() {
    let ctx = common::caller_in(Uuid::new_v4());
    let service = common::service("https://api.github.com").await;
    service
        .upsert_repo(&ctx, repo_record())
        .await
        .expect("repo seed must succeed");
    service
        .upsert_commit_status(
            &ctx,
            "acme",
            "widget",
            status_record(5, "aaa", "ci/build", "2026-08-20T00:00:00Z"),
        )
        .await
        .expect("first upsert must succeed");

    let mut updated = status_record(5, "aaa", "ci/build", "2026-08-20T00:00:00Z");
    updated.state = "failure".to_owned();
    updated.description = Some("build broke".to_owned());
    service
        .upsert_commit_status(&ctx, "acme", "widget", updated)
        .await
        .expect("second upsert must succeed");

    let router = router_for(service, ctx);
    let response = get(router, "/repos/acme/widget/commits/aaa/statuses").await;
    let json = body_json(response).await;
    let items = json.as_array().expect("items");
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["state"], "failure");
    assert_eq!(items[0]["description"], "build broke");
}
