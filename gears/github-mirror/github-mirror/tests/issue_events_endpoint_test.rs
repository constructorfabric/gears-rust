#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::sync::Arc;

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use github_mirror::api::rest::routes::{ConcreteService, register_routes};
use github_mirror::domain::repo::{IssueEventRecord, RepoRecord};
use toolkit::api::OpenApiRegistryImpl;
use toolkit_security::SecurityContext;
use tower::ServiceExt;
use uuid::Uuid;

fn repo_record() -> RepoRecord {
    RepoRecord {
        node_id: None,
        id: 5001,
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

fn event_record(id: i64, issue_number: i64, event: &str, created_at: &str) -> IssueEventRecord {
    IssueEventRecord {
        id,
        repo_id: 0,
        issue_number,
        event: event.to_owned(),
        actor_login: Some("grace".to_owned()),
        label_name: Some("bug".to_owned()),
        assignee_login: None,
        milestone_title: None,
        commit_id: None,
        created_at: created_at.to_owned(),
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
async fn issue_events_are_listed_oldest_first_for_their_issue() {
    let ctx = common::caller_in(Uuid::new_v4());
    let service = common::service("https://api.github.com").await;
    service
        .upsert_repo(&ctx, repo_record())
        .await
        .expect("repo seed must succeed");
    service
        .upsert_issue_event(
            &ctx,
            "acme",
            "widget",
            event_record(2, 11, "closed", "2026-08-20T00:00:00Z"),
        )
        .await
        .expect("event seed must succeed");
    service
        .upsert_issue_event(
            &ctx,
            "acme",
            "widget",
            event_record(1, 11, "labeled", "2026-08-18T00:00:00Z"),
        )
        .await
        .expect("event seed must succeed");
    service
        .upsert_issue_event(
            &ctx,
            "acme",
            "widget",
            event_record(3, 12, "assigned", "2026-08-19T00:00:00Z"),
        )
        .await
        .expect("event seed must succeed");

    let router = router_for(service, ctx);
    let response = get(router, "/repos/acme/widget/issues/11/events").await;

    assert_eq!(response.status(), StatusCode::OK);
    let json = body_json(response).await;
    let items = json.as_array().expect("items");
    assert_eq!(items.len(), 2, "only issue 11 events must be returned");
    assert_eq!(items[0]["id"], 1);
    assert_eq!(items[0]["event"], "labeled");
    assert_eq!(items[0]["actor"]["login"], "grace");
    assert_eq!(items[0]["label"]["name"], "bug");
    assert_eq!(items[1]["id"], 2);
}

#[tokio::test]
async fn issue_events_of_unknown_repository_return_404() {
    let ctx = common::caller_in(Uuid::new_v4());
    let service = common::service("https://api.github.com").await;

    let router = router_for(service, ctx);
    let response = get(router, "/repos/acme/nope/issues/11/events").await;

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn issue_events_are_tenant_scoped() {
    let owner = common::caller_in(Uuid::new_v4());
    let db = common::inmem_db().await;
    let service = common::service_over(db.clone(), "https://api.github.com");
    service
        .upsert_repo(&owner, repo_record())
        .await
        .expect("repo seed must succeed");
    service
        .upsert_issue_event(
            &owner,
            "acme",
            "widget",
            event_record(4, 11, "labeled", "2026-08-20T00:00:00Z"),
        )
        .await
        .expect("event seed must succeed");

    let stranger = common::caller_in(Uuid::new_v4());
    let router = router_for(service, stranger);
    let response = get(router, "/repos/acme/widget/issues/11/events").await;

    assert_eq!(
        response.status(),
        StatusCode::NOT_FOUND,
        "another tenant must not even learn the repository is mirrored"
    );
}

#[tokio::test]
async fn issue_event_upsert_is_idempotent_by_id() {
    let ctx = common::caller_in(Uuid::new_v4());
    let service = common::service("https://api.github.com").await;
    service
        .upsert_repo(&ctx, repo_record())
        .await
        .expect("repo seed must succeed");
    service
        .upsert_issue_event(
            &ctx,
            "acme",
            "widget",
            event_record(5, 11, "labeled", "2026-08-20T00:00:00Z"),
        )
        .await
        .expect("first upsert must succeed");

    let mut updated = event_record(5, 11, "unlabeled", "2026-08-20T00:00:00Z");
    updated.label_name = Some("wontfix".to_owned());
    service
        .upsert_issue_event(&ctx, "acme", "widget", updated)
        .await
        .expect("second upsert must succeed");

    let router = router_for(service, ctx);
    let response = get(router, "/repos/acme/widget/issues/11/events").await;
    let json = body_json(response).await;
    let items = json.as_array().expect("items");
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["event"], "unlabeled");
    assert_eq!(items[0]["label"]["name"], "wontfix");
}
