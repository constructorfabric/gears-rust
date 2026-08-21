use async_trait::async_trait;
use toolkit_macros::domain_model;

use crate::domain::error::DomainError;
use crate::domain::repo::{
    CommentRecord, CommitRecord, IssueRecord, LabelRecord, PullRequestRecord, RepositoryRecord,
    ReviewCommentRecord, ReviewRecord,
};

/// What one sync-lite pass fetched from GitHub for a repository.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FetchedRepository {
    pub repository: RepositoryRecord,
    pub issues: Vec<IssueRecord>,
    pub pull_requests: Vec<PullRequestRecord>,
    pub commits: Vec<CommitRecord>,
    pub comments: Vec<CommentRecord>,
    pub review_comments: Vec<ReviewCommentRecord>,
    pub reviews: Vec<ReviewRecord>,
    pub labels: Vec<LabelRecord>,
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
