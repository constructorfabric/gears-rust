use crate::domain::model::{HttpMatch, MatchRules, PathSuffixMode};

use super::*;

fn make_route(
    tenant_id: Uuid,
    upstream_id: Uuid,
    methods: Vec<HttpMethod>,
    path: &str,
    priority: i32,
) -> Route {
    Route {
        id: Uuid::new_v4(),
        tenant_id,
        upstream_id,
        match_rules: MatchRules {
            http: Some(HttpMatch {
                methods,
                path: path.into(),
                query_allowlist: vec![],
                path_suffix_mode: PathSuffixMode::Append,
            }),
            grpc: None,
        },
        plugins: None,
        rate_limit: None,
        cors: None,
        tags: vec![],
        priority,
        enabled: true,
    }
}

#[tokio::test]
async fn find_matching_longest_prefix_wins() {
    let repo = InMemoryRouteRepo::new();
    let tenant = Uuid::new_v4();
    let upstream = Uuid::new_v4();

    let short = make_route(tenant, upstream, vec![HttpMethod::Post], "/v1", 0);
    let long = make_route(
        tenant,
        upstream,
        vec![HttpMethod::Post],
        "/v1/chat/completions",
        0,
    );
    repo.create(short).await.unwrap();
    repo.create(long.clone()).await.unwrap();

    let matched = repo
        .find_matching(tenant, upstream, "POST", "/v1/chat/completions")
        .await
        .unwrap();
    assert_eq!(matched.id, long.id);
}

#[tokio::test]
async fn find_matching_priority_tiebreak() {
    let repo = InMemoryRouteRepo::new();
    let tenant = Uuid::new_v4();
    let upstream = Uuid::new_v4();

    let low = make_route(tenant, upstream, vec![HttpMethod::Post], "/v1/chat", 0);
    let high = make_route(tenant, upstream, vec![HttpMethod::Post], "/v1/chat", 10);
    repo.create(low).await.unwrap();
    repo.create(high.clone()).await.unwrap();

    let matched = repo
        .find_matching(tenant, upstream, "POST", "/v1/chat/completions")
        .await
        .unwrap();
    assert_eq!(matched.id, high.id);
}

#[tokio::test]
async fn find_matching_method_mismatch_excluded() {
    let repo = InMemoryRouteRepo::new();
    let tenant = Uuid::new_v4();
    let upstream = Uuid::new_v4();

    let post_only = make_route(
        tenant,
        upstream,
        vec![HttpMethod::Post],
        "/v1/chat/completions",
        0,
    );
    repo.create(post_only).await.unwrap();

    let result = repo
        .find_matching(tenant, upstream, "GET", "/v1/chat/completions")
        .await;
    assert!(matches!(result, Err(RepositoryError::NotFound { .. })));
}

#[tokio::test]
async fn find_matching_disabled_excluded() {
    let repo = InMemoryRouteRepo::new();
    let tenant = Uuid::new_v4();
    let upstream = Uuid::new_v4();

    let mut route = make_route(tenant, upstream, vec![HttpMethod::Post], "/v1/chat", 0);
    route.enabled = false;
    repo.create(route).await.unwrap();

    let result = repo
        .find_matching(tenant, upstream, "POST", "/v1/chat/completions")
        .await;
    assert!(matches!(result, Err(RepositoryError::NotFound { .. })));
}

#[tokio::test]
async fn find_matching_unknown_method_returns_not_found() {
    let repo = InMemoryRouteRepo::new();
    let tenant = Uuid::new_v4();
    let upstream = Uuid::new_v4();

    let post_only = make_route(
        tenant,
        upstream,
        vec![HttpMethod::Post],
        "/v1/chat/completions",
        0,
    );
    repo.create(post_only).await.unwrap();

    let result = repo
        .find_matching(tenant, upstream, "HEAD", "/v1/chat/completions")
        .await;
    assert!(matches!(result, Err(RepositoryError::NotFound { .. })));
}

#[tokio::test]
async fn list_by_upstream_returns_correct_set() {
    let repo = InMemoryRouteRepo::new();
    let tenant = Uuid::new_v4();
    let u1 = Uuid::new_v4();
    let u2 = Uuid::new_v4();

    repo.create(make_route(tenant, u1, vec![HttpMethod::Post], "/a", 0))
        .await
        .unwrap();
    repo.create(make_route(tenant, u1, vec![HttpMethod::Get], "/b", 0))
        .await
        .unwrap();
    repo.create(make_route(tenant, u2, vec![HttpMethod::Post], "/c", 0))
        .await
        .unwrap();

    let routes = repo
        .list(tenant, Some(u1), &ListQuery { top: 50, skip: 0 })
        .await
        .unwrap();
    assert_eq!(routes.len(), 2);
}

#[tokio::test]
async fn cross_tenant_delete_returns_not_found_and_preserves_route() {
    let repo = InMemoryRouteRepo::new();
    let owner = Uuid::new_v4();
    let attacker = Uuid::new_v4();
    let upstream = Uuid::new_v4();

    let route = make_route(owner, upstream, vec![HttpMethod::Post], "/v1/chat", 0);
    let id = route.id;
    repo.create(route).await.unwrap();

    // Different tenant cannot delete it.
    let result = repo.delete(attacker, id).await;
    assert!(matches!(result, Err(RepositoryError::NotFound { .. })));

    // Route remains accessible to the owner.
    let fetched = repo.get_by_id(owner, id).await.unwrap();
    assert_eq!(fetched.id, id);

    // Upstream index is also intact.
    let routes = repo
        .list(owner, Some(upstream), &ListQuery { top: 50, skip: 0 })
        .await
        .unwrap();
    assert_eq!(routes.len(), 1);
}

#[tokio::test]
async fn cross_tenant_cascade_delete_preserves_other_tenant_routes() {
    let repo = InMemoryRouteRepo::new();
    let tenant_a = Uuid::new_v4();
    let tenant_b = Uuid::new_v4();
    let upstream = Uuid::new_v4();

    let route_a = make_route(tenant_a, upstream, vec![HttpMethod::Post], "/a", 0);
    let route_b = make_route(tenant_b, upstream, vec![HttpMethod::Get], "/b", 0);
    repo.create(route_a.clone()).await.unwrap();
    repo.create(route_b.clone()).await.unwrap();

    // Cascade delete for tenant_a should only remove tenant_a's route.
    let deleted = repo.delete_by_upstream(tenant_a, upstream).await.unwrap();
    assert_eq!(deleted, 1);

    // tenant_a's route is gone.
    assert!(repo.get_by_id(tenant_a, route_a.id).await.is_err());

    // tenant_b's route still in store.
    let fetched = repo.get_by_id(tenant_b, route_b.id).await.unwrap();
    assert_eq!(fetched.id, route_b.id);

    // tenant_b's route still in upstream index (list works).
    let routes = repo
        .list(tenant_b, Some(upstream), &ListQuery { top: 50, skip: 0 })
        .await
        .unwrap();
    assert_eq!(routes.len(), 1);
    assert_eq!(routes[0].id, route_b.id);
}

#[tokio::test]
async fn cross_tenant_cascade_delete_routes_findable() {
    let repo = InMemoryRouteRepo::new();
    let tenant_a = Uuid::new_v4();
    let tenant_b = Uuid::new_v4();
    let upstream = Uuid::new_v4();

    let route_a = make_route(tenant_a, upstream, vec![HttpMethod::Post], "/v1/chat", 0);
    let route_b = make_route(tenant_b, upstream, vec![HttpMethod::Get], "/v1/models", 0);
    repo.create(route_a).await.unwrap();
    repo.create(route_b.clone()).await.unwrap();

    // Cascade delete for tenant_a.
    repo.delete_by_upstream(tenant_a, upstream).await.unwrap();

    // tenant_b's route still findable via find_matching.
    let matched = repo
        .find_matching(tenant_b, upstream, "GET", "/v1/models")
        .await
        .unwrap();
    assert_eq!(matched.id, route_b.id);
}

#[tokio::test]
async fn delete_by_upstream_cascade() {
    let repo = InMemoryRouteRepo::new();
    let tenant = Uuid::new_v4();
    let upstream = Uuid::new_v4();

    let r1 = make_route(tenant, upstream, vec![HttpMethod::Post], "/a", 0);
    let r2 = make_route(tenant, upstream, vec![HttpMethod::Get], "/b", 0);
    repo.create(r1.clone()).await.unwrap();
    repo.create(r2.clone()).await.unwrap();

    let deleted = repo.delete_by_upstream(tenant, upstream).await.unwrap();
    assert_eq!(deleted, 2);

    assert!(repo.get_by_id(tenant, r1.id).await.is_err());
    assert!(repo.get_by_id(tenant, r2.id).await.is_err());
}
