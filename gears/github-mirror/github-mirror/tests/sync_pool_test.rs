#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use async_trait::async_trait;

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Method, Request, StatusCode};
use github_mirror::api::rest::routes::{ConcreteService, register_routes};
use github_mirror::domain::error::DomainError;
use github_mirror::domain::ports::github::{
    ActionsListing, CommitDetail, CommitListing, FetchOptions, GithubPort, IssueDetail,
    IssueDetailWants, IssueListing, ListCursor, MetadataListing, PullDetail, PullListing, RepoRef,
};
use github_mirror::domain::repo::{RepoRecord, WorkflowJobRecord};
use github_mirror::domain::sync::SyncPoolRunner;
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;
use toolkit::api::OpenApiRegistryImpl;
use toolkit_security::{AccessScope, SecurityContext};
use tower::ServiceExt;
use uuid::Uuid;

fn router_for(service: Arc<ConcreteService>, ctx: SecurityContext) -> Router {
    let openapi = OpenApiRegistryImpl::new();
    register_routes(Router::new(), &openapi, service).layer(axum::Extension(ctx))
}

async fn send(router: Router, method: Method, uri: &str) -> axum::http::Response<Body> {
    let request = Request::builder()
        .method(method)
        .uri(uri)
        .body(Body::empty())
        .unwrap();
    router.oneshot(request).await.unwrap()
}

async fn body_json(response: axum::http::Response<Body>) -> serde_json::Value {
    let bytes = to_bytes(response.into_body(), 1_000_000).await.unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

#[tokio::test]
async fn the_pool_runs_every_queued_sync_and_stops_when_cancelled() {
    let service = common::service_with_github(
        common::inmem_db().await,
        "https://api.github.com",
        Arc::new(common::FakeGithub {
            result: Some(common::fetched_repository()),
        }),
    );
    let jobs = service
        .take_sync_receiver()
        .await
        .expect("the job receiver must still be available");
    let cancel = CancellationToken::new();
    let pool =
        tokio::spawn(SyncPoolRunner::new(Arc::clone(&service), jobs, 1, cancel.clone()).run());

    let mut sessions = Vec::new();
    for _ in 0..3 {
        let router = router_for(Arc::clone(&service), common::caller_in(Uuid::new_v4()));
        let response = send(
            router.clone(),
            Method::POST,
            "/github-mirror/v1/repos/rust-lang/rust/sync",
        )
        .await;
        assert_eq!(response.status(), StatusCode::ACCEPTED);
        let id = body_json(response).await["session_id"]
            .as_str()
            .expect("session_id")
            .to_owned();
        sessions.push((router, id));
    }

    for (router, id) in &sessions {
        let mut status = serde_json::Value::Null;
        for _ in 0..200 {
            let uri = format!("/github-mirror/v1/sessions/{id}");
            status =
                body_json(send(router.clone(), Method::GET, &uri).await).await["status"].clone();
            if status == "complete" {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert_eq!(status, "complete", "session {id} must be run by the pool");
    }

    cancel.cancel();
    tokio::time::timeout(Duration::from_secs(5), pool)
        .await
        .expect("the pool must stop once cancelled")
        .expect("the pool task must not panic");
}

struct GatedGithub {
    inner: common::FakeGithub,
    gate: Arc<Semaphore>,
    running: AtomicUsize,
    most_at_once: AtomicUsize,
}

impl GatedGithub {
    fn new(gate: Arc<Semaphore>) -> Self {
        Self {
            inner: common::FakeGithub {
                result: Some(common::fetched_repository()),
            },
            gate,
            running: AtomicUsize::new(0),
            most_at_once: AtomicUsize::new(0),
        }
    }
}

#[async_trait]
impl GithubPort for GatedGithub {
    async fn fetch_repository_metadata(
        &self,
        owner: &str,
        name: &str,
        options: &FetchOptions,
    ) -> Result<RepoRecord, DomainError> {
        let now = self.running.fetch_add(1, Ordering::SeqCst) + 1;
        self.most_at_once.fetch_max(now, Ordering::SeqCst);
        let permit = self.gate.acquire().await;
        self.running.fetch_sub(1, Ordering::SeqCst);
        drop(permit);
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

async fn queue_sync(service: &Arc<ConcreteService>) -> (Router, String) {
    let router = router_for(Arc::clone(service), common::caller_in(Uuid::new_v4()));
    let response = send(
        router.clone(),
        Method::POST,
        "/github-mirror/v1/repos/rust-lang/rust/sync",
    )
    .await;
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    let id = body_json(response).await["session_id"]
        .as_str()
        .expect("session_id")
        .to_owned();
    (router, id)
}

async fn session_status(router: &Router, id: &str) -> serde_json::Value {
    let uri = format!("/github-mirror/v1/sessions/{id}");
    body_json(send(router.clone(), Method::GET, &uri).await).await["status"].clone()
}

async fn wait_for_running(github: &GatedGithub, wanted: usize) {
    for _ in 0..200 {
        if github.running.load(Ordering::SeqCst) >= wanted {
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("fewer than {wanted} syncs ever reached GitHub");
}

#[tokio::test]
async fn the_pool_never_runs_more_syncs_at_once_than_it_is_allowed() {
    let gate = Arc::new(Semaphore::new(0));
    let github = Arc::new(GatedGithub::new(Arc::clone(&gate)));
    let service = common::service_with_github(
        common::inmem_db().await,
        "https://api.github.com",
        Arc::clone(&github) as Arc<dyn GithubPort>,
    );
    let jobs = service
        .take_sync_receiver()
        .await
        .expect("the job receiver must still be available");
    let cancel = CancellationToken::new();
    let pool =
        tokio::spawn(SyncPoolRunner::new(Arc::clone(&service), jobs, 2, cancel.clone()).run());

    let mut sessions = Vec::new();
    for _ in 0..3 {
        sessions.push(queue_sync(&service).await);
    }

    wait_for_running(&github, 2).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(
        github.running.load(Ordering::SeqCst),
        2,
        "a third sync started while two were running on a pool of two"
    );

    gate.add_permits(3);
    for (router, id) in &sessions {
        let mut status = serde_json::Value::Null;
        for _ in 0..200 {
            status = session_status(router, id).await;
            if status == "complete" {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert_eq!(status, "complete", "session {id} must be run by the pool");
    }
    assert_eq!(github.most_at_once.load(Ordering::SeqCst), 2);

    cancel.cancel();
    tokio::time::timeout(Duration::from_secs(5), pool)
        .await
        .expect("the pool must stop once cancelled")
        .expect("the pool task must not panic");
}

#[tokio::test]
async fn a_cancelled_pool_waits_for_the_running_sync_and_records_it() {
    let gate = Arc::new(Semaphore::new(0));
    let github = Arc::new(GatedGithub::new(Arc::clone(&gate)));
    let service = common::service_with_github(
        common::inmem_db().await,
        "https://api.github.com",
        Arc::clone(&github) as Arc<dyn GithubPort>,
    );
    let jobs = service
        .take_sync_receiver()
        .await
        .expect("the job receiver must still be available");
    let cancel = CancellationToken::new();
    let pool =
        tokio::spawn(SyncPoolRunner::new(Arc::clone(&service), jobs, 1, cancel.clone()).run());

    let (router, id) = queue_sync(&service).await;
    wait_for_running(&github, 1).await;

    cancel.cancel();
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(
        !pool.is_finished(),
        "the pool must wait for the sync it is running instead of dropping it"
    );

    gate.add_permits(1);
    tokio::time::timeout(Duration::from_secs(5), pool)
        .await
        .expect("the pool must stop once its running sync ends")
        .expect("the pool task must not panic");

    let status = session_status(&router, &id).await;
    assert!(
        status == "interrupted" || status == "complete",
        "the stopped sync must end with its outcome recorded, not left {status}"
    );
}
