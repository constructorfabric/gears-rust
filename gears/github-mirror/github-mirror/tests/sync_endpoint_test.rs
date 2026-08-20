#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::sync::Arc;

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use github_mirror::api::rest::routes::{ConcreteService, register_routes};
use github_mirror::domain::ports::github::FetchedRepository;
use github_mirror::domain::repo::{CommitRecord, IssueRecord, PullRequestRecord, RepositoryRecord};
use toolkit::api::OpenApiRegistryImpl;
use toolkit_security::SecurityContext;
use tower::ServiceExt;
use uuid::Uuid;

fn fetched() -> FetchedRepository {
    FetchedRepository {
        repository: RepositoryRecord {
            id: 42,
            owner: "rust-lang".to_owned(),
            name: "rust".to_owned(),
            full_name: "rust-lang/rust".to_owned(),
            default_branch: "master".to_owned(),
            private: false,
            pushed_at: Some("2026-08-20T00:00:00Z".to_owned()),
            stars: 100_000,
            forks: 13_000,
            description: Some("the compiler".to_owned()),
        },
        issues: vec![IssueRecord {
            id: 1,
            repo_id: 42,
            number: 11,
            title: "an issue".to_owned(),
            body: None,
            state: "open".to_owned(),
            is_pull_request: false,
            created_at: "2026-08-20T00:00:00Z".to_owned(),
            updated_at: "2026-08-20T00:00:00Z".to_owned(),
            closed_at: None,
            html_url: None,
        }],
        pull_requests: vec![PullRequestRecord {
            id: 2,
            repo_id: 42,
            number: 12,
            title: "a pr".to_owned(),
            body: None,
            state: "open".to_owned(),
            draft: false,
            merged: false,
            head_sha: Some("h1".to_owned()),
            base_sha: Some("b1".to_owned()),
            lines_added: 0,
            lines_removed: 0,
            created_at: "2026-08-20T00:00:00Z".to_owned(),
            updated_at: "2026-08-20T00:00:00Z".to_owned(),
            closed_at: None,
            merged_at: None,
        }],
        commits: vec![
            CommitRecord {
                repo_id: 42,
                sha: "c1".to_owned(),
                message: "first".to_owned(),
                author_login: None,
                committer_login: None,
                authored_at: None,
                committed_at: Some("2026-08-19T00:00:00Z".to_owned()),
                additions: 0,
                deletions: 0,
            },
            CommitRecord {
                repo_id: 42,
                sha: "c2".to_owned(),
                message: "second".to_owned(),
                author_login: None,
                committer_login: None,
                authored_at: None,
                committed_at: Some("2026-08-20T00:00:00Z".to_owned()),
                additions: 0,
                deletions: 0,
            },
        ],
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

async fn post(router: Router, uri: &str) -> axum::http::Response<Body> {
    let request = Request::builder()
        .method("POST")
        .uri(uri)
        .body(Body::empty())
        .unwrap();
    router.oneshot(request).await.unwrap()
}

async fn get(router: Router, uri: &str) -> axum::http::Response<Body> {
    let request = Request::builder().uri(uri).body(Body::empty()).unwrap();
    router.oneshot(request).await.unwrap()
}

#[tokio::test]
async fn sync_fills_all_four_tables_and_reads_serve_them() {
    let ctx = common::caller_in(Uuid::new_v4());
    let db = common::inmem_db().await;
    let service = common::service_with_github(
        db,
        "https://api.github.com",
        Arc::new(common::FakeGithub {
            result: Some(fetched()),
        }),
    );
    let router = router_for(service, ctx);

    let response = post(
        router.clone(),
        "/github-mirror/v1/repos/rust-lang/rust/sync",
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let summary = body_json(response).await;
    assert_eq!(summary["repository"], "rust-lang/rust");
    assert_eq!(summary["issues_synced"], 1);
    assert_eq!(summary["pull_requests_synced"], 1);
    assert_eq!(summary["commits_synced"], 2);

    let repos = body_json(get(router.clone(), "/github-mirror/v1/repos").await).await;
    assert_eq!(repos["items"][0]["full_name"], "rust-lang/rust");

    let issues = body_json(
        get(
            router.clone(),
            "/github-mirror/v1/repos/rust-lang/rust/issues",
        )
        .await,
    )
    .await;
    assert_eq!(issues["items"].as_array().expect("items").len(), 1);

    let pulls = body_json(
        get(
            router.clone(),
            "/github-mirror/v1/repos/rust-lang/rust/pulls",
        )
        .await,
    )
    .await;
    assert_eq!(pulls["items"].as_array().expect("items").len(), 1);

    let commits =
        body_json(get(router, "/github-mirror/v1/repos/rust-lang/rust/commits").await).await;
    assert_eq!(commits["items"].as_array().expect("items").len(), 2);
    assert_eq!(commits["items"][0]["sha"], "c2");
}

#[tokio::test]
async fn sync_of_unknown_repository_returns_404() {
    let ctx = common::caller_in(Uuid::new_v4());
    let service = common::service("https://api.github.com").await;
    let router = router_for(service, ctx);

    let response = post(router, "/github-mirror/v1/repos/acme/nope/sync").await;

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn sync_is_idempotent() {
    let ctx = common::caller_in(Uuid::new_v4());
    let db = common::inmem_db().await;
    let service = common::service_with_github(
        db,
        "https://api.github.com",
        Arc::new(common::FakeGithub {
            result: Some(fetched()),
        }),
    );
    let router = router_for(service, ctx);

    let first = post(
        router.clone(),
        "/github-mirror/v1/repos/rust-lang/rust/sync",
    )
    .await;
    assert_eq!(first.status(), StatusCode::OK);
    let second = post(
        router.clone(),
        "/github-mirror/v1/repos/rust-lang/rust/sync",
    )
    .await;
    assert_eq!(second.status(), StatusCode::OK);

    let repos = body_json(get(router, "/github-mirror/v1/repos").await).await;
    assert_eq!(repos["items"].as_array().expect("items").len(), 1);
}
