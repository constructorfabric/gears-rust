#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::sync::Arc;

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use github_mirror::api::rest::routes::{ConcreteService, register_routes};
use github_mirror::domain::repo::{ReleaseRecord, RepositoryRecord};
use toolkit::api::OpenApiRegistryImpl;
use toolkit_security::SecurityContext;
use tower::ServiceExt;
use uuid::Uuid;

fn repo_record() -> RepositoryRecord {
    RepositoryRecord {
        id: 990,
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

fn release_record(id: i64, tag_name: &str, created_at: &str) -> ReleaseRecord {
    ReleaseRecord {
        id,
        repo_id: 0,
        tag_name: tag_name.to_owned(),
        name: Some(tag_name.to_owned()),
        draft: false,
        prerelease: false,
        body: Some("changelog".to_owned()),
        author_login: Some("erin".to_owned()),
        created_at: created_at.to_owned(),
        published_at: Some(created_at.to_owned()),
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
async fn releases_are_listed_newest_first() {
    let ctx = common::caller_in(Uuid::new_v4());
    let service = common::service("https://api.github.com").await;
    service
        .upsert_repository(&ctx, repo_record())
        .await
        .expect("repo seed must succeed");
    service
        .upsert_release(
            &ctx,
            "acme",
            "widget",
            release_record(1, "v1.0.0", "2026-08-18T00:00:00Z"),
        )
        .await
        .expect("release seed must succeed");
    service
        .upsert_release(
            &ctx,
            "acme",
            "widget",
            release_record(2, "v1.1.0", "2026-08-20T00:00:00Z"),
        )
        .await
        .expect("release seed must succeed");

    let router = router_for(service, ctx);
    let response = get(router, "/github-mirror/v1/repos/acme/widget/releases").await;

    assert_eq!(response.status(), StatusCode::OK);
    let json = body_json(response).await;
    let items = json["items"].as_array().expect("items");
    assert_eq!(items.len(), 2);
    assert_eq!(items[0]["tag_name"], "v1.1.0");
    assert_eq!(items[1]["tag_name"], "v1.0.0");
    assert_eq!(items[0]["repo_id"], 990);
}

#[tokio::test]
async fn releases_of_unknown_repository_return_404() {
    let ctx = common::caller_in(Uuid::new_v4());
    let service = common::service("https://api.github.com").await;

    let router = router_for(service, ctx);
    let response = get(router, "/github-mirror/v1/repos/acme/nope/releases").await;

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn releases_are_tenant_scoped() {
    let owner = common::caller_in(Uuid::new_v4());
    let db = common::inmem_db().await;
    let service = common::service_over(db.clone(), "https://api.github.com");
    service
        .upsert_repository(&owner, repo_record())
        .await
        .expect("repo seed must succeed");
    service
        .upsert_release(
            &owner,
            "acme",
            "widget",
            release_record(3, "v9.9.9", "2026-08-20T00:00:00Z"),
        )
        .await
        .expect("release seed must succeed");

    let stranger = common::caller_in(Uuid::new_v4());
    let router = router_for(service, stranger);
    let response = get(router, "/github-mirror/v1/repos/acme/widget/releases").await;

    assert_eq!(
        response.status(),
        StatusCode::NOT_FOUND,
        "another tenant must not even learn the repository is mirrored"
    );
}

#[tokio::test]
async fn release_upsert_is_idempotent_by_id() {
    let ctx = common::caller_in(Uuid::new_v4());
    let service = common::service("https://api.github.com").await;
    service
        .upsert_repository(&ctx, repo_record())
        .await
        .expect("repo seed must succeed");
    service
        .upsert_release(
            &ctx,
            "acme",
            "widget",
            release_record(4, "v2.0.0", "2026-08-20T00:00:00Z"),
        )
        .await
        .expect("first upsert must succeed");

    let mut updated = release_record(4, "v2.0.0", "2026-08-20T00:00:00Z");
    updated.prerelease = true;
    updated.body = Some("amended".to_owned());
    service
        .upsert_release(&ctx, "acme", "widget", updated)
        .await
        .expect("second upsert must succeed");

    let router = router_for(service, ctx);
    let response = get(router, "/github-mirror/v1/repos/acme/widget/releases").await;
    let json = body_json(response).await;
    let items = json["items"].as_array().expect("items");
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["prerelease"], true);
    assert_eq!(items[0]["body"], "amended");
}
