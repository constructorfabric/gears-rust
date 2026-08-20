#![allow(clippy::unwrap_used, clippy::expect_used)]

use github_mirror::domain::error::DomainError;
use github_mirror::domain::ports::github::GithubPort;
use github_mirror::infra::github::client::GithubClient;
use httpmock::MockServer;
use serde_json::json;

fn gh_repo_json() -> serde_json::Value {
    json!({
        "id": 42,
        "name": "rust",
        "full_name": "rust-lang/rust",
        "owner": { "login": "rust-lang" },
        "default_branch": "master",
        "private": false,
        "pushed_at": "2026-08-20T00:00:00Z",
        "stargazers_count": 100_000,
        "forks_count": 13_000,
        "description": "the compiler"
    })
}

fn gh_issues_json() -> serde_json::Value {
    json!([
        {
            "id": 1, "number": 11, "title": "an issue", "body": "text",
            "state": "open", "created_at": "2026-08-20T00:00:00Z",
            "updated_at": "2026-08-20T00:00:00Z", "closed_at": null,
            "html_url": "https://github.com/rust-lang/rust/issues/11"
        },
        {
            "id": 2, "number": 12, "title": "a pr shown as issue",
            "state": "open", "pull_request": {},
            "created_at": "2026-08-20T00:00:00Z",
            "updated_at": "2026-08-20T00:00:00Z"
        }
    ])
}

fn gh_pulls_json() -> serde_json::Value {
    json!([
        {
            "id": 3, "number": 13, "title": "a pr", "state": "open",
            "draft": true, "merged_at": null,
            "head": { "sha": "h1" }, "base": { "sha": "b1" },
            "created_at": "2026-08-20T00:00:00Z",
            "updated_at": "2026-08-20T00:00:00Z", "closed_at": null
        }
    ])
}

fn gh_commits_json() -> serde_json::Value {
    json!([
        {
            "sha": "c1",
            "commit": {
                "message": "first",
                "author": { "date": "2026-08-19T00:00:00Z" },
                "committer": { "date": "2026-08-19T00:00:00Z" }
            },
            "author": { "login": "alice" },
            "committer": { "login": "bob" }
        }
    ])
}

#[tokio::test]
async fn fetch_repository_maps_github_payloads_into_records() {
    let server = MockServer::start_async().await;
    server
        .mock_async(|when, then| {
            when.method("GET").path("/repos/rust-lang/rust");
            then.status(200).json_body(gh_repo_json());
        })
        .await;
    server
        .mock_async(|when, then| {
            when.method("GET").path("/repos/rust-lang/rust/issues");
            then.status(200).json_body(gh_issues_json());
        })
        .await;
    server
        .mock_async(|when, then| {
            when.method("GET").path("/repos/rust-lang/rust/pulls");
            then.status(200).json_body(gh_pulls_json());
        })
        .await;
    server
        .mock_async(|when, then| {
            when.method("GET").path("/repos/rust-lang/rust/commits");
            then.status(200).json_body(gh_commits_json());
        })
        .await;

    let client =
        GithubClient::new(server.base_url(), Some("tok".to_owned())).expect("client must build");
    let fetched = client
        .fetch_repository("rust-lang", "rust")
        .await
        .expect("fetch must succeed");

    assert_eq!(fetched.repository.id, 42);
    assert_eq!(fetched.repository.owner, "rust-lang");
    assert_eq!(fetched.repository.full_name, "rust-lang/rust");
    assert_eq!(fetched.repository.stars, 100_000);

    assert_eq!(fetched.issues.len(), 2);
    assert!(!fetched.issues[0].is_pull_request);
    assert!(fetched.issues[1].is_pull_request);

    assert_eq!(fetched.pull_requests.len(), 1);
    assert!(fetched.pull_requests[0].draft);
    assert!(!fetched.pull_requests[0].merged);
    assert_eq!(fetched.pull_requests[0].head_sha.as_deref(), Some("h1"));

    assert_eq!(fetched.commits.len(), 1);
    assert_eq!(fetched.commits[0].sha, "c1");
    assert_eq!(fetched.commits[0].author_login.as_deref(), Some("alice"));
    assert_eq!(fetched.commits[0].committer_login.as_deref(), Some("bob"));
    assert_eq!(
        fetched.commits[0].committed_at.as_deref(),
        Some("2026-08-19T00:00:00Z")
    );
}

#[tokio::test]
async fn github_404_maps_to_not_found() {
    let server = MockServer::start_async().await;
    server
        .mock_async(|when, then| {
            when.method("GET").path("/repos/acme/nope");
            then.status(404).json_body(json!({"message": "Not Found"}));
        })
        .await;

    let client = GithubClient::new(server.base_url(), None).expect("client must build");
    let result = client.fetch_repository("acme", "nope").await;

    assert!(matches!(result, Err(DomainError::NotFound)));
}

#[tokio::test]
async fn github_server_error_maps_to_internal() {
    let server = MockServer::start_async().await;
    server
        .mock_async(|when, then| {
            when.method("GET").path("/repos/acme/flaky");
            then.status(503);
        })
        .await;

    let client = GithubClient::new(server.base_url(), None).expect("client must build");
    let result = client.fetch_repository("acme", "flaky").await;

    assert!(matches!(result, Err(DomainError::Internal(_))));
}

#[tokio::test]
async fn malformed_json_maps_to_internal() {
    let server = MockServer::start_async().await;
    server
        .mock_async(|when, then| {
            when.method("GET").path("/repos/acme/garbage");
            then.status(200).body("not json");
        })
        .await;

    let client = GithubClient::new(server.base_url(), None).expect("client must build");
    let result = client.fetch_repository("acme", "garbage").await;

    assert!(matches!(result, Err(DomainError::Internal(_))));
}
