#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use github_mirror::domain::error::DomainError;
use github_mirror::domain::ports::github::{
    ActionsListing, CommitDetail, CommitListing, FetchOptions, GithubPort, IssueDetail,
    IssueDetailWants, IssueListing, ListCursor, MetadataListing, PullDetail, PullListing, RepoRef,
};
use github_mirror::domain::repo::{PullRequestCommitRecord, RepoRecord, WorkflowJobRecord};
use github_mirror::domain::sync::MAX_REPAIR;
use github_mirror_sdk::{CountDrift, SyncSummary};
use toolkit_odata::ODataQuery;
use toolkit_security::AccessScope;
use uuid::Uuid;

const OWNER: &str = "rust-lang";
const NAME: &str = "rust";
const PULL: i64 = 12;

/// The fixture fake, but pull request 12 declares `declared` commits while
/// each refinement pass hands back only `stored_on_pass(pass)` of them, the
/// way a GitHub listing comes up short.
struct ShortPullWalk {
    inner: common::FakeGithub,
    declared: i64,
    stored_on_pass: fn(usize) -> usize,
    passes: Mutex<Vec<i64>>,
}

impl ShortPullWalk {
    fn new(declared: i64, stored_on_pass: fn(usize) -> usize) -> Arc<Self> {
        Arc::new(Self {
            inner: common::FakeGithub {
                result: Some(common::fetched_repository()),
            },
            declared,
            stored_on_pass,
            passes: Mutex::new(Vec::new()),
        })
    }

    fn passes_for(&self, number: i64) -> usize {
        self.passes
            .lock()
            .unwrap()
            .iter()
            .filter(|seen| **seen == number)
            .count()
    }
}

#[async_trait]
impl GithubPort for ShortPullWalk {
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
        let pass = self.passes_for(number);
        self.passes.lock().unwrap().push(number);
        let mut detail = self
            .inner
            .refine_pull_request(repo, number, options)
            .await?;
        if number == PULL {
            let template = detail
                .commits
                .first()
                .cloned()
                .expect("the fixture gives pull 12 one commit to copy");
            detail.declared.commits = Some(self.declared);
            detail.commits = (0..(self.stored_on_pass)(pass))
                .map(|index| PullRequestCommitRecord {
                    sha: format!("pc{index}"),
                    ..template.clone()
                })
                .collect();
        }
        Ok(detail)
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

async fn sync_with(github: Arc<ShortPullWalk>) -> SyncSummary {
    let ctx = common::caller_in(Uuid::new_v4());
    let service = common::service_with_github(
        common::inmem_db().await,
        "https://api.github.com",
        Arc::clone(&github) as Arc<dyn GithubPort>,
    );
    let mut pump = common::SyncPump::take(&service).await;
    service
        .enqueue_sync(&ctx, OWNER, NAME, None, false, None)
        .await
        .expect("the sync must queue");
    assert_eq!(pump.drain(&service).await, 1);

    let sessions = service
        .list_sessions(&ctx, &ODataQuery::default())
        .await
        .expect("sessions must list");
    let session = &sessions.items[0];
    serde_json::from_str(
        session
            .summary_json
            .as_deref()
            .unwrap_or_else(|| panic!("the run must complete with a summary: {session:?}")),
    )
    .expect("the summary must parse")
}

#[tokio::test]
async fn a_walk_that_keeps_coming_up_short_is_repaired_up_to_the_bound_then_accepted() {
    let github = ShortPullWalk::new(5, |pass| pass + 1);

    let summary = sync_with(Arc::clone(&github)).await;

    assert_eq!(
        github.passes_for(PULL),
        1 + usize::try_from(MAX_REPAIR).unwrap(),
        "the first walk plus one repair per allowed attempt"
    );
    assert_eq!(
        summary.accepted_drift,
        vec![CountDrift {
            entity_type: "pull_request_commits".to_owned(),
            pull_number: PULL,
            expected: 5,
            stored: 4,
            passes: MAX_REPAIR,
        }],
        "the gap GitHub never closed is reported, with the last pass's count"
    );
}

#[tokio::test]
async fn a_gap_that_closes_on_the_repair_pass_leaves_no_drift() {
    let github = ShortPullWalk::new(2, |pass| pass + 1);

    let summary = sync_with(Arc::clone(&github)).await;

    assert_eq!(
        github.passes_for(PULL),
        2,
        "one short walk, one repair, done"
    );
    assert!(
        summary.accepted_drift.is_empty(),
        "{:?}",
        summary.accepted_drift
    );
}

#[tokio::test]
async fn a_gap_that_does_not_move_is_accepted_after_one_repair() {
    let github = ShortPullWalk::new(3, |_pass| 1);

    let summary = sync_with(Arc::clone(&github)).await;

    assert_eq!(
        github.passes_for(PULL),
        2,
        "a repair that changes nothing is not tried a third time"
    );
    assert_eq!(
        summary.accepted_drift,
        vec![CountDrift {
            entity_type: "pull_request_commits".to_owned(),
            pull_number: PULL,
            expected: 3,
            stored: 1,
            passes: 1,
        }]
    );
}
