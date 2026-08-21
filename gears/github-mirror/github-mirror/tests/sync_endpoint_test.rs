#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::sync::Arc;

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use github_mirror::api::rest::routes::{ConcreteService, register_routes};
use github_mirror::domain::ports::github::FetchedRepository;
use github_mirror::domain::repo::{
    BranchRecord, CommentRecord, CommitRecord, ContributorRecord, IssueRecord, LabelRecord,
    MilestoneRecord, PullRequestRecord, ReleaseRecord, RepositoryRecord, ReviewCommentRecord,
    ReviewRecord, WorkflowRunRecord,
};
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
        comments: vec![CommentRecord {
            id: 9,
            repo_id: 42,
            issue_number: 11,
            author_login: Some("carol".to_owned()),
            body: Some("looks good".to_owned()),
            created_at: "2026-08-20T00:00:00Z".to_owned(),
            updated_at: "2026-08-20T00:00:00Z".to_owned(),
            html_url: None,
        }],
        review_comments: vec![ReviewCommentRecord {
            id: 21,
            repo_id: 42,
            pull_number: 12,
            author_login: Some("dave".to_owned()),
            body: Some("rename this".to_owned()),
            path: Some("src/lib.rs".to_owned()),
            diff_hunk: None,
            in_reply_to_id: None,
            commit_id: Some("h1".to_owned()),
            created_at: "2026-08-20T00:00:00Z".to_owned(),
            updated_at: "2026-08-20T00:00:00Z".to_owned(),
            html_url: None,
        }],
        reviews: vec![ReviewRecord {
            id: 31,
            repo_id: 42,
            pull_number: 12,
            author_login: Some("erin".to_owned()),
            state: "APPROVED".to_owned(),
            body: Some("ship it".to_owned()),
            commit_id: Some("h1".to_owned()),
            submitted_at: Some("2026-08-20T00:00:00Z".to_owned()),
            html_url: None,
        }],
        labels: vec![LabelRecord {
            id: 41,
            repo_id: 42,
            name: "bug".to_owned(),
            color: "d73a4a".to_owned(),
            is_default: true,
            description: Some("Something is not working".to_owned()),
        }],
        milestones: vec![MilestoneRecord {
            id: 51,
            repo_id: 42,
            number: 1,
            title: "v1.0".to_owned(),
            state: "open".to_owned(),
            description: Some("first stable".to_owned()),
            open_issues: 3,
            closed_issues: 7,
            due_on: Some("2026-09-30T00:00:00Z".to_owned()),
            created_at: "2026-08-20T00:00:00Z".to_owned(),
            updated_at: "2026-08-20T00:00:00Z".to_owned(),
            closed_at: None,
            html_url: None,
        }],
        releases: vec![ReleaseRecord {
            id: 61,
            repo_id: 42,
            tag_name: "v1.0.0".to_owned(),
            name: Some("First stable".to_owned()),
            draft: false,
            prerelease: false,
            body: Some("changelog".to_owned()),
            author_login: Some("erin".to_owned()),
            created_at: "2026-08-20T00:00:00Z".to_owned(),
            published_at: Some("2026-08-20T00:00:00Z".to_owned()),
            html_url: None,
        }],
        branches: vec![BranchRecord {
            repo_id: 42,
            name: "master".to_owned(),
            commit_sha: "c2".to_owned(),
            protected: true,
        }],
        contributors: vec![ContributorRecord {
            repo_id: 42,
            user_id: 71,
            login: "alice".to_owned(),
            contributions: 120,
            user_type: "User".to_owned(),
            avatar_url: None,
            html_url: None,
        }],
        workflow_runs: vec![WorkflowRunRecord {
            id: 81,
            repo_id: 42,
            workflow_id: 8,
            run_number: 300,
            run_attempt: 1,
            name: Some("CI".to_owned()),
            event: "push".to_owned(),
            status: Some("completed".to_owned()),
            conclusion: Some("success".to_owned()),
            head_branch: Some("master".to_owned()),
            head_sha: "c2".to_owned(),
            created_at: "2026-08-20T00:00:00Z".to_owned(),
            updated_at: "2026-08-20T00:00:00Z".to_owned(),
            html_url: None,
            actor_login: Some("alice".to_owned()),
        }],
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
async fn sync_fills_all_thirteen_tables_and_reads_serve_them() {
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
    assert_eq!(summary["comments_synced"], 1);
    assert_eq!(summary["review_comments_synced"], 1);
    assert_eq!(summary["reviews_synced"], 1);
    assert_eq!(summary["labels_synced"], 1);
    assert_eq!(summary["milestones_synced"], 1);
    assert_eq!(summary["releases_synced"], 1);
    assert_eq!(summary["branches_synced"], 1);
    assert_eq!(summary["contributors_synced"], 1);
    assert_eq!(summary["workflow_runs_synced"], 1);

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

    let commits = body_json(
        get(
            router.clone(),
            "/github-mirror/v1/repos/rust-lang/rust/commits",
        )
        .await,
    )
    .await;
    let router2 = router;
    assert_eq!(commits["items"].as_array().expect("items").len(), 2);
    assert_eq!(commits["items"][0]["sha"], "c2");

    let comments = body_json(
        get(
            router2.clone(),
            "/github-mirror/v1/repos/rust-lang/rust/issues/11/comments",
        )
        .await,
    )
    .await;
    assert_eq!(comments["items"].as_array().expect("items").len(), 1);
    assert_eq!(comments["items"][0]["author_login"], "carol");

    let review_comments = body_json(
        get(
            router2.clone(),
            "/github-mirror/v1/repos/rust-lang/rust/pulls/12/comments",
        )
        .await,
    )
    .await;
    assert_eq!(review_comments["items"].as_array().expect("items").len(), 1);
    assert_eq!(review_comments["items"][0]["author_login"], "dave");
    assert_eq!(review_comments["items"][0]["path"], "src/lib.rs");

    let reviews = body_json(
        get(
            router2.clone(),
            "/github-mirror/v1/repos/rust-lang/rust/pulls/12/reviews",
        )
        .await,
    )
    .await;
    assert_eq!(reviews["items"].as_array().expect("items").len(), 1);
    assert_eq!(reviews["items"][0]["author_login"], "erin");
    assert_eq!(reviews["items"][0]["state"], "APPROVED");

    let labels = body_json(
        get(
            router2.clone(),
            "/github-mirror/v1/repos/rust-lang/rust/labels",
        )
        .await,
    )
    .await;
    assert_eq!(labels["items"].as_array().expect("items").len(), 1);
    assert_eq!(labels["items"][0]["name"], "bug");
    assert_eq!(labels["items"][0]["is_default"], true);

    let milestones = body_json(
        get(
            router2.clone(),
            "/github-mirror/v1/repos/rust-lang/rust/milestones",
        )
        .await,
    )
    .await;
    assert_eq!(milestones["items"].as_array().expect("items").len(), 1);
    assert_eq!(milestones["items"][0]["title"], "v1.0");
    assert_eq!(milestones["items"][0]["closed_issues"], 7);

    let releases = body_json(
        get(
            router2.clone(),
            "/github-mirror/v1/repos/rust-lang/rust/releases",
        )
        .await,
    )
    .await;
    assert_eq!(releases["items"].as_array().expect("items").len(), 1);
    assert_eq!(releases["items"][0]["tag_name"], "v1.0.0");
    assert_eq!(releases["items"][0]["author_login"], "erin");

    let branches = body_json(
        get(
            router2.clone(),
            "/github-mirror/v1/repos/rust-lang/rust/branches",
        )
        .await,
    )
    .await;
    assert_eq!(branches["items"].as_array().expect("items").len(), 1);
    assert_eq!(branches["items"][0]["name"], "master");
    assert_eq!(branches["items"][0]["commit_sha"], "c2");

    let contributors = body_json(
        get(
            router2.clone(),
            "/github-mirror/v1/repos/rust-lang/rust/contributors",
        )
        .await,
    )
    .await;
    assert_eq!(contributors["items"].as_array().expect("items").len(), 1);
    assert_eq!(contributors["items"][0]["login"], "alice");
    assert_eq!(contributors["items"][0]["contributions"], 120);

    let workflow_runs = body_json(
        get(
            router2,
            "/github-mirror/v1/repos/rust-lang/rust/actions/runs",
        )
        .await,
    )
    .await;
    assert_eq!(workflow_runs["items"].as_array().expect("items").len(), 1);
    assert_eq!(workflow_runs["items"][0]["run_number"], 300);
    assert_eq!(workflow_runs["items"][0]["conclusion"], "success");
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
