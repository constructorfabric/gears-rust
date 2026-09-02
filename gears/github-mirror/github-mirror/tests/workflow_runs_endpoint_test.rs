#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::sync::Arc;

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use github_mirror::api::rest::routes::{ConcreteService, register_routes};
use github_mirror::domain::repo::{RepoRecord, WorkflowRunRecord};
use toolkit::api::OpenApiRegistryImpl;
use toolkit_security::SecurityContext;
use tower::ServiceExt;
use uuid::Uuid;

fn repo_record() -> RepoRecord {
    RepoRecord {
        node_id: None,
        id: 998,
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

fn workflow_run_record(id: i64, run_number: i64, created_at: &str) -> WorkflowRunRecord {
    WorkflowRunRecord {
        id,
        repo_id: 0,
        workflow_id: 8,
        run_number,
        run_attempt: 1,
        name: Some("CI".to_owned()),
        event: "push".to_owned(),
        status: Some("completed".to_owned()),
        conclusion: Some("success".to_owned()),
        head_branch: Some("main".to_owned()),
        head_sha: "abc".to_owned(),
        created_at: created_at.to_owned(),
        updated_at: created_at.to_owned(),
        html_url: None,
        actor_login: Some("alice".to_owned()),
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
async fn workflow_runs_are_listed_newest_first() {
    let ctx = common::caller_in(Uuid::new_v4());
    let service = common::service("https://api.github.com").await;
    service
        .upsert_repo(&ctx, repo_record())
        .await
        .expect("repo seed must succeed");
    service
        .upsert_workflow_run(
            &ctx,
            "acme",
            "widget",
            workflow_run_record(1, 100, "2026-08-18T00:00:00Z"),
        )
        .await
        .expect("run seed must succeed");
    service
        .upsert_workflow_run(
            &ctx,
            "acme",
            "widget",
            workflow_run_record(2, 101, "2026-08-20T00:00:00Z"),
        )
        .await
        .expect("run seed must succeed");

    let router = router_for(service, ctx);
    let response = get(router, "/repos/acme/widget/actions/runs").await;

    assert_eq!(response.status(), StatusCode::OK);
    let json = body_json(response).await;
    let items = json["workflow_runs"].as_array().expect("workflow_runs");
    assert_eq!(items.len(), 2);
    assert_eq!(items[0]["run_number"], 101);
    assert_eq!(items[1]["run_number"], 100);
}

#[tokio::test]
async fn workflow_runs_of_unknown_repository_return_404() {
    let ctx = common::caller_in(Uuid::new_v4());
    let service = common::service("https://api.github.com").await;

    let router = router_for(service, ctx);
    let response = get(router, "/repos/acme/nope/actions/runs").await;

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn workflow_runs_are_tenant_scoped() {
    let owner = common::caller_in(Uuid::new_v4());
    let db = common::inmem_db().await;
    let service = common::service_over(db.clone(), "https://api.github.com");
    service
        .upsert_repo(&owner, repo_record())
        .await
        .expect("repo seed must succeed");
    service
        .upsert_workflow_run(
            &owner,
            "acme",
            "widget",
            workflow_run_record(3, 102, "2026-08-20T00:00:00Z"),
        )
        .await
        .expect("run seed must succeed");

    let stranger = common::caller_in(Uuid::new_v4());
    let router = router_for(service, stranger);
    let response = get(router, "/repos/acme/widget/actions/runs").await;

    assert_eq!(
        response.status(),
        StatusCode::NOT_FOUND,
        "another tenant must not even learn the repository is mirrored"
    );
}

#[tokio::test]
async fn workflow_run_upsert_is_idempotent_by_id() {
    let ctx = common::caller_in(Uuid::new_v4());
    let service = common::service("https://api.github.com").await;
    service
        .upsert_repo(&ctx, repo_record())
        .await
        .expect("repo seed must succeed");
    service
        .upsert_workflow_run(
            &ctx,
            "acme",
            "widget",
            workflow_run_record(4, 103, "2026-08-20T00:00:00Z"),
        )
        .await
        .expect("first upsert must succeed");

    let mut updated = workflow_run_record(4, 103, "2026-08-20T00:00:00Z");
    updated.status = Some("completed".to_owned());
    updated.conclusion = Some("failure".to_owned());
    updated.run_attempt = 2;
    service
        .upsert_workflow_run(&ctx, "acme", "widget", updated)
        .await
        .expect("second upsert must succeed");

    let router = router_for(service, ctx);
    let response = get(router, "/repos/acme/widget/actions/runs").await;
    let json = body_json(response).await;
    let items = json["workflow_runs"].as_array().expect("workflow_runs");
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["conclusion"], "failure");
    assert_eq!(items[0]["run_attempt"], 2);
}
