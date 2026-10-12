#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use github_mirror::domain::error::DomainError;
use github_mirror::domain::ports::github::{
    ActionsListing, CommitDetail, CommitListing, FetchOptions, GithubPort, IssueDetail,
    IssueDetailWants, IssueListing, ListCursor, MetadataListing, PullDetail, PullListing, RepoRef,
};
use github_mirror::domain::repo::{RepoRecord, SessionStatus, WorkflowJobRecord};
use toolkit_security::AccessScope;
use uuid::Uuid;

const OWNER: &str = "rust-lang";
const NAME: &str = "rust";

/// A GitHub that never answers, but does stop when asked. A run against it
/// only ends because the deadline ends it.
struct NeverAnswers;

#[async_trait]
impl GithubPort for NeverAnswers {
    async fn fetch_repository_metadata(
        &self,
        _owner: &str,
        _name: &str,
        options: &FetchOptions,
    ) -> Result<RepoRecord, DomainError> {
        options.cancel.cancelled().await;
        Err(DomainError::Cancelled)
    }

    async fn list_issues(
        &self,
        _repo: RepoRef<'_>,
        _cursor: ListCursor<'_>,
        _options: &FetchOptions,
    ) -> Result<IssueListing, DomainError> {
        unreachable!("discovery never finishes, so no listing is ever asked for")
    }

    async fn refine_issue(
        &self,
        _repo: RepoRef<'_>,
        _number: i64,
        _wants: IssueDetailWants,
        _options: &FetchOptions,
    ) -> Result<IssueDetail, DomainError> {
        unreachable!("discovery never finishes")
    }

    async fn list_pull_requests(
        &self,
        _repo: RepoRef<'_>,
        _cursor: ListCursor<'_>,
        _options: &FetchOptions,
    ) -> Result<PullListing, DomainError> {
        unreachable!("discovery never finishes")
    }

    async fn refine_pull_request(
        &self,
        _repo: RepoRef<'_>,
        _number: i64,
        _options: &FetchOptions,
    ) -> Result<PullDetail, DomainError> {
        unreachable!("discovery never finishes")
    }

    async fn list_commits(
        &self,
        _repo: RepoRef<'_>,
        _cursor: ListCursor<'_>,
        _options: &FetchOptions,
    ) -> Result<CommitListing, DomainError> {
        unreachable!("discovery never finishes")
    }

    async fn refine_commit(
        &self,
        _repo: RepoRef<'_>,
        _sha: &str,
        _with_ci: bool,
        _options: &FetchOptions,
    ) -> Result<CommitDetail, DomainError> {
        unreachable!("discovery never finishes")
    }

    async fn list_metadata(
        &self,
        _repo: RepoRef<'_>,
        _options: &FetchOptions,
    ) -> Result<MetadataListing, DomainError> {
        unreachable!("discovery never finishes")
    }

    async fn list_actions(
        &self,
        _repo: RepoRef<'_>,
        _options: &FetchOptions,
    ) -> Result<ActionsListing, DomainError> {
        unreachable!("discovery never finishes")
    }

    async fn refine_workflow_run(
        &self,
        _repo: RepoRef<'_>,
        _run_id: i64,
        _options: &FetchOptions,
    ) -> Result<Vec<WorkflowJobRecord>, DomainError> {
        unreachable!("discovery never finishes")
    }

    async fn clear_cache(
        &self,
        _scope: &AccessScope,
        _owner: &str,
        _name: Option<&str>,
        _repo_ids: &[i64],
    ) -> Result<u64, DomainError> {
        unreachable!("this test never clears the cache")
    }
}

/// The deadline is the only thing that ends a run which will not end itself.
/// It stops the work, waits for it to wind down, and records the run as
/// failed with a message naming the budget, so the session says why it ended
/// rather than simply stopping.
#[tokio::test]
async fn a_run_that_will_not_end_is_stopped_at_its_deadline() {
    let ctx = common::caller_in(Uuid::new_v4());
    // Real time, short: the database pool's own acquire timeout is a timer,
    // so a paused clock fires it at once and nothing gets a connection.
    let deadline = Duration::from_millis(300);
    let service = common::service_with_deadline(
        common::inmem_db().await,
        "https://api.github.com",
        Arc::new(NeverAnswers),
        common::enforcer(),
        deadline,
    );
    let mut pump = common::SyncPump::take(&service).await;

    let queued = service
        .enqueue_sync(&ctx, OWNER, NAME, None, false, None)
        .await
        .expect("the sync must queue");

    let started = std::time::Instant::now();
    assert_eq!(pump.drain(&service).await, 1, "the worker must run the job");

    assert!(
        started.elapsed() >= deadline,
        "the run must have been given its whole budget, waited {:?}",
        started.elapsed()
    );

    let session = service
        .get_session(&ctx, queued.session_id)
        .await
        .expect("the session must be readable");

    assert_eq!(
        session.status,
        SessionStatus::Failed,
        "a run stopped by its deadline failed; it was not interrupted by a shutdown"
    );
    let error = session.error.expect("a failed session carries its reason");
    assert!(
        error.contains("ran past its deadline") && error.contains(OWNER),
        "the reason must name the budget it passed: {error}"
    );
    assert_eq!(
        session.progress_percent, 100,
        "the run is over, whatever its outcome"
    );
}
