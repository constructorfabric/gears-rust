use async_trait::async_trait;
use toolkit_macros::domain_model;

use crate::domain::error::DomainError;
use crate::domain::repo::{
    BranchRecord, CheckRunRecord, CommentRecord, CommitCommentRecord, CommitFileRecord,
    CommitRecord, CommitStatusRecord, ContributorRecord, DeploymentRecord, IssueEventRecord,
    IssueReactionRecord, IssueRecord, IssueTimelineEventRecord, LabelRecord, MilestoneRecord,
    PullRequestCommitRecord, PullRequestFileRecord, PullRequestRecord, ReleaseRecord, RepoRecord,
    ReviewCommentRecord, ReviewRecord, ReviewThreadRecord, TagRecord, WorkflowJobRecord,
    WorkflowRunRecord,
};

/// Which top-level listings this fetch walked to their final page.
///
/// Deletion reconciliation may only run against a listing that is provably
/// complete: "absent from a truncated page" says nothing about existence.
/// On this increment the client fetches first pages only, so a listing is
/// complete exactly when its response advertises no next page.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[allow(clippy::struct_excessive_bools)]
pub struct ListingCompleteness {
    pub issues: bool,
    pub pull_requests: bool,
    pub commits: bool,
    pub comments: bool,
    pub review_comments: bool,
    pub labels: bool,
    pub milestones: bool,
    pub releases: bool,
    pub branches: bool,
    pub tags: bool,
    pub contributors: bool,
    pub issue_events: bool,
    pub commit_comments: bool,
    pub deployments: bool,
}

impl ListingCompleteness {
    /// Every listing complete — what a fake in tests reports.
    #[must_use]
    pub fn all_complete() -> Self {
        Self {
            issues: true,
            pull_requests: true,
            commits: true,
            comments: true,
            review_comments: true,
            labels: true,
            milestones: true,
            releases: true,
            branches: true,
            tags: true,
            contributors: true,
            issue_events: true,
            commit_comments: true,
            deployments: true,
        }
    }
}

/// What one sync-lite pass fetched from GitHub for a repository.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FetchedRepository {
    pub repository: RepoRecord,
    /// Which listings below are complete and therefore safe to reconcile
    /// deletions against.
    pub complete: ListingCompleteness,
    pub issues: Vec<IssueRecord>,
    pub pull_requests: Vec<PullRequestRecord>,
    pub commits: Vec<CommitRecord>,
    pub comments: Vec<CommentRecord>,
    pub review_comments: Vec<ReviewCommentRecord>,
    pub reviews: Vec<ReviewRecord>,
    pub labels: Vec<LabelRecord>,
    pub milestones: Vec<MilestoneRecord>,
    pub releases: Vec<ReleaseRecord>,
    pub branches: Vec<BranchRecord>,
    pub contributors: Vec<ContributorRecord>,
    pub workflow_runs: Vec<WorkflowRunRecord>,
    pub pull_request_files: Vec<PullRequestFileRecord>,
    pub tags: Vec<TagRecord>,
    pub commit_files: Vec<CommitFileRecord>,
    pub review_threads: Vec<ReviewThreadRecord>,
    pub commit_comments: Vec<CommitCommentRecord>,
    pub issue_events: Vec<IssueEventRecord>,
    pub deployments: Vec<DeploymentRecord>,
    pub pull_request_commits: Vec<PullRequestCommitRecord>,
    pub commit_statuses: Vec<CommitStatusRecord>,
    pub workflow_jobs: Vec<WorkflowJobRecord>,
    pub issue_reactions: Vec<IssueReactionRecord>,
    pub check_runs: Vec<CheckRunRecord>,
    pub issue_timeline: Vec<IssueTimelineEventRecord>,
}

/// Outbound port to GitHub's REST API (implemented in `infra/github`).
///
/// Increment 1 of gears-rust#4630: fetches a repository and the first page of
/// its issues, pull requests, and commits. Conditional requests, pagination,
/// and rate-limit admission arrive as that issue completes.
#[async_trait]
pub trait GithubPort: Send + Sync {
    async fn fetch_repository(
        &self,
        owner: &str,
        name: &str,
    ) -> Result<FetchedRepository, DomainError>;
}
