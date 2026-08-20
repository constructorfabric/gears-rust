use github_mirror_sdk::{Commit, Issue, PullRequest, Repository};

use crate::domain::service::{MirrorStatus, SyncSummary};

#[derive(Debug)]
#[toolkit_macros::api_dto(response)]
pub struct GithubMirrorHealthDto {
    pub gear: String,
    pub version: String,
    pub api_base_url: String,
}

impl From<MirrorStatus> for GithubMirrorHealthDto {
    fn from(status: MirrorStatus) -> Self {
        Self {
            gear: status.gear,
            version: status.version,
            api_base_url: status.api_base_url,
        }
    }
}

/// A mirrored GitHub repository as served by the read API.
#[derive(Debug)]
#[toolkit_macros::api_dto(response)]
pub struct RepositoryDto {
    pub id: i64,
    pub owner: String,
    pub name: String,
    pub full_name: String,
    pub private: bool,
    pub description: Option<String>,
}

impl From<Repository> for RepositoryDto {
    fn from(repo: Repository) -> Self {
        Self {
            id: repo.id,
            owner: repo.owner,
            name: repo.name,
            full_name: repo.full_name,
            private: repo.private,
            description: repo.description,
        }
    }
}

/// A mirrored GitHub issue as served by the read API.
#[derive(Debug)]
#[toolkit_macros::api_dto(response)]
pub struct IssueDto {
    pub id: i64,
    pub repo_id: i64,
    pub number: i64,
    pub title: String,
    pub body: Option<String>,
    pub state: String,
    pub is_pull_request: bool,
    pub created_at: String,
    pub updated_at: String,
    pub closed_at: Option<String>,
    pub html_url: Option<String>,
}

impl From<Issue> for IssueDto {
    fn from(issue: Issue) -> Self {
        Self {
            id: issue.id,
            repo_id: issue.repo_id,
            number: issue.number,
            title: issue.title,
            body: issue.body,
            state: issue.state,
            is_pull_request: issue.is_pull_request,
            created_at: issue.created_at,
            updated_at: issue.updated_at,
            closed_at: issue.closed_at,
            html_url: issue.html_url,
        }
    }
}

/// A mirrored GitHub pull request as served by the read API.
#[derive(Debug)]
#[toolkit_macros::api_dto(response)]
pub struct PullRequestDto {
    pub id: i64,
    pub repo_id: i64,
    pub number: i64,
    pub title: String,
    pub body: Option<String>,
    pub state: String,
    pub draft: bool,
    pub merged: bool,
    pub head_sha: Option<String>,
    pub base_sha: Option<String>,
    pub lines_added: i64,
    pub lines_removed: i64,
    pub created_at: String,
    pub updated_at: String,
    pub closed_at: Option<String>,
    pub merged_at: Option<String>,
}

impl From<PullRequest> for PullRequestDto {
    fn from(pr: PullRequest) -> Self {
        Self {
            id: pr.id,
            repo_id: pr.repo_id,
            number: pr.number,
            title: pr.title,
            body: pr.body,
            state: pr.state,
            draft: pr.draft,
            merged: pr.merged,
            head_sha: pr.head_sha,
            base_sha: pr.base_sha,
            lines_added: pr.lines_added,
            lines_removed: pr.lines_removed,
            created_at: pr.created_at,
            updated_at: pr.updated_at,
            closed_at: pr.closed_at,
            merged_at: pr.merged_at,
        }
    }
}

/// A mirrored GitHub commit as served by the read API.
#[derive(Debug)]
#[toolkit_macros::api_dto(response)]
pub struct CommitDto {
    pub repo_id: i64,
    pub sha: String,
    pub message: String,
    pub author_login: Option<String>,
    pub committer_login: Option<String>,
    pub authored_at: Option<String>,
    pub committed_at: Option<String>,
    pub additions: i64,
    pub deletions: i64,
}

impl From<Commit> for CommitDto {
    fn from(c: Commit) -> Self {
        Self {
            repo_id: c.repo_id,
            sha: c.sha,
            message: c.message,
            author_login: c.author_login,
            committer_login: c.committer_login,
            authored_at: c.authored_at,
            committed_at: c.committed_at,
            additions: c.additions,
            deletions: c.deletions,
        }
    }
}

/// Result of one sync pass, as served by the sync endpoint.
#[derive(Debug)]
#[toolkit_macros::api_dto(response)]
pub struct SyncSummaryDto {
    pub repository: String,
    pub issues_synced: u64,
    pub pull_requests_synced: u64,
    pub commits_synced: u64,
}

impl From<SyncSummary> for SyncSummaryDto {
    fn from(s: SyncSummary) -> Self {
        Self {
            repository: s.repository,
            issues_synced: s.issues_synced,
            pull_requests_synced: s.pull_requests_synced,
            commits_synced: s.commits_synced,
        }
    }
}
