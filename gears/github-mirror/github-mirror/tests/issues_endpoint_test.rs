#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::sync::Arc;

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use github_mirror::api::rest::routes::{ConcreteService, register_routes};
use github_mirror::domain::repo::{IssueRecord, RepositoryRecord};
use toolkit::api::OpenApiRegistryImpl;
use toolkit_security::SecurityContext;
use tower::ServiceExt;
use uuid::Uuid;

fn repo_record() -> RepositoryRecord {
    RepositoryRecord {
        id: 900,
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

fn issue_record(id: i64, number: i64, title: &str) -> IssueRecord {
    IssueRecord {
        id,
        repo_id: 0,
        number,
        title: title.to_owned(),
        body: Some("text".to_owned()),
        state: "open".to_owned(),
        is_pull_request: false,
        created_at: "2026-08-20T00:00:00Z".to_owned(),
        updated_at: "2026-08-20T00:00:00Z".to_owned(),
        closed_at: None,
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
async fn issues_are_listed_for_a_mirrored_repository() {
    let ctx = common::caller_in(Uuid::new_v4());
    let service = common::service("https://api.github.com").await;
    service
        .upsert_repository(&ctx, repo_record())
        .await
        .expect("repo seed must succeed");
    service
        .upsert_issue(&ctx, "acme", "widget", issue_record(1, 11, "first"))
        .await
        .expect("issue seed must succeed");
    service
        .upsert_issue(&ctx, "acme", "widget", issue_record(2, 12, "second"))
        .await
        .expect("issue seed must succeed");

    let router = router_for(service, ctx);
    let response = get(router, "/github-mirror/v1/repos/acme/widget/issues").await;

    assert_eq!(response.status(), StatusCode::OK);
    let json = body_json(response).await;
    let items = json["items"].as_array().expect("items");
    assert_eq!(items.len(), 2);
    assert_eq!(items[0]["number"], 11);
    assert_eq!(items[0]["title"], "first");
    assert_eq!(items[0]["repo_id"], 900);
}

#[tokio::test]
async fn issues_of_unknown_repository_return_404() {
    let ctx = common::caller_in(Uuid::new_v4());
    let service = common::service("https://api.github.com").await;

    let router = router_for(service, ctx);
    let response = get(router, "/github-mirror/v1/repos/acme/nope/issues").await;

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn issues_are_tenant_scoped() {
    let owner = common::caller_in(Uuid::new_v4());
    let db = common::inmem_db().await;
    let service = common::service_over(db.clone(), "https://api.github.com");
    service
        .upsert_repository(&owner, repo_record())
        .await
        .expect("repo seed must succeed");
    service
        .upsert_issue(&owner, "acme", "widget", issue_record(1, 11, "secret"))
        .await
        .expect("issue seed must succeed");

    let stranger = common::caller_in(Uuid::new_v4());
    let router = router_for(service, stranger);
    let response = get(router, "/github-mirror/v1/repos/acme/widget/issues").await;

    assert_eq!(
        response.status(),
        StatusCode::NOT_FOUND,
        "another tenant must not even learn the repository is mirrored"
    );
}
