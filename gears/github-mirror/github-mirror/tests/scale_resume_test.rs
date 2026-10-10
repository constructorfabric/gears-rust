#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use github_mirror::domain::error::DomainError;
use github_mirror::domain::ports::github::{
    ActionsListing, CommitDetail, CommitListing, FetchOptions, FetchedRepository, ForceMode,
    GithubPort, IssueDetail, IssueDetailWants, IssueListing, ListCursor, MetadataListing,
    PullDetail, PullListing, RepoRef,
};
use github_mirror::domain::repo::{
    CommentRecord, CommitRecord, IssueRecord, PageWindow, PullRequestRecord, RepoRecord,
    RepoRunStatus, SessionStatus, WorkflowJobRecord,
};
use tokio_util::sync::CancellationToken;
use toolkit_odata::ODataQuery;
use toolkit_security::{AccessScope, SecurityContext};
use uuid::Uuid;

const OWNER: &str = "rust-lang";
const NAME: &str = "rust";
const REPO_ID: i64 = 42;

const ISSUES: i64 = 50_000;
const COMMENTS: i64 = 30_000;
const PULL_REQUESTS: i64 = 20_000;
const COMMITS: i64 = 10_000;

/// Trips `cancel` on the `after`-th refinement call, so a run is stopped part
/// way through writing entities rather than before any were written.
struct StopsMidRefinement {
    inner: common::FakeGithub,
    cancel: CancellationToken,
    after: usize,
    refinements: AtomicUsize,
}

impl StopsMidRefinement {
    fn count(&self) {
        if self.refinements.fetch_add(1, Ordering::SeqCst) + 1 == self.after {
            self.cancel.cancel();
        }
    }
}

#[async_trait]
impl GithubPort for StopsMidRefinement {
    async fn fetch_repository_metadata(
        &self,
        owner: &str,
        name: &str,
        options: &FetchOptions,
    ) -> Result<RepoRecord, DomainError> {
        self.inner
            .fetch_repository_metadata(owner, name, options)
            .await
    }

    async fn list_issues(
        &self,
        repo: RepoRef<'_>,
        cursor: ListCursor<'_>,
        options: &FetchOptions,
    ) -> Result<IssueListing, DomainError> {
        self.inner.list_issues(repo, cursor, options).await
    }

    async fn refine_issue(
        &self,
        repo: RepoRef<'_>,
        number: i64,
        wants: IssueDetailWants,
        options: &FetchOptions,
    ) -> Result<IssueDetail, DomainError> {
        self.count();
        self.inner.refine_issue(repo, number, wants, options).await
    }

    async fn list_pull_requests(
        &self,
        repo: RepoRef<'_>,
        cursor: ListCursor<'_>,
        options: &FetchOptions,
    ) -> Result<PullListing, DomainError> {
        self.inner.list_pull_requests(repo, cursor, options).await
    }

    async fn refine_pull_request(
        &self,
        repo: RepoRef<'_>,
        number: i64,
        options: &FetchOptions,
    ) -> Result<PullDetail, DomainError> {
        self.count();
        self.inner.refine_pull_request(repo, number, options).await
    }

    async fn list_commits(
        &self,
        repo: RepoRef<'_>,
        cursor: ListCursor<'_>,
        options: &FetchOptions,
    ) -> Result<CommitListing, DomainError> {
        self.inner.list_commits(repo, cursor, options).await
    }

    async fn refine_commit(
        &self,
        repo: RepoRef<'_>,
        sha: &str,
        with_ci: bool,
        options: &FetchOptions,
    ) -> Result<CommitDetail, DomainError> {
        self.count();
        self.inner.refine_commit(repo, sha, with_ci, options).await
    }

    async fn list_metadata(
        &self,
        repo: RepoRef<'_>,
        options: &FetchOptions,
    ) -> Result<MetadataListing, DomainError> {
        self.inner.list_metadata(repo, options).await
    }

    async fn list_actions(
        &self,
        repo: RepoRef<'_>,
        options: &FetchOptions,
    ) -> Result<ActionsListing, DomainError> {
        self.inner.list_actions(repo, options).await
    }

    async fn refine_workflow_run(
        &self,
        repo: RepoRef<'_>,
        run_id: i64,
        options: &FetchOptions,
    ) -> Result<Vec<WorkflowJobRecord>, DomainError> {
        self.inner.refine_workflow_run(repo, run_id, options).await
    }

    async fn clear_cache(
        &self,
        scope: &AccessScope,
        tenant_id: Uuid,
        owner: &str,
        name: Option<&str>,
        repo_ids: &[i64],
    ) -> Result<u64, DomainError> {
        self.inner
            .clear_cache(scope, tenant_id, owner, name, repo_ids)
            .await
    }

    async fn expire_cache(
        &self,
        scope: &AccessScope,
        tenant_id: Uuid,
        fetched_before: chrono::DateTime<chrono::Utc>,
    ) -> Result<u64, DomainError> {
        self.inner
            .expire_cache(scope, tenant_id, fetched_before)
            .await
    }

    async fn cache_size(
        &self,
        scope: &AccessScope,
        tenant_id: uuid::Uuid,
        owner: &str,
        name: &str,
        repo_ids: &[i64],
    ) -> Result<u64, DomainError> {
        self.inner
            .cache_size(scope, tenant_id, owner, name, repo_ids)
            .await
    }

    async fn rate_limit(
        &self,
    ) -> Result<Vec<github_mirror::domain::ports::github::RateLimitQuota>, DomainError> {
        self.inner.rate_limit().await
    }
}

fn stamp(i: i64) -> String {
    let base = chrono::DateTime::parse_from_rfc3339("2026-01-01T00:00:00Z")
        .unwrap()
        .with_timezone(&chrono::Utc);
    (base + chrono::Duration::seconds(i)).to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// The small fixture's repository with large, generated listings in place of
/// its hand-written ones: 110,000 entities across the four biggest families.
fn large_repository() -> FetchedRepository {
    let mut repo = common::fetched_repository();
    repo.issues = (0..ISSUES)
        .map(|i| IssueRecord {
            author_login: Some(format!("user{}", i % 500)),
            author_json: None,
            assignees_json: None,
            labels_json: None,
            comments_count: None,
            locked: None,
            node_id: None,
            id: 1_000_000 + i,
            repo_id: REPO_ID,
            number: 1 + i,
            title: format!("issue {i}"),
            body: None,
            state: if i % 3 == 0 { "closed" } else { "open" }.to_owned(),
            is_pull_request: false,
            created_at: stamp(i),
            updated_at: stamp(i),
            closed_at: None,
            html_url: None,
        })
        .collect();
    repo.comments = (0..COMMENTS)
        .map(|i| CommentRecord {
            id: 2_000_000 + i,
            repo_id: REPO_ID,
            issue_number: 1 + i % ISSUES,
            author_login: Some(format!("user{}", i % 500)),
            body: Some(format!("comment {i}")),
            created_at: stamp(i),
            updated_at: stamp(i),
            html_url: None,
        })
        .collect();
    repo.pull_requests = (0..PULL_REQUESTS)
        .map(|i| PullRequestRecord {
            author_login: Some(format!("user{}", i % 500)),
            author_json: None,
            assignees_json: None,
            labels_json: None,
            comments_count: None,
            locked: None,
            requested_reviewers_json: None,
            node_id: None,
            id: 3_000_000 + i,
            repo_id: REPO_ID,
            number: ISSUES + 1 + i,
            title: format!("pr {i}"),
            body: None,
            state: if i % 2 == 0 { "closed" } else { "open" }.to_owned(),
            draft: false,
            merged: i % 2 == 0,
            head_sha: Some(format!("h{i}")),
            base_sha: Some("b1".to_owned()),
            lines_added: 1,
            lines_removed: 1,
            created_at: stamp(i),
            updated_at: stamp(i),
            closed_at: None,
            merged_at: None,
            html_url: None,
            head_ref: Some(format!("feature-{i}")),
            base_ref: Some("master".to_owned()),
        })
        .collect();
    repo.commits = (0..COMMITS)
        .map(|i| CommitRecord {
            repo_id: REPO_ID,
            sha: format!("{i:040x}"),
            message: format!("commit {i}"),
            author_login: Some(format!("user{}", i % 500)),
            committer_login: None,
            authored_at: Some(stamp(i)),
            committed_at: Some(stamp(i)),
            additions: 1,
            deletions: 0,
        })
        .collect();
    repo.review_comments.clear();
    repo.reviews.clear();
    repo.pull_request_files.clear();
    repo.review_threads.clear();
    repo.pull_request_commits.clear();
    repo.commit_files.clear();
    repo.commit_statuses.clear();
    repo.check_runs.clear();
    repo.commit_comments.clear();
    repo.issue_reactions.clear();
    repo.issue_timeline.clear();
    repo.issue_events.clear();
    repo
}

async fn counts(
    service: &common::ConcreteService,
    ctx: &SecurityContext,
) -> [(&'static str, u64); 4] {
    let window = PageWindow::first(1);
    let filter = github_mirror::domain::repo::ListingFilter::default();
    let issues = service
        .list_issues(ctx, OWNER, NAME, window, filter)
        .await
        .expect("issues must list")
        .1;
    let pulls = service
        .list_pull_requests(ctx, OWNER, NAME, window, filter)
        .await
        .expect("pull requests must list")
        .1;
    let commits = service
        .list_commits(ctx, OWNER, NAME, window, None)
        .await
        .expect("commits must list")
        .1;
    let comments = service
        .list_comments(ctx, OWNER, NAME, 1, PageWindow::first(1000))
        .await
        .expect("comments must list")
        .items
        .len() as u64;
    [
        ("issues", issues),
        ("pull_requests", pulls),
        ("commits", commits),
        ("comments_of_issue_1", comments),
    ]
}

async fn run_status(service: &common::ConcreteService, ctx: &SecurityContext) -> RepoRunStatus {
    service
        .list_repo_sync_status(ctx, &ODataQuery::default(), None)
        .await
        .expect("run statuses must list")
        .items[0]
        .status
}

#[tokio::test]
#[ignore = "110,000 entities through SQLite: minutes, not seconds; run with --ignored"]
async fn a_hundred_thousand_entities_survive_repeated_interruption_and_resume() {
    let _logs = tracing::subscriber::set_default(
        tracing_subscriber::fmt()
            .with_env_filter("warn")
            .with_test_writer()
            .finish(),
    );
    let fixture = Arc::new(large_repository());
    let ctx = common::caller_in(Uuid::new_v4());

    let clean = common::service_with_github(
        common::inmem_db().await,
        "https://api.github.com",
        Arc::new(common::FakeGithub {
            result: Some((*fixture).clone()),
        }),
    );
    let mut clean_pump = common::SyncPump::take(&clean).await;
    clean
        .enqueue_sync(&ctx, OWNER, NAME, None, ForceMode::None, None)
        .await
        .expect("the clean sync must queue");
    assert_eq!(clean_pump.drain(&clean).await, 1);
    let clean_session = &clean
        .list_sessions(&ctx, &ODataQuery::default())
        .await
        .expect("sessions must list")
        .items[0];
    assert_eq!(
        clean_session.status,
        SessionStatus::Complete,
        "the clean run must finish: error {:?}, summary {:?}",
        clean_session.error,
        clean_session.summary_json
    );
    let expected = counts(&clean, &ctx).await;
    assert_eq!(expected[0].1, ISSUES as u64);
    assert_eq!(expected[1].1, PULL_REQUESTS as u64);
    assert_eq!(expected[2].1, COMMITS as u64);
    assert_eq!(run_status(&clean, &ctx).await, RepoRunStatus::Complete);

    let db = common::inmem_db().await;
    for (round, after) in [1usize, 5_000, 10_000, 15_000].into_iter().enumerate() {
        let stopped = CancellationToken::new();
        let service = common::service_with_github(
            db.clone(),
            "https://api.github.com",
            Arc::new(StopsMidRefinement {
                inner: common::FakeGithub {
                    result: Some((*fixture).clone()),
                },
                cancel: stopped.clone(),
                after,
                refinements: AtomicUsize::new(0),
            }),
        );
        let mut pump = common::SyncPump::take(&service).await;
        if round == 0 {
            service
                .enqueue_sync(&ctx, OWNER, NAME, None, ForceMode::None, None)
                .await
                .expect("the first interrupted sync must queue");
        } else {
            let resumed = service
                .resume_incomplete_syncs(&ctx, None, ForceMode::None)
                .await
                .expect("resume must queue the repository again");
            assert_eq!(resumed.session_ids.len(), 1, "round {round}");
            assert!(resumed.refused.is_empty(), "round {round}");
        }
        assert_eq!(pump.drain_under(&service, &stopped).await, 1);

        let sessions = service
            .list_sessions(&ctx, &ODataQuery::default())
            .await
            .expect("sessions must list");
        assert_eq!(
            sessions.items[0].status,
            SessionStatus::Interrupted,
            "round {round}: a run cut off after {after} refinements must report it"
        );
        assert_eq!(
            run_status(&service, &ctx).await,
            RepoRunStatus::InProgress,
            "round {round}: the repository stays in progress for resume"
        );
    }

    let service = common::service_with_github(
        db,
        "https://api.github.com",
        Arc::new(common::FakeGithub {
            result: Some((*fixture).clone()),
        }),
    );
    let mut pump = common::SyncPump::take(&service).await;
    let resumed = service
        .resume_incomplete_syncs(&ctx, None, ForceMode::None)
        .await
        .expect("the final resume must queue");
    assert_eq!(resumed.session_ids.len(), 1);
    assert_eq!(pump.drain(&service).await, 1);

    assert_eq!(
        counts(&service, &ctx).await,
        expected,
        "four interruptions and resumes must end in the state one clean run reaches"
    );
    assert_eq!(run_status(&service, &ctx).await, RepoRunStatus::Complete);
    let sessions = service
        .list_sessions(&ctx, &ODataQuery::default())
        .await
        .expect("sessions must list");
    assert_eq!(sessions.items[0].status, SessionStatus::Complete);
    assert_eq!(
        sessions.items.len(),
        5,
        "four interrupted sessions and the one that finished"
    );
}
