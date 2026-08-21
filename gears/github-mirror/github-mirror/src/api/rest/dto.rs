use github_mirror_sdk::{
    Branch, Comment, Commit, Issue, Label, Milestone, PullRequest, Release, Repository, Review,
    ReviewComment,
};

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
    pub comments_synced: u64,
    pub review_comments_synced: u64,
    pub reviews_synced: u64,
    pub labels_synced: u64,
    pub milestones_synced: u64,
    pub releases_synced: u64,
    pub branches_synced: u64,
}

impl From<SyncSummary> for SyncSummaryDto {
    fn from(s: SyncSummary) -> Self {
        Self {
            repository: s.repository,
            issues_synced: s.issues_synced,
            pull_requests_synced: s.pull_requests_synced,
            commits_synced: s.commits_synced,
            comments_synced: s.comments_synced,
            review_comments_synced: s.review_comments_synced,
            reviews_synced: s.reviews_synced,
            labels_synced: s.labels_synced,
            milestones_synced: s.milestones_synced,
            releases_synced: s.releases_synced,
            branches_synced: s.branches_synced,
        }
    }
}

/// A mirrored GitHub issue/PR comment as served by the read API.
#[derive(Debug)]
#[toolkit_macros::api_dto(response)]
pub struct CommentDto {
    pub id: i64,
    pub repo_id: i64,
    pub issue_number: i64,
    pub author_login: Option<String>,
    pub body: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    pub html_url: Option<String>,
}

impl From<Comment> for CommentDto {
    fn from(c: Comment) -> Self {
        Self {
            id: c.id,
            repo_id: c.repo_id,
            issue_number: c.issue_number,
            author_login: c.author_login,
            body: c.body,
            created_at: c.created_at,
            updated_at: c.updated_at,
            html_url: c.html_url,
        }
    }
}

/// A mirrored GitHub PR review comment as served by the read API.
#[derive(Debug)]
#[toolkit_macros::api_dto(response)]
pub struct ReviewCommentDto {
    pub id: i64,
    pub repo_id: i64,
    pub pull_number: i64,
    pub author_login: Option<String>,
    pub body: Option<String>,
    pub path: Option<String>,
    pub diff_hunk: Option<String>,
    pub in_reply_to_id: Option<i64>,
    pub commit_id: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    pub html_url: Option<String>,
}

impl From<ReviewComment> for ReviewCommentDto {
    fn from(c: ReviewComment) -> Self {
        Self {
            id: c.id,
            repo_id: c.repo_id,
            pull_number: c.pull_number,
            author_login: c.author_login,
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
}

/// A mirrored GitHub PR review as served by the read API.
#[derive(Debug)]
#[toolkit_macros::api_dto(response)]
pub struct ReviewDto {
    pub id: i64,
    pub repo_id: i64,
    pub pull_number: i64,
    pub author_login: Option<String>,
    pub state: String,
    pub body: Option<String>,
    pub commit_id: Option<String>,
    pub submitted_at: Option<String>,
    pub html_url: Option<String>,
}

impl From<Review> for ReviewDto {
    fn from(r: Review) -> Self {
        Self {
            id: r.id,
            repo_id: r.repo_id,
            pull_number: r.pull_number,
            author_login: r.author_login,
            state: r.state,
            body: r.body,
            commit_id: r.commit_id,
            submitted_at: r.submitted_at,
            html_url: r.html_url,
        }
    }
}

/// A mirrored GitHub label as served by the read API.
#[derive(Debug)]
#[toolkit_macros::api_dto(response)]
pub struct LabelDto {
    pub id: i64,
    pub repo_id: i64,
    pub name: String,
    pub color: String,
    pub is_default: bool,
    pub description: Option<String>,
}

impl From<Label> for LabelDto {
    fn from(l: Label) -> Self {
        Self {
            id: l.id,
            repo_id: l.repo_id,
            name: l.name,
            color: l.color,
            is_default: l.is_default,
            description: l.description,
        }
    }
}

/// A mirrored GitHub milestone as served by the read API.
#[derive(Debug)]
#[toolkit_macros::api_dto(response)]
pub struct MilestoneDto {
    pub id: i64,
    pub repo_id: i64,
    pub number: i64,
    pub title: String,
    pub state: String,
    pub description: Option<String>,
    pub open_issues: i64,
    pub closed_issues: i64,
    pub due_on: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    pub closed_at: Option<String>,
    pub html_url: Option<String>,
}

impl From<Milestone> for MilestoneDto {
    fn from(m: Milestone) -> Self {
        Self {
            id: m.id,
            repo_id: m.repo_id,
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
}

/// A mirrored GitHub release as served by the read API.
#[derive(Debug)]
#[toolkit_macros::api_dto(response)]
pub struct ReleaseDto {
    pub id: i64,
    pub repo_id: i64,
    pub tag_name: String,
    pub name: Option<String>,
    pub draft: bool,
    pub prerelease: bool,
    pub body: Option<String>,
    pub author_login: Option<String>,
    pub created_at: String,
    pub published_at: Option<String>,
    pub html_url: Option<String>,
}

impl From<Release> for ReleaseDto {
    fn from(r: Release) -> Self {
        Self {
            id: r.id,
            repo_id: r.repo_id,
            tag_name: r.tag_name,
            name: r.name,
            draft: r.draft,
            prerelease: r.prerelease,
            body: r.body,
            author_login: r.author_login,
            created_at: r.created_at,
            published_at: r.published_at,
            html_url: r.html_url,
        }
    }
}

/// A mirrored GitHub branch head as served by the read API.
#[derive(Debug)]
#[toolkit_macros::api_dto(response)]
pub struct BranchDto {
    pub repo_id: i64,
    pub name: String,
    pub commit_sha: String,
    pub protected: bool,
}

impl From<Branch> for BranchDto {
    fn from(b: Branch) -> Self {
        Self {
            repo_id: b.repo_id,
            name: b.name,
            commit_sha: b.commit_sha,
            protected: b.protected,
        }
    }
}
