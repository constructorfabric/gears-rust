#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::sync::Arc;

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use github_mirror::api::rest::routes::{ConcreteService, register_routes};
use github_mirror::domain::repo::{DeploymentRecord, RepoRecord};
use toolkit::api::OpenApiRegistryImpl;
use toolkit_security::SecurityContext;
use tower::ServiceExt;
use uuid::Uuid;

fn repo_record() -> RepoRecord {
    RepoRecord {
        node_id: None,
        id: 6001,
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

fn deployment_record(id: i64, environment: &str, created_at: &str) -> DeploymentRecord {
    DeploymentRecord {
        id,
        repo_id: 0,
        git_ref: "main".to_owned(),
        sha: "aaa".to_owned(),
        environment: environment.to_owned(),
        task: "deploy".to_owned(),
        description: Some("ship".to_owned()),
        creator_login: Some("heidi".to_owned()),
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
async fn deployments_are_listed_newest_first() {
    let ctx = common::caller_in(Uuid::new_v4());
    let service = common::service("https://api.github.com").await;
    service
        .upsert_repo(&ctx, repo_record())
        .await
        .expect("repo seed must succeed");
    service
        .upsert_deployment(
            &ctx,
            "acme",
            "widget",
            deployment_record(1, "staging", "2026-08-18T00:00:00Z"),
        )
        .await
        .expect("deployment seed must succeed");
    service
        .upsert_deployment(
            &ctx,
            "acme",
            "widget",
            deployment_record(2, "production", "2026-08-20T00:00:00Z"),
        )
        .await
        .expect("deployment seed must succeed");

    let router = router_for(service, ctx);
    let response = get(router, "/repos/acme/widget/deployments").await;

    assert_eq!(response.status(), StatusCode::OK);
    let json = body_json(response).await;
    let items = json.as_array().expect("items");
    assert_eq!(items.len(), 2);
    assert_eq!(items[0]["environment"], "production");
    assert_eq!(items[1]["environment"], "staging");
    assert_eq!(items[0]["ref"], "main");
    assert_eq!(items[0]["creator"]["login"], "heidi");
}

#[tokio::test]
async fn deployments_of_unknown_repository_return_404() {
    let ctx = common::caller_in(Uuid::new_v4());
    let service = common::service("https://api.github.com").await;

    let router = router_for(service, ctx);
    let response = get(router, "/repos/acme/nope/deployments").await;

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn deployments_are_tenant_scoped() {
    let owner = common::caller_in(Uuid::new_v4());
    let db = common::inmem_db().await;
    let service = common::service_over(db.clone(), "https://api.github.com");
    service
        .upsert_repo(&owner, repo_record())
        .await
        .expect("repo seed must succeed");
    service
        .upsert_deployment(
            &owner,
            "acme",
            "widget",
            deployment_record(3, "production", "2026-08-20T00:00:00Z"),
        )
        .await
        .expect("deployment seed must succeed");

    let stranger = common::caller_in(Uuid::new_v4());
    let router = router_for(service, stranger);
    let response = get(router, "/repos/acme/widget/deployments").await;

    assert_eq!(
        response.status(),
        StatusCode::NOT_FOUND,
        "another tenant must not even learn the repository is mirrored"
    );
}

#[tokio::test]
async fn deployment_upsert_is_idempotent_by_id() {
    let ctx = common::caller_in(Uuid::new_v4());
    let service = common::service("https://api.github.com").await;
    service
        .upsert_repo(&ctx, repo_record())
        .await
        .expect("repo seed must succeed");
    service
        .upsert_deployment(
            &ctx,
            "acme",
            "widget",
            deployment_record(4, "staging", "2026-08-20T00:00:00Z"),
        )
        .await
        .expect("first upsert must succeed");

    let mut updated = deployment_record(4, "production", "2026-08-20T00:00:00Z");
    updated.description = Some("promoted".to_owned());
    service
        .upsert_deployment(&ctx, "acme", "widget", updated)
        .await
        .expect("second upsert must succeed");

    let router = router_for(service, ctx);
    let response = get(router, "/repos/acme/widget/deployments").await;
    let json = body_json(response).await;
    let items = json.as_array().expect("items");
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["environment"], "production");
    assert_eq!(items[0]["description"], "promoted");
}
