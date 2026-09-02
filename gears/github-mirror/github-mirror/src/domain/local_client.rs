use std::sync::Arc;

use async_trait::async_trait;
use github_mirror_sdk::{GithubMirrorClientV1, MirrorStatus, Repo, SyncSummary};
use toolkit_canonical_errors::{CanonicalError, resource_error};
use toolkit_macros::domain_model;
use toolkit_odata::{ODataQuery, Page};
use toolkit_security::SecurityContext;

use crate::domain::error::DomainError;
use crate::domain::repo::{
    BranchRepository, CheckRunRepository, CommentRepository, CommitCommentRepository,
    CommitFileRepository, CommitRepository, CommitStatusRepository, ContributorRepository,
    DeploymentRepository, IssueEventRepository, IssueReactionRepository, IssueRepository,
    IssueTimelineRepository, LabelRepository, MilestoneRepository, PullRequestCommitRepository,
    PullRequestFileRepository, PullRequestRepository, ReleaseRepository, RepoRepository,
    ReviewCommentRepository, ReviewRepository, ReviewThreadRepository, TagRepository,
    WorkflowJobRepository, WorkflowRunRepository,
};
use crate::domain::service::Service;

#[resource_error(gts_id!("cf.core.github_mirror.repository.v1~"))]
pub struct RepositoryError;

/// What a caller is told about an internal failure. The cause is logged, not
/// returned: the messages name upstream GitHub paths and storage internals.
const INTERNAL_DETAIL: &str = "The mirror could not complete this request";

impl From<DomainError> for CanonicalError {
    // Flat match on the domain enum is the whole point of this conversion;
    // the structured `tracing::*!` macros count toward cognitive complexity
    // but splitting the arms into helpers would just hide the mapping.
    #[allow(clippy::cognitive_complexity)]
    fn from(e: DomainError) -> Self {
        match e {
            DomainError::NotFound => RepositoryError::not_found("Repo not found")
                .with_resource("repository")
                .create(),
            DomainError::Validation { field, message } => RepositoryError::invalid_argument()
                .with_field_violation(field, message, "VALIDATION_ERROR")
                .create(),
            DomainError::AccessLost(msg) => {
                tracing::warn!(msg = %msg, "github-mirror upstream access lost");
                RepositoryError::not_found("Repo not found or not accessible")
                    .with_resource("repository")
                    .create()
            }
            DomainError::Conflict(msg) => RepositoryError::already_exists(msg)
                .with_resource("repository")
                .create(),
            DomainError::Forbidden(msg) => {
                tracing::warn!(msg = %msg, "github-mirror access forbidden");
                RepositoryError::not_found("Repo not found or not accessible")
                    .with_resource("repository")
                    .create()
            }
            // Both arms keep the detail in the log and hand the caller a
            // fixed message: an internal failure's text names upstream
            // GitHub paths and storage internals, and a repository that is
            // private to one tenant should not be inferable from another
            // tenant's error body.
            DomainError::Internal(msg) => {
                tracing::error!(msg = %msg, "github-mirror internal error");
                CanonicalError::internal(INTERNAL_DETAIL).create()
            }
            DomainError::Database(db_err) => {
                tracing::error!(error = ?db_err, "github-mirror database error");
                CanonicalError::internal(INTERNAL_DETAIL).create()
            }
        }
    }
}

type SharedService<R, I, P, C, M, V, W, L, N, E, B, O, F, G, T, D, H, K, X, Y, Z, S, J, Q, U, A> =
    Arc<Service<R, I, P, C, M, V, W, L, N, E, B, O, F, G, T, D, H, K, X, Y, Z, S, J, Q, U, A>>;

// One generic parameter per aggregate repository; the alias is as small
// as the type gets.
#[allow(clippy::type_complexity)]
#[domain_model]
pub struct LocalClient<
    R: RepoRepository + 'static,
    I: IssueRepository + 'static,
    P: PullRequestRepository + 'static,
    C: CommitRepository + 'static,
    M: CommentRepository + 'static,
    V: ReviewCommentRepository + 'static,
    W: ReviewRepository + 'static,
    L: LabelRepository + 'static,
    N: MilestoneRepository + 'static,
    E: ReleaseRepository + 'static,
    B: BranchRepository + 'static,
    O: ContributorRepository + 'static,
    F: WorkflowRunRepository + 'static,
    G: PullRequestFileRepository + 'static,
    T: TagRepository + 'static,
    D: CommitFileRepository + 'static,
    H: ReviewThreadRepository + 'static,
    K: CommitCommentRepository + 'static,
    X: IssueEventRepository + 'static,
    Y: DeploymentRepository + 'static,
    Z: PullRequestCommitRepository + 'static,
    S: CommitStatusRepository + 'static,
    J: WorkflowJobRepository + 'static,
    Q: IssueReactionRepository + 'static,
    U: CheckRunRepository + 'static,
    A: IssueTimelineRepository + 'static,
> {
    service:
        SharedService<R, I, P, C, M, V, W, L, N, E, B, O, F, G, T, D, H, K, X, Y, Z, S, J, Q, U, A>,
}

impl<
    R: RepoRepository + 'static,
    I: IssueRepository + 'static,
    P: PullRequestRepository + 'static,
    C: CommitRepository + 'static,
    M: CommentRepository + 'static,
    V: ReviewCommentRepository + 'static,
    W: ReviewRepository + 'static,
    L: LabelRepository + 'static,
    N: MilestoneRepository + 'static,
    E: ReleaseRepository + 'static,
    B: BranchRepository + 'static,
    O: ContributorRepository + 'static,
    F: WorkflowRunRepository + 'static,
    G: PullRequestFileRepository + 'static,
    T: TagRepository + 'static,
    D: CommitFileRepository + 'static,
    H: ReviewThreadRepository + 'static,
    K: CommitCommentRepository + 'static,
    X: IssueEventRepository + 'static,
    Y: DeploymentRepository + 'static,
    Z: PullRequestCommitRepository + 'static,
    S: CommitStatusRepository + 'static,
    J: WorkflowJobRepository + 'static,
    Q: IssueReactionRepository + 'static,
    U: CheckRunRepository + 'static,
    A: IssueTimelineRepository + 'static,
> LocalClient<R, I, P, C, M, V, W, L, N, E, B, O, F, G, T, D, H, K, X, Y, Z, S, J, Q, U, A>
{
    #[must_use]
    #[allow(clippy::type_complexity)]
    pub fn new(
        service: SharedService<
            R,
            I,
            P,
            C,
            M,
            V,
            W,
            L,
            N,
            E,
            B,
            O,
            F,
            G,
            T,
            D,
            H,
            K,
            X,
            Y,
            Z,
            S,
            J,
            Q,
            U,
            A,
        >,
    ) -> Self {
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
    L: LabelRepository + 'static,
    N: MilestoneRepository + 'static,
    E: ReleaseRepository + 'static,
    B: BranchRepository + 'static,
    O: ContributorRepository + 'static,
    F: WorkflowRunRepository + 'static,
    G: PullRequestFileRepository + 'static,
    T: TagRepository + 'static,
    D: CommitFileRepository + 'static,
    H: ReviewThreadRepository + 'static,
    K: CommitCommentRepository + 'static,
    X: IssueEventRepository + 'static,
    Y: DeploymentRepository + 'static,
    Z: PullRequestCommitRepository + 'static,
    S: CommitStatusRepository + 'static,
    J: WorkflowJobRepository + 'static,
    Q: IssueReactionRepository + 'static,
    U: CheckRunRepository + 'static,
    A: IssueTimelineRepository + 'static,
> GithubMirrorClientV1
    for LocalClient<R, I, P, C, M, V, W, L, N, E, B, O, F, G, T, D, H, K, X, Y, Z, S, J, Q, U, A>
{
    async fn status(&self, _ctx: &SecurityContext) -> Result<MirrorStatus, CanonicalError> {
        let status = self.service.status();
        Ok(MirrorStatus {
            gear: status.gear,
            version: status.version,
            api_base_url: status.api_base_url,
        })
    }

    async fn list_repos(
        &self,
        ctx: &SecurityContext,
        query: ODataQuery,
    ) -> Result<Page<Repo>, CanonicalError> {
        self.service
            .list_repos(ctx, &query)
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
            labels_synced: summary.labels_synced,
            milestones_synced: summary.milestones_synced,
            releases_synced: summary.releases_synced,
            branches_synced: summary.branches_synced,
            contributors_synced: summary.contributors_synced,
            workflow_runs_synced: summary.workflow_runs_synced,
            pull_request_files_synced: summary.pull_request_files_synced,
            tags_synced: summary.tags_synced,
            commit_files_synced: summary.commit_files_synced,
            review_threads_synced: summary.review_threads_synced,
            commit_comments_synced: summary.commit_comments_synced,
            issue_events_synced: summary.issue_events_synced,
            deployments_synced: summary.deployments_synced,
            pull_request_commits_synced: summary.pull_request_commits_synced,
            commit_statuses_synced: summary.commit_statuses_synced,
            workflow_jobs_synced: summary.workflow_jobs_synced,
            issue_reactions_synced: summary.issue_reactions_synced,
            check_runs_synced: summary.check_runs_synced,
            issue_timeline_synced: summary.issue_timeline_synced,
            stale_rows_deleted: summary.stale_rows_deleted,
        })
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn status_of(e: DomainError) -> u16 {
        CanonicalError::from(e).status_code()
    }

    #[test]
    fn every_domain_error_maps_to_its_canonical_status() {
        assert_eq!(status_of(DomainError::NotFound), 404);
        assert_eq!(
            status_of(DomainError::Validation {
                field: "owner".to_owned(),
                message: "empty".to_owned(),
            }),
            400
        );
        // Both "caller lacks rights" and "the mirror's own upstream access
        // is gone" must read as 404: a 403 would confirm the repo exists.
        assert_eq!(status_of(DomainError::forbidden("no scope")), 404);
        assert_eq!(
            status_of(DomainError::AccessLost("token revoked".to_owned())),
            404
        );
        assert_eq!(
            status_of(DomainError::Conflict("sync already running".to_owned())),
            409
        );
        assert_eq!(status_of(DomainError::internal("boom")), 500);
        assert_eq!(
            status_of(DomainError::Database(toolkit_db::DbError::InvalidConfig(
                "bad dsn".to_owned()
            ))),
            500
        );
    }
}
