use crate::domain::model::{Endpoint, Scheme, Server};

use super::*;

fn make_upstream(tenant_id: Uuid, alias: &str) -> Upstream {
    Upstream {
        id: Uuid::new_v4(),
        tenant_id,
        alias: alias.into(),
        server: Server {
            endpoints: vec![Endpoint {
                scheme: Scheme::Https,
                host: "api.openai.com".into(),
                port: 443,
            }],
        },
        protocol: "gts.x.core.oagw.protocol.v1~x.core.oagw.http.v1".into(),
        enabled: true,
        auth: None,
        headers: None,
        plugins: None,
        rate_limit: None,
        cors: None,
        tags: vec![],
    }
}

#[tokio::test]
async fn create_and_get_round_trip() {
    let repo = InMemoryUpstreamRepo::new();
    let tenant = Uuid::new_v4();
    let u = make_upstream(tenant, "openai");
    let id = u.id;

    let created = repo.create(u).await.unwrap();
    assert_eq!(created.alias, "openai");

    let fetched = repo.get_by_id(tenant, id).await.unwrap();
    assert_eq!(fetched.id, id);
    assert_eq!(fetched.alias, "openai");
}

#[tokio::test]
async fn get_by_alias() {
    let repo = InMemoryUpstreamRepo::new();
    let tenant = Uuid::new_v4();
    let u = make_upstream(tenant, "openai");
    repo.create(u.clone()).await.unwrap();

    let fetched = repo.get_by_alias(tenant, "openai").await.unwrap();
    assert_eq!(fetched.id, u.id);
}

#[tokio::test]
async fn alias_uniqueness_same_tenant() {
    let repo = InMemoryUpstreamRepo::new();
    let tenant = Uuid::new_v4();

    repo.create(make_upstream(tenant, "openai")).await.unwrap();
    let err = repo.create(make_upstream(tenant, "openai")).await;
    assert!(matches!(err, Err(RepositoryError::Conflict(_))));
}

#[tokio::test]
async fn alias_allowed_different_tenant() {
    let repo = InMemoryUpstreamRepo::new();
    let t1 = Uuid::new_v4();
    let t2 = Uuid::new_v4();

    repo.create(make_upstream(t1, "openai")).await.unwrap();
    repo.create(make_upstream(t2, "openai")).await.unwrap();
}

#[tokio::test]
async fn update_preserves_id() {
    let repo = InMemoryUpstreamRepo::new();
    let tenant = Uuid::new_v4();
    let mut u = make_upstream(tenant, "openai");
    let id = u.id;
    repo.create(u.clone()).await.unwrap();

    u.alias = "openai-v2".into();
    let updated = repo.update(u).await.unwrap();
    assert_eq!(updated.id, id);
    assert_eq!(updated.alias, "openai-v2");

    // Old alias should not resolve.
    assert!(repo.get_by_alias(tenant, "openai").await.is_err());
    // New alias should resolve.
    assert!(repo.get_by_alias(tenant, "openai-v2").await.is_ok());
}

#[tokio::test]
async fn update_to_duplicate_alias_returns_conflict_and_preserves_original() {
    let repo = InMemoryUpstreamRepo::new();
    let tenant = Uuid::new_v4();

    let u1 = make_upstream(tenant, "openai");
    let mut u2 = make_upstream(tenant, "anthropic");
    repo.create(u1.clone()).await.unwrap();
    repo.create(u2.clone()).await.unwrap();

    // Try to rename u2's alias to "openai" (already taken by u1).
    u2.alias = "openai".into();
    let result = repo.update(u2).await;
    assert!(matches!(result, Err(RepositoryError::Conflict(_))));

    // Original alias mappings are intact.
    let fetched_u1 = repo.get_by_alias(tenant, "openai").await.unwrap();
    assert_eq!(fetched_u1.id, u1.id);

    let fetched_u2 = repo.get_by_alias(tenant, "anthropic").await.unwrap();
    assert_eq!(fetched_u2.alias, "anthropic");
}

#[tokio::test]
async fn delete_removes_alias_index() {
    let repo = InMemoryUpstreamRepo::new();
    let tenant = Uuid::new_v4();
    let u = make_upstream(tenant, "openai");
    let id = u.id;
    repo.create(u).await.unwrap();

    repo.delete(tenant, id).await.unwrap();
    assert!(repo.get_by_id(tenant, id).await.is_err());
    assert!(repo.get_by_alias(tenant, "openai").await.is_err());
}

#[tokio::test]
async fn list_with_pagination() {
    let repo = InMemoryUpstreamRepo::new();
    let tenant = Uuid::new_v4();

    for i in 0..5 {
        repo.create(make_upstream(tenant, &format!("svc-{i}")))
            .await
            .unwrap();
    }

    let all = repo
        .list(tenant, &ListQuery { top: 50, skip: 0 })
        .await
        .unwrap();
    assert_eq!(all.len(), 5);

    let page = repo
        .list(tenant, &ListQuery { top: 2, skip: 1 })
        .await
        .unwrap();
    assert_eq!(page.len(), 2);
}

#[tokio::test]
async fn cross_tenant_delete_returns_not_found_and_preserves_upstream() {
    let repo = InMemoryUpstreamRepo::new();
    let owner = Uuid::new_v4();
    let attacker = Uuid::new_v4();

    let u = make_upstream(owner, "openai");
    let id = u.id;
    repo.create(u).await.unwrap();

    // Different tenant cannot delete it.
    let result = repo.delete(attacker, id).await;
    assert!(matches!(result, Err(RepositoryError::NotFound { .. })));

    // Upstream remains accessible to the owner.
    let fetched = repo.get_by_id(owner, id).await.unwrap();
    assert_eq!(fetched.id, id);
    assert_eq!(fetched.alias, "openai");

    // Alias index is also intact.
    let by_alias = repo.get_by_alias(owner, "openai").await.unwrap();
    assert_eq!(by_alias.id, id);
}

#[tokio::test]
async fn list_pagination_is_deterministic() {
    let repo = InMemoryUpstreamRepo::new();
    let tenant = Uuid::new_v4();

    for i in 0..5 {
        repo.create(make_upstream(tenant, &format!("svc-{i}")))
            .await
            .unwrap();
    }

    let query = ListQuery { top: 3, skip: 0 };
    let first = repo.list(tenant, &query).await.unwrap();
    let second = repo.list(tenant, &query).await.unwrap();

    // Same results both times.
    assert_eq!(first.len(), 3);
    let first_ids: Vec<Uuid> = first.iter().map(|u| u.id).collect();
    let second_ids: Vec<Uuid> = second.iter().map(|u| u.id).collect();
    assert_eq!(first_ids, second_ids);

    // IDs are sorted.
    for w in first_ids.windows(2) {
        assert!(w[0] < w[1], "IDs must be in ascending order");
    }
}

#[tokio::test]
async fn cross_tenant_isolation() {
    let repo = InMemoryUpstreamRepo::new();
    let t1 = Uuid::new_v4();
    let t2 = Uuid::new_v4();

    let u = make_upstream(t1, "openai");
    let id = u.id;
    repo.create(u).await.unwrap();

    // Different tenant cannot see it.
    assert!(repo.get_by_id(t2, id).await.is_err());
}
