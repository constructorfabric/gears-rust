#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::sync::Arc;

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use github_mirror::api::rest::routes::{ConcreteService, register_routes};
use github_mirror::domain::repo::{ContributorRecord, RepoRecord};
use toolkit::api::OpenApiRegistryImpl;
use toolkit_security::SecurityContext;
use tower::ServiceExt;
use uuid::Uuid;

/// An RFC3339 literal as the instant the mirror stores.
fn instant(raw: &str) -> chrono::DateTime<chrono::Utc> {
    chrono::DateTime::parse_from_rfc3339(raw)
        .expect("test timestamps must be valid RFC3339")
        .with_timezone(&chrono::Utc)
}

fn repo_record() -> RepoRecord {
    RepoRecord {
        node_id: None,
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
        clone_url: None,
    }
}

fn contributor_record(user_id: i64, login: &str, roles: &[&str]) -> ContributorRecord {
    ContributorRecord {
        repo_id: 0,
        user_id,
        login: Some(login.to_owned()),
        account_type: "User".to_owned(),
        avatar_url: None,
        html_url: None,
        roles: roles.iter().map(|r| (*r).to_owned()).collect(),
        first_seen_at: Some(instant("2026-03-02T09:14:00Z")),
        last_seen_at: Some(instant("2026-08-19T17:02:00Z")),
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
async fn contributors_are_listed_by_user_id() {
    let ctx = common::caller_in(Uuid::new_v4());
    let service = common::service("https://api.github.com").await;
    service
        .upsert_repo(&ctx, repo_record())
        .await
        .expect("repo seed must succeed");
    service
        .upsert_contributor(
            &ctx,
            "acme",
            "widget",
            contributor_record(1, "bob", &["commenter"]),
        )
        .await
        .expect("contributor seed must succeed");
    service
        .upsert_contributor(
            &ctx,
            "acme",
            "widget",
            contributor_record(2, "alice", &["author", "reviewer"]),
        )
        .await
        .expect("contributor seed must succeed");

    let router = router_for(service, ctx);
    let response = get(router, "/repos/acme/widget/contributors").await;

    assert_eq!(response.status(), StatusCode::OK);
    let json = body_json(response).await;
    let items = json.as_array().expect("items");
    assert_eq!(items.len(), 2);
    assert_eq!(items[0]["login"], "bob", "ordered by GitHub user id");
    assert_eq!(items[1]["login"], "alice");
    assert_eq!(
        items[1]["roles"],
        serde_json::json!(["author", "reviewer"]),
        "PRD 5.2's association roles ride along with each person"
    );
}

#[tokio::test]
async fn contributors_of_unknown_repository_return_404() {
    let ctx = common::caller_in(Uuid::new_v4());
    let service = common::service("https://api.github.com").await;

    let router = router_for(service, ctx);
    let response = get(router, "/repos/acme/nope/contributors").await;

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn contributors_are_tenant_scoped() {
    let owner = common::caller_in(Uuid::new_v4());
    let db = common::inmem_db().await;
    let service = common::service_over(db.clone(), "https://api.github.com");
    service
        .upsert_repo(&owner, repo_record())
        .await
        .expect("repo seed must succeed");
    service
        .upsert_contributor(
            &owner,
            "acme",
            "widget",
            contributor_record(3, "carol", &["commenter"]),
        )
        .await
        .expect("contributor seed must succeed");

    let stranger = common::caller_in(Uuid::new_v4());
    let router = router_for(service, stranger);
    let response = get(router, "/repos/acme/widget/contributors").await;

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
        .upsert_repo(&ctx, repo_record())
        .await
        .expect("repo seed must succeed");
    service
        .upsert_contributor(
            &ctx,
            "acme",
            "widget",
            contributor_record(4, "dave", &["commenter"]),
        )
        .await
        .expect("first upsert must succeed");

    let updated = contributor_record(4, "dave", &["assignee"]);
    service
        .upsert_contributor(&ctx, "acme", "widget", updated)
        .await
        .expect("second upsert must succeed");

    let router = router_for(service, ctx);
    let response = get(router, "/repos/acme/widget/contributors").await;
    let json = body_json(response).await;
    let items = json.as_array().expect("items");
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["roles"], serde_json::json!(["assignee"]));
}
