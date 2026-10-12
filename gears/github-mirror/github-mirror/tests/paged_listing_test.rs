#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use github_mirror::domain::error::DomainError;
use github_mirror::domain::ports::github::{
    ActionsListing, CommitDetail, CommitListing, FetchOptions, FetchedRepository, GithubPort,
    IssueDetail, IssueDetailWants, IssueListing, ListCursor, ListingCompleteness, MetadataListing,
    PullDetail, PullListing, RepoRef,
};
use github_mirror::domain::repo::{
    IssueRecord, ListingFilter, PageWindow, RepoRecord, SyncWatermarkRepository, WorkflowJobRecord,
};
use github_mirror::infra::storage::sea_orm_repo::SeaOrmSyncWatermarkRepository;
use toolkit_db::{DBProvider, DbError};
use toolkit_security::AccessScope;
use uuid::Uuid;

const OWNER: &str = "rust-lang";
const NAME: &str = "rust";
const REPO_ID: i64 = 42;
const PAGE_ONE_ETAG: &str = "W/\"issues-page-one\"";
const PAGE_TWO: &str = "https://api.github.com/repositories/42/issues?page=2";

fn four_open_issues() -> FetchedRepository {
    let mut fixture = common::fetched_repository();
    let template = fixture.issues[0].clone();
    fixture.issues = (0..4i64)
        .map(|offset| IssueRecord {
            id: 100 + offset,
            number: 11 + offset,
            updated_at: format!("2026-08-2{offset}T00:00:00Z"),
            ..template.clone()
        })
        .collect();
    fixture
}

/// The fixture fake, but the issues listing comes in two pages: the first
/// half with a `next` cursor, then the second half plus a repeat of a page-one
/// issue, as GitHub does when the list shifts under the walk.
struct TwoPageIssues {
    inner: common::FakeGithub,
    cursors_seen: Mutex<Vec<Option<String>>>,
    refined: Mutex<Vec<i64>>,
}

#[async_trait]
impl GithubPort for TwoPageIssues {
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
        self.cursors_seen
            .lock()
            .unwrap()
            .push(cursor.continue_from.map(ToOwned::to_owned));
        let mut full = self.inner.list_issues(repo, cursor, options).await?;
        let second = full.issues.split_off(full.issues.len().div_euclid(2));
        let first = std::mem::take(&mut full.issues);
        match cursor.continue_from {
            None => Ok(IssueListing {
                issues: first,
                comments: Vec::new(),
                issue_events: Vec::new(),
                contributors: Vec::new(),
                complete: ListingCompleteness::none(),
                page1_etag: Some(PAGE_ONE_ETAG.to_owned()),
                unchanged: false,
                swept_to_end: false,
                next: Some(PAGE_TWO.to_owned()),
            }),
            Some(PAGE_TWO) => {
                let mut issues = second;
                issues.push(first[0].clone());
                Ok(IssueListing {
                    issues,
                    page1_etag: Some("W/\"not-page-one\"".to_owned()),
                    unchanged: false,
                    swept_to_end: true,
                    next: None,
                    ..full
                })
            }
            Some(other) => Err(DomainError::internal(format!(
                "the walk asked for a page nobody handed out: {other}"
            ))),
        }
    }

    async fn refine_issue(
        &self,
        repo: RepoRef<'_>,
        number: i64,
        wants: IssueDetailWants,
        options: &FetchOptions,
    ) -> Result<IssueDetail, DomainError> {
        self.refined.lock().unwrap().push(number);
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
        owner: &str,
        name: Option<&str>,
        repo_ids: &[i64],
    ) -> Result<u64, DomainError> {
        self.inner.clear_cache(scope, owner, name, repo_ids).await
    }
}

#[tokio::test]
async fn a_listing_walked_in_two_pages_keeps_one_state_across_them() {
    let tenant = Uuid::new_v4();
    let ctx = common::caller_in(tenant);
    let db = common::inmem_db().await;
    let github = Arc::new(TwoPageIssues {
        inner: common::FakeGithub {
            result: Some(four_open_issues()),
        },
        cursors_seen: Mutex::new(Vec::new()),
        refined: Mutex::new(Vec::new()),
    });
    let service = common::service_with_github(
        db.clone(),
        "https://api.github.com",
        Arc::clone(&github) as Arc<dyn GithubPort>,
    );
    let mut pump = common::SyncPump::take(&service).await;

    service
        .enqueue_sync(&ctx, OWNER, NAME, None, false, None)
        .await
        .expect("the sync must queue");
    assert_eq!(pump.drain(&service).await, 1);

    assert_eq!(
        *github.cursors_seen.lock().unwrap(),
        [None, Some(PAGE_TWO.to_owned())],
        "page two is fetched from the cursor page one handed back, then the walk stops"
    );

    let mut stored: Vec<i64> = service
        .list_issues(
            &ctx,
            OWNER,
            NAME,
            PageWindow::first(50),
            ListingFilter::default(),
        )
        .await
        .expect("issues must list")
        .0
        .items
        .iter()
        .map(|issue| issue.number)
        .collect();
    stored.sort_unstable();
    assert_eq!(stored, [11, 12, 13, 14]);

    let mut refined = github.refined.lock().unwrap().clone();
    refined.sort_unstable();
    assert_eq!(
        refined,
        [11, 12, 13, 14],
        "issue 11 came back on page two but is refined once: the swept set spans pages"
    );

    let watermark = SeaOrmSyncWatermarkRepository::new(Arc::new(DBProvider::<DbError>::new(db)))
        .find(&AccessScope::for_tenant(tenant), REPO_ID, "issues")
        .await
        .expect("the watermark must read")
        .expect("a sweep that reached the end leaves a watermark");
    assert_eq!(
        watermark.last_seen_updated_at.as_deref(),
        Some("2026-08-23T00:00:00Z"),
        "the high-water mark takes in the newest issue of every page"
    );
    assert_eq!(
        watermark.page1_etag.as_deref(),
        Some(PAGE_ONE_ETAG),
        "the validator kept is page one's, not the last page's"
    );
}
