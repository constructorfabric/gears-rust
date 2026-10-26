#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::sync::Arc;

use github_mirror::domain::error::DomainError;
use github_mirror::domain::ports::github::ForceMode;
use github_mirror::domain::repo::{ListingFilter, PageWindow, RepoRunStatus};
use toolkit_odata::ODataQuery;
use uuid::Uuid;

const OWNER: &str = "rust-lang";
const NAME: &str = "rust";

async fn synced_service() -> (Arc<common::ConcreteService>, common::SyncPump) {
    let service = common::service_with_github(
        common::inmem_db().await,
        "https://api.github.com",
        Arc::new(common::FakeGithub {
            result: Some(common::fetched_repository()),
        }),
    );
    let pump = common::SyncPump::take(&service).await;
    (service, pump)
}

#[tokio::test]
async fn deleting_a_repository_removes_every_trace_and_the_next_sync_starts_over() {
    let ctx = common::caller_in(Uuid::new_v4());
    let (service, mut pump) = synced_service().await;
    service
        .enqueue_sync(&ctx, OWNER, NAME, None, ForceMode::None, None)
        .await
        .expect("the sync must queue");
    assert_eq!(pump.drain(&service).await, 1);
    assert!(service.get_repo(&ctx, OWNER, NAME).await.is_ok());

    let removed = service
        .delete_repository(&ctx, OWNER, NAME)
        .await
        .expect("the delete must succeed");
    assert!(removed > 0, "rows must have been removed, got {removed}");

    assert!(matches!(
        service.get_repo(&ctx, OWNER, NAME).await,
        Err(DomainError::NotFound)
    ));
    assert!(matches!(
        service
            .list_issues(
                &ctx,
                OWNER,
                NAME,
                PageWindow::first(10),
                ListingFilter::default()
            )
            .await,
        Err(DomainError::NotFound)
    ));
    assert!(
        service
            .list_sessions(&ctx, &ODataQuery::default())
            .await
            .unwrap()
            .items
            .is_empty(),
        "the sessions of the deleted repository must go too"
    );
    assert!(
        service
            .list_repo_sync_status(&ctx, &ODataQuery::default(), None)
            .await
            .unwrap()
            .items
            .is_empty()
    );

    service
        .enqueue_sync(&ctx, OWNER, NAME, None, ForceMode::None, None)
        .await
        .expect("a fresh sync must queue");
    assert_eq!(pump.drain(&service).await, 1);
    let statuses = service
        .list_repo_sync_status(&ctx, &ODataQuery::default(), None)
        .await
        .unwrap();
    assert_eq!(statuses.items[0].status, RepoRunStatus::Complete);
    assert!(service.get_repo(&ctx, OWNER, NAME).await.is_ok());
}

#[tokio::test]
async fn deleting_leaves_the_other_tenants_copy_alone() {
    let one = common::caller_in(Uuid::new_v4());
    let two = common::caller_in(Uuid::new_v4());
    let (service, mut pump) = synced_service().await;
    for ctx in [&one, &two] {
        service
            .enqueue_sync(ctx, OWNER, NAME, None, ForceMode::None, None)
            .await
            .unwrap();
    }
    assert_eq!(pump.drain(&service).await, 2);

    service.delete_repository(&one, OWNER, NAME).await.unwrap();

    assert!(matches!(
        service.get_repo(&one, OWNER, NAME).await,
        Err(DomainError::NotFound)
    ));
    assert!(service.get_repo(&two, OWNER, NAME).await.is_ok());
    assert_eq!(
        service
            .list_sessions(&two, &ODataQuery::default())
            .await
            .unwrap()
            .items
            .len(),
        1
    );
}

#[tokio::test]
async fn deleting_an_unknown_repository_is_not_found() {
    let ctx = common::caller();
    let (service, _pump) = synced_service().await;
    assert!(matches!(
        service.delete_repository(&ctx, OWNER, NAME).await,
        Err(DomainError::NotFound)
    ));
}
