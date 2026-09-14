#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::sync::Arc;

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use github_mirror::api::rest::routes::{ConcreteService, register_routes};
use github_mirror::domain::repo::{CheckRunRecord, RepoRecord};
use toolkit::api::OpenApiRegistryImpl;
use toolkit_security::SecurityContext;
use tower::ServiceExt;
use uuid::Uuid;

fn repo_record() -> RepoRecord {
    RepoRecord {
        node_id: None,
        id: 9201,
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

fn check_run_record(id: i64, head_sha: &str, name: &str) -> CheckRunRecord {
    CheckRunRecord {
        id,
        repo_id: 0,
        head_sha: head_sha.to_owned(),
        name: name.to_owned(),
        status: Some("completed".to_owned()),
        conclusion: Some("success".to_owned()),
        started_at: Some("2026-08-20T00:00:00Z".to_owned()),
        completed_at: Some("2026-08-20T00:03:00Z".to_owned()),
        html_url: Some(format!("https://github.com/acme/widget/runs/{id}")),
        details_url: None,
        check_suite_id: Some(900),
        app_slug: Some("github-actions".to_owned()),
        app_name: Some("GitHub Actions".to_owned()),
        output_title: Some("no warnings".to_owned()),
        output_summary: None,
        annotations_count: 0,
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
async fn check_runs_are_listed_for_their_commit_in_the_github_envelope() {
    let ctx = common::caller_in(Uuid::new_v4());
    let service = common::service("https://api.github.com").await;
    service
        .upsert_repo(&ctx, repo_record())
        .await
        .expect("repo seed must succeed");
    for run in [
        check_run_record(2, "aaa", "clippy"),
        check_run_record(1, "aaa", "fmt"),
        check_run_record(3, "bbb", "other-commit"),
    ] {
        service
            .upsert_check_run(&ctx, "acme", "widget", run)
            .await
            .expect("check-run seed must succeed");
    }

    let router = router_for(service, ctx);
    let response = get(router, "/repos/acme/widget/commits/aaa/check-runs").await;

    assert_eq!(response.status(), StatusCode::OK);
    let json = body_json(response).await;
    assert_eq!(json["total_count"], 2);
    let runs = json["check_runs"].as_array().expect("check_runs");
    assert_eq!(runs.len(), 2, "only commit aaa check runs must be returned");
    assert_eq!(runs[0]["id"], 1, "check runs must come back in id order");
    assert_eq!(runs[0]["name"], "fmt");
    assert_eq!(runs[1]["name"], "clippy");
    assert_eq!(runs[1]["check_suite"]["id"], 900);
    assert_eq!(runs[1]["app"]["slug"], "github-actions");
    assert_eq!(runs[1]["output"]["title"], "no warnings");
    assert_eq!(runs[1]["output"]["annotations_count"], 0);
}

#[tokio::test]
async fn check_runs_of_unknown_repository_return_404() {
    let ctx = common::caller_in(Uuid::new_v4());
    let service = common::service("https://api.github.com").await;

    let router = router_for(service, ctx);
    let response = get(router, "/repos/acme/nope/commits/aaa/check-runs").await;

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn check_runs_are_tenant_scoped() {
    let owner = common::caller_in(Uuid::new_v4());
    let db = common::inmem_db().await;
    let service = common::service_over(db.clone(), "https://api.github.com");
    service
        .upsert_repo(&owner, repo_record())
        .await
        .expect("repo seed must succeed");
    service
        .upsert_check_run(
            &owner,
            "acme",
            "widget",
            check_run_record(4, "aaa", "secret"),
        )
        .await
        .expect("check-run seed must succeed");

    let stranger = common::caller_in(Uuid::new_v4());
    let router = router_for(service, stranger);
    let response = get(router, "/repos/acme/widget/commits/aaa/check-runs").await;

    assert_eq!(
        response.status(),
        StatusCode::NOT_FOUND,
        "another tenant must not even learn the repository is mirrored"
    );
}

#[tokio::test]
async fn check_run_upsert_is_idempotent_by_id() {
    let ctx = common::caller_in(Uuid::new_v4());
    let service = common::service("https://api.github.com").await;
    service
        .upsert_repo(&ctx, repo_record())
        .await
        .expect("repo seed must succeed");
    service
        .upsert_check_run(&ctx, "acme", "widget", check_run_record(5, "aaa", "clippy"))
        .await
        .expect("first upsert must succeed");

    let mut updated = check_run_record(5, "aaa", "clippy");
    updated.status = Some("in_progress".to_owned());
    updated.conclusion = None;
    updated.app_slug = None;
    updated.app_name = None;
    service
        .upsert_check_run(&ctx, "acme", "widget", updated)
        .await
        .expect("second upsert must succeed");

    let router = router_for(service, ctx);
    let json = body_json(get(router, "/repos/acme/widget/commits/aaa/check-runs").await).await;
    assert_eq!(json["total_count"], 1);
    let runs = json["check_runs"].as_array().expect("check_runs");
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0]["status"], "in_progress");
    assert!(runs[0]["conclusion"].is_null());
    assert!(
        runs[0]["app"].is_null(),
        "an app-less check run must not invent an empty app object"
    );
}
