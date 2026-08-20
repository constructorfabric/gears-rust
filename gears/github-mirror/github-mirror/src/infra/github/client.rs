use async_trait::async_trait;
use serde::Deserialize;

use crate::domain::error::DomainError;
use crate::domain::ports::github::{FetchedRepository, GithubPort};
use crate::domain::repo::{CommitRecord, IssueRecord, PullRequestRecord, RepositoryRecord};

const FIRST_PAGE_SIZE: u32 = 50;
const USER_AGENT: &str = concat!("cf-gears-github-mirror/", env!("CARGO_PKG_VERSION"));

/// Minimal GitHub REST client — increment 1 of gears-rust#4630.
///
/// No conditional requests, pagination, or rate-limit admission yet; those
/// arrive as #4630 completes. The token comes from gear config as a temporary
/// shortcut until credstore integration (#4534).
pub struct GithubClient {
    http: reqwest::Client,
    api_base_url: String,
    token: Option<String>,
}

impl GithubClient {
    /// # Errors
    /// Returns `DomainError::Internal` when the underlying HTTP client cannot
    /// be constructed.
    pub fn new(api_base_url: String, token: Option<String>) -> Result<Self, DomainError> {
        let http = reqwest::Client::builder()
            .user_agent(USER_AGENT)
            .build()
            .map_err(|e| DomainError::internal(format!("failed to build HTTP client: {e}")))?;
        Ok(Self {
            http,
            api_base_url,
            token,
        })
    }

    async fn get_json<T: serde::de::DeserializeOwned>(&self, path: &str) -> Result<T, DomainError> {
        let url = format!("{}{path}", self.api_base_url.trim_end_matches('/'));
        let mut request = self
            .http
            .get(&url)
            .header("Accept", "application/vnd.github+json");
        if let Some(token) = &self.token {
            request = request.bearer_auth(token);
        }

        let response = request
            .send()
            .await
            .map_err(|e| DomainError::internal(format!("GitHub request failed: {e}")))?;

        let status = response.status();
        if status == reqwest::StatusCode::NOT_FOUND {
            return Err(DomainError::NotFound);
        }
        if !status.is_success() {
            return Err(DomainError::internal(format!(
                "GitHub responded with {status} for {path}"
            )));
        }

        response
            .json::<T>()
            .await
            .map_err(|e| DomainError::internal(format!("GitHub response decode failed: {e}")))
    }
}

#[derive(Debug, Deserialize)]
struct GhOwner {
    login: String,
}

#[derive(Debug, Deserialize)]
struct GhRepository {
    id: i64,
    name: String,
    full_name: String,
    owner: GhOwner,
    default_branch: String,
    private: bool,
    pushed_at: Option<String>,
    stargazers_count: i64,
    forks_count: i64,
    description: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GhIssue {
    id: i64,
    number: i64,
    title: String,
    body: Option<String>,
    state: String,
    #[serde(default)]
    pull_request: Option<serde::de::IgnoredAny>,
    created_at: String,
    updated_at: String,
    closed_at: Option<String>,
    html_url: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GhRef {
    sha: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GhPullRequest {
    id: i64,
    number: i64,
    title: String,
    body: Option<String>,
    state: String,
    draft: Option<bool>,
    merged_at: Option<String>,
    head: Option<GhRef>,
    base: Option<GhRef>,
    created_at: String,
    updated_at: String,
    closed_at: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GhCommitPerson {
    date: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GhCommitDetails {
    message: String,
    author: Option<GhCommitPerson>,
    committer: Option<GhCommitPerson>,
}

#[derive(Debug, Deserialize)]
struct GhActor {
    login: String,
}

#[derive(Debug, Deserialize)]
struct GhCommit {
    sha: String,
    commit: GhCommitDetails,
    author: Option<GhActor>,
    committer: Option<GhActor>,
}

fn repository_record(r: GhRepository) -> RepositoryRecord {
    RepositoryRecord {
        id: r.id,
        owner: r.owner.login,
        name: r.name,
        full_name: r.full_name,
        default_branch: r.default_branch,
        private: r.private,
        pushed_at: r.pushed_at,
        stars: r.stargazers_count,
        forks: r.forks_count,
        description: r.description,
    }
}

fn issue_record(repo_id: i64, i: GhIssue) -> IssueRecord {
    IssueRecord {
        id: i.id,
        repo_id,
        number: i.number,
        title: i.title,
        body: i.body,
        state: i.state,
        is_pull_request: i.pull_request.is_some(),
        created_at: i.created_at,
        updated_at: i.updated_at,
        closed_at: i.closed_at,
        html_url: i.html_url,
    }
}

fn pull_request_record(repo_id: i64, p: GhPullRequest) -> PullRequestRecord {
    PullRequestRecord {
        id: p.id,
        repo_id,
        number: p.number,
        title: p.title,
        body: p.body,
        state: p.state,
        draft: p.draft.unwrap_or(false),
        merged: p.merged_at.is_some(),
        head_sha: p.head.and_then(|r| r.sha),
        base_sha: p.base.and_then(|r| r.sha),
        lines_added: 0,
        lines_removed: 0,
        created_at: p.created_at,
        updated_at: p.updated_at,
        closed_at: p.closed_at,
        merged_at: p.merged_at,
    }
}

fn commit_record(repo_id: i64, c: GhCommit) -> CommitRecord {
    CommitRecord {
        repo_id,
        sha: c.sha,
        message: c.commit.message,
        author_login: c.author.map(|a| a.login),
        committer_login: c.committer.map(|a| a.login),
        authored_at: c.commit.author.and_then(|p| p.date),
        committed_at: c.commit.committer.and_then(|p| p.date),
        additions: 0,
        deletions: 0,
    }
}

#[async_trait]
impl GithubPort for GithubClient {
    async fn fetch_repository(
        &self,
        owner: &str,
        name: &str,
    ) -> Result<FetchedRepository, DomainError> {
        let repo: GhRepository = self.get_json(&format!("/repos/{owner}/{name}")).await?;
        let repo_id = repo.id;

        let issues: Vec<GhIssue> = self
            .get_json(&format!(
                "/repos/{owner}/{name}/issues?state=all&per_page={FIRST_PAGE_SIZE}"
            ))
            .await?;
        let pulls: Vec<GhPullRequest> = self
            .get_json(&format!(
                "/repos/{owner}/{name}/pulls?state=all&per_page={FIRST_PAGE_SIZE}"
            ))
            .await?;
        let commits: Vec<GhCommit> = self
            .get_json(&format!(
                "/repos/{owner}/{name}/commits?per_page={FIRST_PAGE_SIZE}"
            ))
            .await?;

        Ok(FetchedRepository {
            repository: repository_record(repo),
            issues: issues
                .into_iter()
                .map(|i| issue_record(repo_id, i))
                .collect(),
            pull_requests: pulls
                .into_iter()
                .map(|p| pull_request_record(repo_id, p))
                .collect(),
            commits: commits
                .into_iter()
                .map(|c| commit_record(repo_id, c))
                .collect(),
        })
    }
}
