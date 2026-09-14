#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::sync::Arc;

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use github_mirror::api::rest::routes::{ConcreteService, register_routes};
use github_mirror::domain::repo::{IssueReactionRecord, RepoRecord};
use toolkit::api::OpenApiRegistryImpl;
use toolkit_security::SecurityContext;
use tower::ServiceExt;
use uuid::Uuid;

fn repo_record() -> RepoRecord {
    RepoRecord {
        node_id: None,
        id: 9101,
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

fn reaction_record(id: i64, issue_number: i64, content: &str) -> IssueReactionRecord {
    IssueReactionRecord {
        id,
        repo_id: 0,
        issue_number,
        content: content.to_owned(),
        user_login: Some("kate".to_owned()),
        created_at: "2026-08-20T00:00:00Z".to_owned(),
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
async fn issue_reactions_are_listed_for_their_issue() {
    let ctx = common::caller_in(Uuid::new_v4());
    let service = common::service("https://api.github.com").await;
    service
        .upsert_repo(&ctx, repo_record())
        .await
        .expect("repo seed must succeed");
    for reaction in [
        reaction_record(2, 7, "heart"),
        reaction_record(1, 7, "+1"),
        reaction_record(3, 8, "eyes"),
    ] {
        service
            .upsert_issue_reaction(&ctx, "acme", "widget", reaction)
            .await
            .expect("reaction seed must succeed");
    }

    let router = router_for(service, ctx);
    let response = get(router, "/repos/acme/widget/issues/7/reactions").await;

    assert_eq!(response.status(), StatusCode::OK);
    let json = body_json(response).await;
    let items = json.as_array().expect("items");
    assert_eq!(items.len(), 2, "only issue 7 reactions must be returned");
    assert_eq!(items[0]["id"], 1, "reactions must come back in id order");
    assert_eq!(items[0]["content"], "+1");
    assert_eq!(items[1]["content"], "heart");
    assert_eq!(items[1]["user"]["login"], "kate");
}

#[tokio::test]
async fn issue_reactions_of_unknown_repository_return_404() {
    let ctx = common::caller_in(Uuid::new_v4());
    let service = common::service("https://api.github.com").await;

    let router = router_for(service, ctx);
    let response = get(router, "/repos/acme/nope/issues/7/reactions").await;

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn issue_reactions_are_tenant_scoped() {
    let owner = common::caller_in(Uuid::new_v4());
    let db = common::inmem_db().await;
    let service = common::service_over(db.clone(), "https://api.github.com");
    service
        .upsert_repo(&owner, repo_record())
        .await
        .expect("repo seed must succeed");
    service
        .upsert_issue_reaction(&owner, "acme", "widget", reaction_record(4, 7, "rocket"))
        .await
        .expect("reaction seed must succeed");

    let stranger = common::caller_in(Uuid::new_v4());
    let router = router_for(service, stranger);
    let response = get(router, "/repos/acme/widget/issues/7/reactions").await;

    assert_eq!(
        response.status(),
        StatusCode::NOT_FOUND,
        "another tenant must not even learn the repository is mirrored"
    );
}

#[tokio::test]
async fn issue_reaction_upsert_is_idempotent_by_id() {
    let ctx = common::caller_in(Uuid::new_v4());
    let service = common::service("https://api.github.com").await;
    service
        .upsert_repo(&ctx, repo_record())
        .await
        .expect("repo seed must succeed");
    service
        .upsert_issue_reaction(&ctx, "acme", "widget", reaction_record(5, 7, "heart"))
        .await
        .expect("first upsert must succeed");

    let mut updated = reaction_record(5, 7, "heart");
    updated.content = "hooray".to_owned();
    updated.user_login = None;
    service
        .upsert_issue_reaction(&ctx, "acme", "widget", updated)
        .await
        .expect("second upsert must succeed");

    let router = router_for(service, ctx);
    let json = body_json(get(router, "/repos/acme/widget/issues/7/reactions").await).await;
    let items = json.as_array().expect("items");
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["content"], "hooray");
    assert!(items[0]["user"].is_null());
}
