#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::sync::Arc;

use github_mirror::GithubMirrorGear;
use github_mirror::domain::local_client::LocalClient;
use github_mirror::domain::ports::github::FetchedRepository;
use github_mirror::domain::repo::RepositoryRecord;
use github_mirror_sdk::GithubMirrorClientV1;
use toolkit::{ClientHub, Gear};
use toolkit_odata::ODataQuery;

fn record(id: i64, owner: &str, name: &str) -> RepositoryRecord {
    RepositoryRecord {
        id,
        owner: owner.to_owned(),
        name: name.to_owned(),
        full_name: format!("{owner}/{name}"),
        default_branch: "main".to_owned(),
        private: false,
        pushed_at: None,
        stars: 0,
        forks: 0,
        description: Some("mirrored".to_owned()),
    }
}

#[tokio::test]
async fn consumer_resolves_client_from_hub_and_queries_status() {
    let hub = Arc::new(ClientHub::new());
    let gear = GithubMirrorGear::default();
    gear.init(&common::gear_ctx(hub.clone(), None).await)
        .await
        .expect("init must succeed");

    let client = hub
        .get::<dyn GithubMirrorClientV1>()
        .unwrap_or_else(|e| panic!("consumer must resolve the client from ClientHub: {e}"));

    let status = client
        .status(&common::caller())
        .await
        .unwrap_or_else(|e| panic!("status query must succeed: {e}"));

    assert_eq!(status.gear, "github-mirror");
    assert_eq!(status.api_base_url, "https://api.github.com");
    assert_eq!(status.version, env!("CARGO_PKG_VERSION"));
}

#[tokio::test]
async fn list_repositories_via_hub_returns_seeded_rows() {
    let db = common::inmem_db().await;
    let service = common::service_over(db.clone(), "https://api.github.com");

    let tenant = uuid::Uuid::new_v4();
    let ctx = common::caller_in(tenant);
    service
        .upsert_repository(&ctx, record(101, "constructorfabric", "gears-rust"))
        .await
        .unwrap_or_else(|e| panic!("seed upsert must succeed: {e}"));
    service
        .upsert_repository(&ctx, record(102, "constructorfabric", "github-repotap"))
        .await
        .unwrap_or_else(|e| panic!("seed upsert must succeed: {e}"));

    let hub = Arc::new(ClientHub::new());
    let gear = GithubMirrorGear::default();
    let gear_ctx = common::gear_ctx(hub.clone(), None)
        .await
        .with_db(toolkit_db::DBProvider::new(db));
    gear.init(&gear_ctx).await.expect("init must succeed");

    let client = hub
        .get::<dyn GithubMirrorClientV1>()
        .unwrap_or_else(|e| panic!("consumer must resolve the client from ClientHub: {e}"));

    let page = client
        .list_repositories(&ctx, ODataQuery::default())
        .await
        .unwrap_or_else(|e| panic!("list must succeed: {e}"));

    assert_eq!(page.items.len(), 2);
    assert_eq!(page.items[0].full_name, "constructorfabric/gears-rust");
    assert_eq!(page.items[1].full_name, "constructorfabric/github-repotap");
    assert_eq!(page.items[0].id, 101);
}

#[tokio::test]
async fn sync_repository_via_sdk_trait_fills_the_mirror() {
    let db = common::inmem_db().await;
    let fetched = FetchedRepository {
        repository: record(500, "constructorfabric", "gears-rust"),
        issues: vec![],
        pull_requests: vec![],
        commits: vec![],
        comments: vec![],
        review_comments: vec![],
        reviews: vec![],
        labels: vec![],
        milestones: vec![],
        releases: vec![],
        branches: vec![],
    };
    let service = common::service_with_github(
        db,
        "https://api.github.com",
        Arc::new(common::FakeGithub {
            result: Some(fetched),
        }),
    );
    let client: Arc<dyn GithubMirrorClientV1> = Arc::new(LocalClient::new(service));

    let ctx = common::caller();
    let summary = client
        .sync_repository(&ctx, "constructorfabric", "gears-rust")
        .await
        .unwrap_or_else(|e| panic!("sync must succeed: {e}"));

    assert_eq!(summary.repository, "constructorfabric/gears-rust");
    assert_eq!(summary.issues_synced, 0);

    let page = client
        .list_repositories(&ctx, ODataQuery::default())
        .await
        .unwrap_or_else(|e| panic!("list must succeed: {e}"));
    assert_eq!(page.items.len(), 1);
}
