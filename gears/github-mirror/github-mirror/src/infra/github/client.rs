use async_trait::async_trait;
use serde::Deserialize;

use crate::domain::error::DomainError;
use crate::domain::ports::github::{FetchedRepository, GithubPort};
use crate::domain::repo::{
    BranchRecord, CommentRecord, CommitRecord, IssueRecord, LabelRecord, MilestoneRecord,
    PullRequestRecord, ReleaseRecord, RepositoryRecord, ReviewCommentRecord, ReviewRecord,
};

const FIRST_PAGE_SIZE: u32 = 50;
/// GitHub serves reviews only per pull request, so sync-lite fetches them
/// for the first few pulls of the page to keep the call count bounded.
const REVIEW_SYNC_PULL_CAP: usize = 10;
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
struct GhComment {
    id: i64,
    #[serde(default)]
    user: Option<GhActor>,
    body: Option<String>,
    created_at: String,
    updated_at: String,
    html_url: Option<String>,
    issue_url: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GhReviewComment {
    id: i64,
    #[serde(default)]
    user: Option<GhActor>,
    body: Option<String>,
    path: Option<String>,
    diff_hunk: Option<String>,
    in_reply_to_id: Option<i64>,
    commit_id: Option<String>,
    created_at: String,
    updated_at: String,
    html_url: Option<String>,
    pull_request_url: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GhLabel {
    id: i64,
    name: String,
    color: String,
    #[serde(default, rename = "default")]
    is_default: bool,
    description: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GhMilestone {
    id: i64,
    number: i64,
    title: String,
    state: String,
    description: Option<String>,
    open_issues: i64,
    closed_issues: i64,
    due_on: Option<String>,
    created_at: String,
    updated_at: String,
    closed_at: Option<String>,
    html_url: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GhRelease {
    id: i64,
    tag_name: String,
    name: Option<String>,
    draft: bool,
    prerelease: bool,
    body: Option<String>,
    #[serde(default)]
    author: Option<GhActor>,
    created_at: String,
    published_at: Option<String>,
    html_url: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GhBranchCommit {
    sha: String,
}

#[derive(Debug, Deserialize)]
struct GhBranch {
    name: String,
    commit: GhBranchCommit,
    #[serde(default)]
    protected: bool,
}

#[derive(Debug, Deserialize)]
struct GhReview {
    id: i64,
    #[serde(default)]
    user: Option<GhActor>,
    state: String,
    body: Option<String>,
    commit_id: Option<String>,
    submitted_at: Option<String>,
    html_url: Option<String>,
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

fn issue_number_from_url(issue_url: Option<&str>) -> i64 {
    issue_url
        .and_then(|u| u.rsplit('/').next())
        .and_then(|n| n.parse().ok())
        .unwrap_or(0)
}

fn comment_record(repo_id: i64, c: GhComment) -> CommentRecord {
    let issue_number = issue_number_from_url(c.issue_url.as_deref());
    CommentRecord {
        id: c.id,
        repo_id,
        issue_number,
        author_login: c.user.map(|u| u.login),
        body: c.body,
        created_at: c.created_at,
        updated_at: c.updated_at,
        html_url: c.html_url,
    }
}

fn review_comment_record(repo_id: i64, c: GhReviewComment) -> ReviewCommentRecord {
    let pull_number = issue_number_from_url(c.pull_request_url.as_deref());
    ReviewCommentRecord {
        id: c.id,
        repo_id,
        pull_number,
        author_login: c.user.map(|u| u.login),
        body: c.body,
        path: c.path,
        diff_hunk: c.diff_hunk,
        in_reply_to_id: c.in_reply_to_id,
        commit_id: c.commit_id,
        created_at: c.created_at,
        updated_at: c.updated_at,
        html_url: c.html_url,
    }
}

fn label_record(repo_id: i64, l: GhLabel) -> LabelRecord {
    LabelRecord {
        id: l.id,
        repo_id,
        name: l.name,
        color: l.color,
        is_default: l.is_default,
        description: l.description,
    }
}

fn milestone_record(repo_id: i64, m: GhMilestone) -> MilestoneRecord {
    MilestoneRecord {
        id: m.id,
        repo_id,
        number: m.number,
        title: m.title,
        state: m.state,
        description: m.description,
        open_issues: m.open_issues,
        closed_issues: m.closed_issues,
        due_on: m.due_on,
        created_at: m.created_at,
        updated_at: m.updated_at,
        closed_at: m.closed_at,
        html_url: m.html_url,
    }
}

fn release_record(repo_id: i64, r: GhRelease) -> ReleaseRecord {
    ReleaseRecord {
        id: r.id,
        repo_id,
        tag_name: r.tag_name,
        name: r.name,
        draft: r.draft,
        prerelease: r.prerelease,
        body: r.body,
        author_login: r.author.map(|a| a.login),
        created_at: r.created_at,
        published_at: r.published_at,
        html_url: r.html_url,
    }
}

fn branch_record(repo_id: i64, b: GhBranch) -> BranchRecord {
    BranchRecord {
        repo_id,
        name: b.name,
        commit_sha: b.commit.sha,
        protected: b.protected,
    }
}

fn review_record(repo_id: i64, pull_number: i64, r: GhReview) -> ReviewRecord {
    ReviewRecord {
        id: r.id,
        repo_id,
        pull_number,
        author_login: r.user.map(|u| u.login),
        state: r.state,
        body: r.body,
        commit_id: r.commit_id,
        submitted_at: r.submitted_at,
        html_url: r.html_url,
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
        let comments: Vec<GhComment> = self
            .get_json(&format!(
                "/repos/{owner}/{name}/issues/comments?per_page={FIRST_PAGE_SIZE}"
            ))
            .await?;
        let review_comments: Vec<GhReviewComment> = self
            .get_json(&format!(
                "/repos/{owner}/{name}/pulls/comments?per_page={FIRST_PAGE_SIZE}"
            ))
            .await?;

        let labels: Vec<GhLabel> = self
            .get_json(&format!(
                "/repos/{owner}/{name}/labels?per_page={FIRST_PAGE_SIZE}"
            ))
            .await?;

        let milestones: Vec<GhMilestone> = self
            .get_json(&format!(
                "/repos/{owner}/{name}/milestones?state=all&per_page={FIRST_PAGE_SIZE}"
            ))
            .await?;

        let releases: Vec<GhRelease> = self
            .get_json(&format!(
                "/repos/{owner}/{name}/releases?per_page={FIRST_PAGE_SIZE}"
            ))
            .await?;

        let branches: Vec<GhBranch> = self
            .get_json(&format!(
                "/repos/{owner}/{name}/branches?per_page={FIRST_PAGE_SIZE}"
            ))
            .await?;

        let mut reviews: Vec<ReviewRecord> = Vec::new();
        for pull in pulls.iter().take(REVIEW_SYNC_PULL_CAP) {
            let page: Vec<GhReview> = self
                .get_json(&format!(
                    "/repos/{owner}/{name}/pulls/{}/reviews?per_page={FIRST_PAGE_SIZE}",
                    pull.number
                ))
                .await?;
            reviews.extend(
                page.into_iter()
                    .map(|r| review_record(repo_id, pull.number, r)),
            );
        }

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
            comments: comments
                .into_iter()
                .map(|c| comment_record(repo_id, c))
                .collect(),
            review_comments: review_comments
                .into_iter()
                .map(|c| review_comment_record(repo_id, c))
                .collect(),
            reviews,
            labels: labels
                .into_iter()
                .map(|l| label_record(repo_id, l))
                .collect(),
            milestones: milestones
                .into_iter()
                .map(|m| milestone_record(repo_id, m))
                .collect(),
            releases: releases
                .into_iter()
                .map(|r| release_record(repo_id, r))
                .collect(),
            branches: branches
                .into_iter()
                .map(|b| branch_record(repo_id, b))
                .collect(),
        })
    }
}
