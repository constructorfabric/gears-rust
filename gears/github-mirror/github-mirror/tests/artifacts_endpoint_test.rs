#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::sync::Arc;

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use github_mirror::api::rest::routes::{ConcreteService, register_routes};
use github_mirror::domain::ports::github::ActionsListing;
use github_mirror::domain::repo::{ArtifactRecord, RepoRecord, SyncWriter};
use github_mirror::infra::storage::sea_orm_repo::SeaOrmSyncWriter;
use toolkit::api::OpenApiRegistryImpl;
use toolkit_db::DBProvider;
use toolkit_security::{AccessScope, SecurityContext};
use tower::ServiceExt;
use uuid::Uuid;

const REPO_ID: i64 = 5101;

fn repo_record() -> RepoRecord {
    RepoRecord {
        node_id: None,
        id: REPO_ID,
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

fn artifact_record(id: i64, name: &str, created_at: &str, run: Option<i64>) -> ArtifactRecord {
    ArtifactRecord {
        id,
        repo_id: REPO_ID,
        name: name.to_owned(),
        size_in_bytes: 1024,
        expired: false,
        workflow_run_id: run,
        head_branch: run.map(|_| "main".to_owned()),
        head_sha: run.map(|_| "abc".to_owned()),
        created_at: Some(created_at.to_owned()),
        updated_at: Some(created_at.to_owned()),
        expires_at: Some("2026-11-20T00:00:00Z".to_owned()),
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

async fn service_with_artifacts(ctx: &SecurityContext) -> Arc<ConcreteService> {
    let db = common::inmem_db().await;
    let service = common::service_over(db.clone(), "https://api.github.com");
    service
        .upsert_repo(ctx, repo_record())
        .await
        .expect("repo seed must succeed");

    let tenant = ctx.subject_tenant_id();
    SeaOrmSyncWriter::new(Arc::new(DBProvider::new(db)))
        .write_actions_listing(
            &AccessScope::for_tenant(tenant),
            tenant,
            ActionsListing {
                workflow_runs: Vec::new(),
                deployments: Vec::new(),
                artifacts: vec![
                    artifact_record(1, "coverage", "2026-08-18T00:00:00Z", Some(81)),
                    artifact_record(2, "binaries", "2026-08-20T00:00:00Z", None),
                ],
            },
        )
        .await
        .expect("artifact seed must succeed");
    service
}

#[tokio::test]
async fn artifacts_are_listed_newest_first_in_githubs_shape() {
    let ctx = common::caller_in(Uuid::new_v4());
    let service = service_with_artifacts(&ctx).await;

    let response = get(router_for(service, ctx), "/repos/acme/widget/actions/artifacts").await;

    assert_eq!(response.status(), StatusCode::OK);
    let json = body_json(response).await;
    assert_eq!(json["total_count"], 2);
    let artifacts = json["artifacts"].as_array().expect("artifacts");
    assert_eq!(artifacts[0]["name"], "binaries");
    assert!(artifacts[0]["workflow_run"].is_null());
    assert_eq!(artifacts[1]["name"], "coverage");
    assert_eq!(artifacts[1]["size_in_bytes"], 1024);
    assert_eq!(artifacts[1]["workflow_run"]["id"], 81);
    assert_eq!(artifacts[1]["workflow_run"]["head_branch"], "main");
}

#[tokio::test]
async fn a_page_of_artifacts_still_counts_every_artifact() {
    let ctx = common::caller_in(Uuid::new_v4());
    let service = service_with_artifacts(&ctx).await;

    let response = get(
        router_for(service, ctx),
        "/repos/acme/widget/actions/artifacts?per_page=1",
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    assert!(response.headers().get(axum::http::header::LINK).is_some());
    let json = body_json(response).await;
    assert_eq!(json["total_count"], 2);
    assert_eq!(json["artifacts"].as_array().expect("artifacts").len(), 1);
}

#[tokio::test]
async fn artifacts_of_unknown_repository_return_404() {
    let ctx = common::caller_in(Uuid::new_v4());
    let service = common::service("https://api.github.com").await;

    let response = get(router_for(service, ctx), "/repos/acme/nope/actions/artifacts").await;

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn artifacts_are_tenant_scoped() {
    let owner = common::caller_in(Uuid::new_v4());
    let service = service_with_artifacts(&owner).await;

    let stranger = common::caller_in(Uuid::new_v4());
    let response = get(
        router_for(service, stranger),
        "/repos/acme/widget/actions/artifacts",
    )
    .await;

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}
