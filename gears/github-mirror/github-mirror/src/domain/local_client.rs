use std::sync::Arc;

use async_trait::async_trait;
use github_mirror_sdk::{GithubMirrorClientV1, MirrorStatus, Repository, SyncSummary};
use toolkit_canonical_errors::{CanonicalError, resource_error};
use toolkit_macros::domain_model;
use toolkit_odata::{ODataQuery, Page};
use toolkit_security::SecurityContext;

use crate::domain::error::DomainError;
use crate::domain::repo::{
    CommentRepository, CommitRepository, IssueRepository, PullRequestRepository, RepoRepository,
    ReviewCommentRepository, ReviewRepository,
};
use crate::domain::service::Service;

#[resource_error(gts_id!("cf.core.github_mirror.repository.v1~"))]
pub struct RepositoryError;

impl From<DomainError> for CanonicalError {
    // Flat match on the domain enum is the whole point of this conversion;
    // the structured `tracing::*!` macros count toward cognitive complexity
    // but splitting the arms into helpers would just hide the mapping.
    #[allow(clippy::cognitive_complexity)]
    fn from(e: DomainError) -> Self {
        match e {
            DomainError::NotFound => RepositoryError::not_found("Repository not found")
                .with_resource("repository")
                .create(),
            DomainError::Validation { field, message } => RepositoryError::invalid_argument()
                .with_field_violation(field, message, "VALIDATION_ERROR")
                .create(),
            DomainError::Forbidden(msg) => {
                tracing::warn!(msg = %msg, "github-mirror access forbidden");
                RepositoryError::not_found("Repository not found or not accessible")
                    .with_resource("repository")
                    .create()
            }
            DomainError::Internal(msg) => {
                tracing::error!(msg = %msg, "github-mirror internal error");
                CanonicalError::internal(msg).create()
            }
            DomainError::Database(db_err) => {
                tracing::error!(error = ?db_err, "github-mirror database error");
                CanonicalError::internal(db_err.to_string()).create()
            }
        }
    }
}

type SharedService<R, I, P, C, M, V, W> = Arc<Service<R, I, P, C, M, V, W>>;

#[domain_model]
pub struct LocalClient<
    R: RepoRepository + 'static,
    I: IssueRepository + 'static,
    P: PullRequestRepository + 'static,
    C: CommitRepository + 'static,
    M: CommentRepository + 'static,
    V: ReviewCommentRepository + 'static,
    W: ReviewRepository + 'static,
> {
    service: SharedService<R, I, P, C, M, V, W>,
}

impl<
    R: RepoRepository + 'static,
    I: IssueRepository + 'static,
    P: PullRequestRepository + 'static,
    C: CommitRepository + 'static,
    M: CommentRepository + 'static,
    V: ReviewCommentRepository + 'static,
    W: ReviewRepository + 'static,
> LocalClient<R, I, P, C, M, V, W>
{
    #[must_use]
    pub fn new(service: SharedService<R, I, P, C, M, V, W>) -> Self {
        Self { service }
    }
}

#[async_trait]
impl<
    R: RepoRepository + 'static,
    I: IssueRepository + 'static,
    P: PullRequestRepository + 'static,
    C: CommitRepository + 'static,
    M: CommentRepository + 'static,
    V: ReviewCommentRepository + 'static,
    W: ReviewRepository + 'static,
> GithubMirrorClientV1 for LocalClient<R, I, P, C, M, V, W>
{
    async fn status(&self, _ctx: &SecurityContext) -> Result<MirrorStatus, CanonicalError> {
        let status = self.service.status();
        Ok(MirrorStatus {
            gear: status.gear,
            version: status.version,
            api_base_url: status.api_base_url,
        })
    }

    async fn list_repositories(
        &self,
        ctx: &SecurityContext,
        query: ODataQuery,
    ) -> Result<Page<Repository>, CanonicalError> {
        self.service
            .list_repositories(ctx, &query)
            .await
            .map_err(CanonicalError::from)
    }

    async fn sync_repository(
        &self,
        ctx: &SecurityContext,
        owner: &str,
        name: &str,
    ) -> Result<SyncSummary, CanonicalError> {
        let summary = self
            .service
            .sync_repository(ctx, owner, name)
            .await
            .map_err(CanonicalError::from)?;
        Ok(SyncSummary {
            repository: summary.repository,
            issues_synced: summary.issues_synced,
            pull_requests_synced: summary.pull_requests_synced,
            commits_synced: summary.commits_synced,
            comments_synced: summary.comments_synced,
            review_comments_synced: summary.review_comments_synced,
            reviews_synced: summary.reviews_synced,
        })
    }
}
