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

fn gh_comments_json() -> serde_json::Value {
    json!([
        {
            "id": 7,
            "user": { "login": "carol" },
            "body": "looks good",
            "created_at": "2026-08-20T00:00:00Z",
            "updated_at": "2026-08-20T00:00:00Z",
            "html_url": "https://github.com/rust-lang/rust/issues/11#issuecomment-7",
            "issue_url": "https://api.github.com/repos/rust-lang/rust/issues/11"
        }
    ])
}

fn gh_review_comments_json() -> serde_json::Value {
    json!([
        {
            "id": 21,
            "user": { "login": "dave" },
            "body": "rename this",
            "path": "src/lib.rs",
            "diff_hunk": "@@ -1 +1 @@",
            "in_reply_to_id": null,
            "commit_id": "h1",
            "created_at": "2026-08-20T00:00:00Z",
            "updated_at": "2026-08-20T00:00:00Z",
            "html_url": "https://github.com/rust-lang/rust/pull/13#discussion_r21",
            "pull_request_url": "https://api.github.com/repos/rust-lang/rust/pulls/13"
        }
    ])
}

fn gh_reviews_json() -> serde_json::Value {
    json!([
        {
            "id": 31,
            "user": { "login": "erin" },
            "state": "APPROVED",
            "body": "ship it",
            "commit_id": "h1",
            "submitted_at": "2026-08-20T00:00:00Z",
            "html_url": "https://github.com/rust-lang/rust/pull/13#pullrequestreview-31"
        }
    ])
}

fn gh_labels_json() -> serde_json::Value {
    json!([
        {
            "id": 41,
            "name": "bug",
            "color": "d73a4a",
            "default": true,
            "description": "Something is not working"
        }
    ])
}

fn gh_milestones_json() -> serde_json::Value {
    json!([
        {
            "id": 51,
            "number": 1,
            "title": "v1.0",
            "state": "open",
            "description": "first stable",
            "open_issues": 3,
            "closed_issues": 7,
            "due_on": "2026-09-30T00:00:00Z",
            "created_at": "2026-08-20T00:00:00Z",
            "updated_at": "2026-08-20T00:00:00Z",
            "closed_at": null,
            "html_url": "https://github.com/rust-lang/rust/milestone/1"
        }
    ])
}

fn gh_releases_json() -> serde_json::Value {
    json!([
        {
            "id": 61,
            "tag_name": "v1.0.0",
            "name": "First stable",
            "draft": false,
            "prerelease": false,
            "body": "changelog",
            "author": { "login": "erin" },
            "created_at": "2026-08-20T00:00:00Z",
            "published_at": "2026-08-20T00:00:00Z",
            "html_url": "https://github.com/rust-lang/rust/releases/tag/v1.0.0"
        }
    ])
}

fn gh_branches_json() -> serde_json::Value {
    json!([
        {
            "name": "master",
            "commit": { "sha": "c1" },
            "protected": true
        }
    ])
}

fn gh_contributors_json() -> serde_json::Value {
    json!([
        {
            "id": 71,
            "login": "alice",
            "contributions": 120,
            "type": "User",
            "avatar_url": "https://avatars.githubusercontent.com/u/71",
            "html_url": "https://github.com/alice"
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
    server
        .mock_async(|when, then| {
            when.method("GET")
                .path("/repos/rust-lang/rust/issues/comments");
            then.status(200).json_body(gh_comments_json());
        })
        .await;
    server
        .mock_async(|when, then| {
            when.method("GET")
                .path("/repos/rust-lang/rust/pulls/comments");
            then.status(200).json_body(gh_review_comments_json());
        })
        .await;
    server
        .mock_async(|when, then| {
            when.method("GET")
                .path("/repos/rust-lang/rust/pulls/13/reviews");
            then.status(200).json_body(gh_reviews_json());
        })
        .await;
    server
        .mock_async(|when, then| {
            when.method("GET").path("/repos/rust-lang/rust/labels");
            then.status(200).json_body(gh_labels_json());
        })
        .await;
    server
        .mock_async(|when, then| {
            when.method("GET").path("/repos/rust-lang/rust/milestones");
            then.status(200).json_body(gh_milestones_json());
        })
        .await;
    server
        .mock_async(|when, then| {
            when.method("GET").path("/repos/rust-lang/rust/releases");
            then.status(200).json_body(gh_releases_json());
        })
        .await;
    server
        .mock_async(|when, then| {
            when.method("GET").path("/repos/rust-lang/rust/branches");
            then.status(200).json_body(gh_branches_json());
        })
        .await;
    server
        .mock_async(|when, then| {
            when.method("GET")
                .path("/repos/rust-lang/rust/contributors");
            then.status(200).json_body(gh_contributors_json());
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

    assert_eq!(fetched.comments.len(), 1);
    assert_eq!(fetched.comments[0].issue_number, 11);
    assert_eq!(fetched.comments[0].author_login.as_deref(), Some("carol"));

    assert_eq!(fetched.review_comments.len(), 1);
    assert_eq!(fetched.review_comments[0].pull_number, 13);
    assert_eq!(
        fetched.review_comments[0].author_login.as_deref(),
        Some("dave")
    );
    assert_eq!(
        fetched.review_comments[0].path.as_deref(),
        Some("src/lib.rs")
    );

    assert_eq!(fetched.reviews.len(), 1);
    assert_eq!(fetched.reviews[0].pull_number, 13);
    assert_eq!(fetched.reviews[0].state, "APPROVED");
    assert_eq!(fetched.reviews[0].author_login.as_deref(), Some("erin"));

    assert_eq!(fetched.labels.len(), 1);
    assert_eq!(fetched.labels[0].name, "bug");
    assert!(fetched.labels[0].is_default);
    assert_eq!(
        fetched.labels[0].description.as_deref(),
        Some("Something is not working")
    );

    assert_eq!(fetched.milestones.len(), 1);
    assert_eq!(fetched.milestones[0].number, 1);
    assert_eq!(fetched.milestones[0].title, "v1.0");
    assert_eq!(fetched.milestones[0].open_issues, 3);
    assert_eq!(fetched.milestones[0].closed_issues, 7);

    assert_eq!(fetched.releases.len(), 1);
    assert_eq!(fetched.releases[0].tag_name, "v1.0.0");
    assert!(!fetched.releases[0].draft);
    assert_eq!(fetched.releases[0].author_login.as_deref(), Some("erin"));

    assert_eq!(fetched.branches.len(), 1);
    assert_eq!(fetched.branches[0].name, "master");
    assert_eq!(fetched.branches[0].commit_sha, "c1");
    assert!(fetched.branches[0].protected);

    assert_eq!(fetched.contributors.len(), 1);
    assert_eq!(fetched.contributors[0].user_id, 71);
    assert_eq!(fetched.contributors[0].login, "alice");
    assert_eq!(fetched.contributors[0].contributions, 120);
    assert_eq!(fetched.contributors[0].user_type, "User");
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
