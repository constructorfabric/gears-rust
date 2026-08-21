//! Public models for the github-mirror gear.
//!
//! Transport-agnostic data structures defining the contract between the
//! github-mirror gear and its consumers. All models carry `#[domain_model]`
//! so infrastructure types cannot leak into them.

use toolkit_macros::domain_model;

/// Runtime identity of the mirror gear.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MirrorStatus {
    pub gear: String,
    pub version: String,
    pub api_base_url: String,
}

/// A mirrored GitHub repository (minimal read-slice shape).
///
/// Field set intentionally starts small — it mirrors what the first
/// read-slice (`GET /github-mirror/v1/repos`) serves from the local store
/// and grows as further entity fields are ported.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Repository {
    /// GitHub's numeric repository id.
    pub id: i64,
    pub owner: String,
    pub name: String,
    pub full_name: String,
    pub private: bool,
    pub description: Option<String>,
}

/// A mirrored GitHub issue (read-slice shape).
///
/// GitHub's API treats pull requests as issues too — `is_pull_request`
/// carries that distinction so consumers can filter either way.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Issue {
    /// GitHub's numeric issue id.
    pub id: i64,
    /// Owning repository's GitHub id.
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

/// A mirrored GitHub pull request (read-slice shape).
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PullRequest {
    /// GitHub's numeric pull-request id.
    pub id: i64,
    /// Owning repository's GitHub id.
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

/// A mirrored GitHub commit (read-slice shape).
///
/// Keyed by `(repo_id, sha)` — commits have no numeric GitHub id.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Commit {
    /// Owning repository's GitHub id.
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

/// Result of one sync pass.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncSummary {
    pub repository: String,
    pub issues_synced: u64,
    pub pull_requests_synced: u64,
    pub commits_synced: u64,
    pub comments_synced: u64,
    pub review_comments_synced: u64,
}

/// A mirrored GitHub issue/PR comment (read-slice shape).
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Comment {
    /// GitHub's numeric comment id.
    pub id: i64,
    /// Owning repository's GitHub id.
    pub repo_id: i64,
    /// Owning issue/PR number.
    pub issue_number: i64,
    pub author_login: Option<String>,
    pub body: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    pub html_url: Option<String>,
}

/// A mirrored GitHub pull-request review comment (inline diff comment).
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewComment {
    /// GitHub's numeric review-comment id.
    pub id: i64,
    /// Owning repository's GitHub id.
    pub repo_id: i64,
    /// Owning pull-request number.
    pub pull_number: i64,
    pub author_login: Option<String>,
    pub body: Option<String>,
    /// File path the comment is attached to.
    pub path: Option<String>,
    pub diff_hunk: Option<String>,
    /// Id of the comment this one replies to.
    pub in_reply_to_id: Option<i64>,
    /// Commit SHA the comment pins to.
    pub commit_id: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    pub html_url: Option<String>,
}
