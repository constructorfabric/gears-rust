#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use github_mirror::domain::error::DomainError;
use github_mirror::domain::ports::github::{
    FetchOptions, ForceMode, GithubPort, IssueDetailWants, RepoRef,
};
use github_mirror::domain::scope::ScopeConfig;
use github_mirror::infra::github::cache::NoCache;
use github_mirror::infra::github::client::GithubClient;
use github_mirror::infra::github::rate_limit::TokenFingerprint;
use httpmock::MockServer;
use serde_json::json;
use toolkit_security::AccessScope;
use toolkit_utils::SecretString;

const PRIVATE: RepoRef<'static> = RepoRef {
    owner: "acme",
    name: "private",
    repo_id: 7,
};

const REACTIONS_ONLY: IssueDetailWants = IssueDetailWants {
    reactions: true,
    timeline: false,
};

fn client(server: &MockServer, tokens: &[&str]) -> GithubClient {
    GithubClient::with_tokens(
        server.base_url(),
        tokens
            .iter()
            .map(|token| SecretString::new(*token))
            .collect(),
        Arc::new(NoCache),
    )
    .expect("client must build")
}

fn options() -> FetchOptions {
    options_for(uuid::Uuid::new_v4())
}

fn options_for(tenant_id: uuid::Uuid) -> FetchOptions {
    FetchOptions {
        tenant_id,
        access_scope: AccessScope::for_tenant(tenant_id),
        scope: ScopeConfig::default(),
        force: ForceMode::None,
        since: None,
        cancel: tokio_util::sync::CancellationToken::new(),
        telemetry: Arc::default(),
        public_repo: Arc::default(),
        token_access: Arc::default(),
        max_concurrent_tasks: None,
    }
}

fn label(token: &str) -> String {
    TokenFingerprint::of(token).short().to_owned()
}

fn repo_json(name: &str, private: bool) -> serde_json::Value {
    json!({
        "id": 7,
        "name": name,
        "full_name": format!("acme/{name}"),
        "owner": { "login": "acme" },
        "default_branch": "main",
        "private": private,
        "pushed_at": "2026-11-01T00:00:00Z",
        "stargazers_count": 0,
        "forks_count": 0,
        "clone_url": format!("https://github.com/acme/{name}.git"),
        "description": null
    })
}

#[tokio::test]
async fn a_repo_hidden_from_the_first_token_is_fetched_with_the_next_one() {
    let server = MockServer::start_async().await;
    let refused = server
        .mock_async(|when, then| {
            when.method("GET")
                .path("/repos/acme/private")
                .header("authorization", "Bearer tok-a");
            then.status(404);
        })
        .await;
    let answered = server
        .mock_async(|when, then| {
            when.method("GET")
                .path("/repos/acme/private")
                .header("authorization", "Bearer tok-b");
            then.status(200).json_body(repo_json("private", true));
        })
        .await;
    let options = options();

    let repo = client(&server, &["tok-a", "tok-b"])
        .fetch_repository_metadata("acme", "private", &options)
        .await
        .expect("tok-b sees the repository");

    assert_eq!(repo.full_name, "acme/private");
    refused.assert_async().await;
    answered.assert_async().await;
    assert_eq!(options.token_access.sees(&label("tok-a")), Some(false));
    assert_eq!(options.token_access.sees(&label("tok-b")), Some(true));
}

#[tokio::test]
async fn a_repo_hidden_from_every_token_is_not_found() {
    let server = MockServer::start_async().await;
    let first = server
        .mock_async(|when, then| {
            when.method("GET")
                .path("/repos/acme/gone")
                .header("authorization", "Bearer tok-a");
            then.status(404);
        })
        .await;
    let second = server
        .mock_async(|when, then| {
            when.method("GET")
                .path("/repos/acme/gone")
                .header("authorization", "Bearer tok-b");
            then.status(404);
        })
        .await;

    let result = client(&server, &["tok-a", "tok-b"])
        .fetch_repository_metadata("acme", "gone", &options())
        .await;

    assert!(matches!(result, Err(DomainError::NotFound)), "{result:?}");
    first.assert_async().await;
    second.assert_async().await;
}

#[tokio::test]
async fn a_404_inside_a_repo_the_token_sees_is_a_real_one() {
    let server = MockServer::start_async().await;
    let reactions = server
        .mock_async(|when, then| {
            when.method("GET")
                .path("/repos/acme/private/issues/42/reactions");
            then.status(404);
        })
        .await;
    let repo = server
        .mock_async(|when, then| {
            when.method("GET").path("/repos/acme/private");
            then.status(200).json_body(repo_json("private", true));
        })
        .await;
    let options = options();

    let result = client(&server, &["tok-a"])
        .refine_issue(PRIVATE, 42, REACTIONS_ONLY, &options)
        .await;

    assert!(matches!(result, Err(DomainError::NotFound)), "{result:?}");
    reactions.assert_async().await;
    repo.assert_async().await;
    assert_eq!(options.token_access.sees(&label("tok-a")), Some(true));
}

#[tokio::test]
async fn a_404_inside_a_repo_hidden_from_the_token_is_retried_with_the_next_one() {
    let server = MockServer::start_async().await;
    let refused = server
        .mock_async(|when, then| {
            when.method("GET")
                .path("/repos/acme/private/issues/42/reactions")
                .header("authorization", "Bearer tok-a");
            then.status(404);
        })
        .await;
    let repo_refused = server
        .mock_async(|when, then| {
            when.method("GET")
                .path("/repos/acme/private")
                .header("authorization", "Bearer tok-a");
            then.status(404);
        })
        .await;
    let answered = server
        .mock_async(|when, then| {
            when.method("GET")
                .path("/repos/acme/private/issues/42/reactions")
                .header("authorization", "Bearer tok-b");
            then.status(200).json_body(json!([]));
        })
        .await;
    let options = options();

    let detail = client(&server, &["tok-a", "tok-b"])
        .refine_issue(PRIVATE, 42, REACTIONS_ONLY, &options)
        .await
        .expect("tok-b sees the repository");

    assert!(detail.reactions.is_empty());
    refused.assert_async().await;
    repo_refused.assert_async().await;
    answered.assert_async().await;
    assert_eq!(options.token_access.sees(&label("tok-a")), Some(false));
}

#[tokio::test]
async fn a_404_on_a_public_repo_is_real_without_asking_about_the_repo() {
    let server = MockServer::start_async().await;
    let refused = server
        .mock_async(|when, then| {
            when.method("GET")
                .path("/repos/acme/private/issues/42/reactions")
                .header("authorization", "Bearer tok-a");
            then.status(404);
        })
        .await;
    let other_token = server
        .mock_async(|when, then| {
            when.method("GET")
                .path("/repos/acme/private/issues/42/reactions")
                .header("authorization", "Bearer tok-b");
            then.status(200).json_body(json!([]));
        })
        .await;
    let repo = server
        .mock_async(|when, then| {
            when.method("GET").path("/repos/acme/private");
            then.status(200).json_body(repo_json("private", false));
        })
        .await;
    let options = options();
    options.public_repo.set(true).unwrap();

    let result = client(&server, &["tok-a", "tok-b"])
        .refine_issue(PRIVATE, 42, REACTIONS_ONLY, &options)
        .await;

    assert!(matches!(result, Err(DomainError::NotFound)), "{result:?}");
    refused.assert_async().await;
    assert_eq!(other_token.calls_async().await, 0);
    assert_eq!(repo.calls_async().await, 0);
}

#[tokio::test]
async fn a_classic_token_without_the_repo_scope_is_left_out_on_a_private_repo() {
    let server = MockServer::start_async().await;
    server
        .mock_async(|when, then| {
            when.method("GET")
                .path("/repos/acme/tools")
                .header("authorization", "Bearer tok-a");
            then.status(200)
                .header("X-OAuth-Scopes", "public_repo, gist")
                .header("X-RateLimit-Limit", "5000")
                .header("X-RateLimit-Remaining", "4999")
                .json_body(repo_json("tools", false));
        })
        .await;
    server
        .mock_async(|when, then| {
            when.method("GET")
                .path("/repos/acme/tools")
                .header("authorization", "Bearer tok-b");
            then.status(200)
                .header("X-OAuth-Scopes", "repo, read:org")
                .header("X-RateLimit-Limit", "5000")
                .header("X-RateLimit-Remaining", "100")
                .json_body(repo_json("tools", false));
        })
        .await;
    let without_repo_scope = server
        .mock_async(|when, then| {
            when.method("GET")
                .path("/repos/acme/private/issues/1/reactions")
                .header("authorization", "Bearer tok-a");
            then.status(200).json_body(json!([]));
        })
        .await;
    let with_repo_scope = server
        .mock_async(|when, then| {
            when.method("GET")
                .path("/repos/acme/private/issues/1/reactions")
                .header("authorization", "Bearer tok-b");
            then.status(200).json_body(json!([]));
        })
        .await;
    let client = client(&server, &["tok-a", "tok-b"]);
    let tenant_id = uuid::Uuid::new_v4();
    let warm_up = options_for(tenant_id);
    for _ in 0..2 {
        client
            .fetch_repository_metadata("acme", "tools", &warm_up)
            .await
            .expect("each token answers once");
    }
    let options = options_for(tenant_id);
    options.public_repo.set(false).unwrap();

    client
        .refine_issue(PRIVATE, 1, REACTIONS_ONLY, &options)
        .await
        .expect("tok-b has the repo scope");

    assert_eq!(without_repo_scope.calls_async().await, 0);
    with_repo_scope.assert_async().await;
}

#[tokio::test]
async fn a_token_github_answers_401_is_rejected() {
    let server = MockServer::start_async().await;
    let refused = server
        .mock_async(|when, then| {
            when.method("GET")
                .path("/rate_limit")
                .header("authorization", "Bearer tok-revoked");
            then.status(401);
        })
        .await;
    let accepted = server
        .mock_async(|when, then| {
            when.method("GET")
                .path("/rate_limit")
                .header("authorization", "Bearer tok-good");
            then.status(200).json_body(json!({ "resources": {} }));
        })
        .await;
    let client = client(&server, &[]);

    assert!(
        client
            .rejects_token(&SecretString::new("tok-revoked"))
            .await
            .unwrap()
    );
    assert!(
        !client
            .rejects_token(&SecretString::new("tok-good"))
            .await
            .unwrap()
    );
    refused.assert_async().await;
    accepted.assert_async().await;
}

#[tokio::test]
async fn a_token_github_cannot_check_is_not_rejected() {
    let server = MockServer::start_async().await;
    let unavailable = server
        .mock_async(|when, then| {
            when.method("GET").path("/rate_limit");
            then.status(503);
        })
        .await;

    assert!(
        !client(&server, &[])
            .rejects_token(&SecretString::new("tok-a"))
            .await
            .unwrap()
    );
    unavailable.assert_async().await;
}
