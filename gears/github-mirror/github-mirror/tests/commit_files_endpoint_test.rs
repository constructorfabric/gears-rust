#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::sync::Arc;

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use github_mirror::api::rest::routes::{ConcreteService, register_routes};
use github_mirror::domain::repo::{CommitFileRecord, RepoRecord};
use toolkit::api::OpenApiRegistryImpl;
use toolkit_security::SecurityContext;
use tower::ServiceExt;
use uuid::Uuid;

fn repo_record() -> RepoRecord {
    RepoRecord {
        node_id: None,
        id: 1002,
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

fn file_record(commit_sha: &str, filename: &str, additions: i64) -> CommitFileRecord {
    CommitFileRecord {
        repo_id: 0,
        commit_sha: commit_sha.to_owned(),
        filename: filename.to_owned(),
        status: "modified".to_owned(),
        additions,
        deletions: 1,
        changes: additions + 1,
        previous_filename: None,
        sha: Some("blob".to_owned()),
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
async fn commit_files_are_listed_by_name_for_their_commit() {
    let ctx = common::caller_in(Uuid::new_v4());
    let service = common::service("https://api.github.com").await;
    service
        .upsert_repo(&ctx, repo_record())
        .await
        .expect("repo seed must succeed");
    service
        .upsert_commit_file(&ctx, "acme", "widget", file_record("aaa", "src/main.rs", 5))
        .await
        .expect("file seed must succeed");
    service
        .upsert_commit_file(&ctx, "acme", "widget", file_record("aaa", "Cargo.toml", 1))
        .await
        .expect("file seed must succeed");
    service
        .upsert_commit_file(&ctx, "acme", "widget", file_record("bbb", "other.rs", 2))
        .await
        .expect("file seed must succeed");

    let router = router_for(service, ctx);
    let response = get(
        router,
        "/github-mirror/v1/repos/acme/widget/commits/aaa/files",
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    let json = body_json(response).await;
    let items = json["items"].as_array().expect("items");
    assert_eq!(items.len(), 2, "only commit aaa files must be returned");
    assert_eq!(items[0]["filename"], "Cargo.toml");
    assert_eq!(items[1]["filename"], "src/main.rs");
    assert_eq!(items[0]["repo_id"], 1002);
}

#[tokio::test]
async fn commit_files_of_unknown_repository_return_404() {
    let ctx = common::caller_in(Uuid::new_v4());
    let service = common::service("https://api.github.com").await;

    let router = router_for(service, ctx);
    let response = get(
        router,
        "/github-mirror/v1/repos/acme/nope/commits/aaa/files",
    )
    .await;

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn commit_files_are_tenant_scoped() {
    let owner = common::caller_in(Uuid::new_v4());
    let db = common::inmem_db().await;
    let service = common::service_over(db.clone(), "https://api.github.com");
    service
        .upsert_repo(&owner, repo_record())
        .await
        .expect("repo seed must succeed");
    service
        .upsert_commit_file(&owner, "acme", "widget", file_record("aaa", "secret.rs", 3))
        .await
        .expect("file seed must succeed");

    let stranger = common::caller_in(Uuid::new_v4());
    let router = router_for(service, stranger);
    let response = get(
        router,
        "/github-mirror/v1/repos/acme/widget/commits/aaa/files",
    )
    .await;

    assert_eq!(
        response.status(),
        StatusCode::NOT_FOUND,
        "another tenant must not even learn the repository is mirrored"
    );
}

#[tokio::test]
async fn commit_file_upsert_is_idempotent_by_filename() {
    let ctx = common::caller_in(Uuid::new_v4());
    let service = common::service("https://api.github.com").await;
    service
        .upsert_repo(&ctx, repo_record())
        .await
        .expect("repo seed must succeed");
    service
        .upsert_commit_file(&ctx, "acme", "widget", file_record("aaa", "src/main.rs", 5))
        .await
        .expect("first upsert must succeed");

    let updated = file_record("aaa", "src/main.rs", 50);
    service
        .upsert_commit_file(&ctx, "acme", "widget", updated)
        .await
        .expect("second upsert must succeed");

    let router = router_for(service, ctx);
    let response = get(
        router,
        "/github-mirror/v1/repos/acme/widget/commits/aaa/files",
    )
    .await;
    let json = body_json(response).await;
    let items = json["items"].as_array().expect("items");
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["additions"], 50);
}
