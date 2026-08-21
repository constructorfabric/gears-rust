#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use github_mirror::domain::repo::RepositoryRecord;
use toolkit_odata::ODataQuery;
use uuid::Uuid;

fn repo(id: i64, name: &str) -> RepositoryRecord {
    RepositoryRecord {
        id,
        owner: "acme".to_owned(),
        name: name.to_owned(),
        full_name: format!("acme/{name}"),
        default_branch: "main".to_owned(),
        private: true,
        pushed_at: Some("2026-08-18T00:00:00Z".to_owned()),
        stars: 7,
        forks: 2,
        description: None,
    }
}

#[tokio::test]
async fn two_tenants_can_mirror_the_same_repository_without_collision() {
    let service = common::service("https://api.github.com").await;
    let tenant_a = common::caller_in(Uuid::new_v4());
    let tenant_b = common::caller_in(Uuid::new_v4());

    service
        .upsert_repository(&tenant_a, repo(500, "shared"))
        .await
        .unwrap_or_else(|e| panic!("tenant A upsert must succeed: {e}"));
    service
        .upsert_repository(&tenant_b, repo(500, "shared"))
        .await
        .unwrap_or_else(|e| panic!("tenant B upsert of the same repo id must succeed: {e}"));

    let page_a = service
        .list_repositories(&tenant_a, &ODataQuery::default())
        .await
        .unwrap_or_else(|e| panic!("tenant A list must succeed: {e}"));
    let page_b = service
        .list_repositories(&tenant_b, &ODataQuery::default())
        .await
        .unwrap_or_else(|e| panic!("tenant B list must succeed: {e}"));

    assert_eq!(page_a.items.len(), 1);
    assert_eq!(page_b.items.len(), 1);
}

#[tokio::test]
async fn queries_are_tenant_scoped() {
    let service = common::service("https://api.github.com").await;
    let tenant_a = common::caller_in(Uuid::new_v4());
    let tenant_b = common::caller_in(Uuid::new_v4());

    service
        .upsert_repository(&tenant_a, repo(1, "only-a"))
        .await
        .unwrap_or_else(|e| panic!("upsert must succeed: {e}"));

    let page_b = service
        .list_repositories(&tenant_b, &ODataQuery::default())
        .await
        .unwrap_or_else(|e| panic!("list must succeed: {e}"));

    assert!(page_b.items.is_empty());
}

#[tokio::test]
async fn upsert_is_idempotent_and_updates_fields() {
    let service = common::service("https://api.github.com").await;
    let tenant = common::caller_in(Uuid::new_v4());

    service
        .upsert_repository(&tenant, repo(9, "thing"))
        .await
        .unwrap_or_else(|e| panic!("first upsert must succeed: {e}"));

    let mut updated = repo(9, "thing");
    updated.stars = 42;
    updated.description = Some("now described".to_owned());
    service
        .upsert_repository(&tenant, updated)
        .await
        .unwrap_or_else(|e| panic!("second upsert must succeed: {e}"));

    let page = service
        .list_repositories(&tenant, &ODataQuery::default())
        .await
        .unwrap_or_else(|e| panic!("list must succeed: {e}"));

    assert_eq!(page.items.len(), 1);
    assert_eq!(page.items[0].description.as_deref(), Some("now described"));
}

#[tokio::test]
async fn list_respects_query_limit() {
    let service = common::service("https://api.github.com").await;
    let tenant = common::caller_in(Uuid::new_v4());

    for i in 0..5 {
        service
            .upsert_repository(&tenant, repo(i, &format!("repo-{i}")))
            .await
            .unwrap_or_else(|e| panic!("upsert must succeed: {e}"));
    }

    let page = service
        .list_repositories(&tenant, &ODataQuery::default().with_limit(2))
        .await
        .unwrap_or_else(|e| panic!("list must succeed: {e}"));

    assert_eq!(page.items.len(), 2);
    assert_eq!(page.page_info.limit, 2);
}
