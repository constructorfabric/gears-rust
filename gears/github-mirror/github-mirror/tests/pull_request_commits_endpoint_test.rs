#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::sync::Arc;

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use github_mirror::api::rest::routes::{ConcreteService, register_routes};
use github_mirror::domain::repo::{PullRequestCommitRecord, RepoRecord};
use toolkit::api::OpenApiRegistryImpl;
use toolkit_security::SecurityContext;
use tower::ServiceExt;
use uuid::Uuid;

fn repo_record() -> RepoRecord {
    RepoRecord {
        node_id: None,
        id: 7001,
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

fn commit_record(
    pull_number: i64,
    sha: &str,
    message: &str,
    committed_at: &str,
) -> PullRequestCommitRecord {
    PullRequestCommitRecord {
        repo_id: 0,
        pull_number,
        sha: sha.to_owned(),
        message: message.to_owned(),
        author_login: Some("ivan".to_owned()),
        committer_login: Some("ivan".to_owned()),
        authored_at: Some(committed_at.to_owned()),
        committed_at: Some(committed_at.to_owned()),
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
async fn pull_request_commits_are_listed_oldest_first_for_their_pull() {
    let ctx = common::caller_in(Uuid::new_v4());
    let service = common::service("https://api.github.com").await;
    service
        .upsert_repo(&ctx, repo_record())
        .await
        .expect("repo seed must succeed");
    service
        .upsert_pull_request_commit(
            &ctx,
            "acme",
            "widget",
            commit_record(7, "bbb", "second", "2026-08-20T00:00:00Z"),
        )
        .await
        .expect("commit seed must succeed");
    service
        .upsert_pull_request_commit(
            &ctx,
            "acme",
            "widget",
            commit_record(7, "aaa", "first", "2026-08-18T00:00:00Z"),
        )
        .await
        .expect("commit seed must succeed");
    service
        .upsert_pull_request_commit(
            &ctx,
            "acme",
            "widget",
            commit_record(8, "ccc", "other pull", "2026-08-19T00:00:00Z"),
        )
        .await
        .expect("commit seed must succeed");

    let router = router_for(service, ctx);
    let response = get(router, "/repos/acme/widget/pulls/7/commits").await;

    assert_eq!(response.status(), StatusCode::OK);
    let json = body_json(response).await;
    let items = json.as_array().expect("items");
    assert_eq!(items.len(), 2, "only pull 7 commits must be returned");
    assert_eq!(items[0]["sha"], "aaa");
    assert_eq!(items[0]["commit"]["message"], "first");
    assert_eq!(items[0]["author"]["login"], "ivan");
    assert_eq!(items[1]["sha"], "bbb");
}

#[tokio::test]
async fn pull_request_commits_of_unknown_repository_return_404() {
    let ctx = common::caller_in(Uuid::new_v4());
    let service = common::service("https://api.github.com").await;

    let router = router_for(service, ctx);
    let response = get(router, "/repos/acme/nope/pulls/7/commits").await;

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn pull_request_commits_are_tenant_scoped() {
    let owner = common::caller_in(Uuid::new_v4());
    let db = common::inmem_db().await;
    let service = common::service_over(db.clone(), "https://api.github.com");
    service
        .upsert_repo(&owner, repo_record())
        .await
        .expect("repo seed must succeed");
    service
        .upsert_pull_request_commit(
            &owner,
            "acme",
            "widget",
            commit_record(7, "ddd", "secret", "2026-08-20T00:00:00Z"),
        )
        .await
        .expect("commit seed must succeed");

    let stranger = common::caller_in(Uuid::new_v4());
    let router = router_for(service, stranger);
    let response = get(router, "/repos/acme/widget/pulls/7/commits").await;

    assert_eq!(
        response.status(),
        StatusCode::NOT_FOUND,
        "another tenant must not even learn the repository is mirrored"
    );
}

#[tokio::test]
async fn pull_request_commit_upsert_is_idempotent_by_sha() {
    let ctx = common::caller_in(Uuid::new_v4());
    let service = common::service("https://api.github.com").await;
    service
        .upsert_repo(&ctx, repo_record())
        .await
        .expect("repo seed must succeed");
    service
        .upsert_pull_request_commit(
            &ctx,
            "acme",
            "widget",
            commit_record(7, "aaa", "first", "2026-08-20T00:00:00Z"),
        )
        .await
        .expect("first upsert must succeed");

    let updated = commit_record(7, "aaa", "amended", "2026-08-20T00:00:00Z");
    service
        .upsert_pull_request_commit(&ctx, "acme", "widget", updated)
        .await
        .expect("second upsert must succeed");

    let router = router_for(service, ctx);
    let response = get(router, "/repos/acme/widget/pulls/7/commits").await;
    let json = body_json(response).await;
    let items = json.as_array().expect("items");
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["commit"]["message"], "amended");
}
