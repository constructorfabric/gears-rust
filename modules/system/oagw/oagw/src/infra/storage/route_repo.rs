use crate::domain::model::{HttpMethod, ListQuery, Route};
use crate::domain::repo::{RepositoryError, RouteRepository};
use async_trait::async_trait;
use dashmap::DashMap;
use modkit_macros::domain_model;
use uuid::Uuid;

/// In-memory route repository backed by `DashMap`.
#[domain_model]
pub struct InMemoryRouteRepo {
    /// Primary store: route_id -> Route.
    store: DashMap<Uuid, Route>,
    /// Upstream index: upstream_id -> vec of route_ids.
    upstream_index: DashMap<Uuid, Vec<Uuid>>,
}

impl InMemoryRouteRepo {
    #[must_use]
    pub fn new() -> Self {
        Self {
            store: DashMap::new(),
            upstream_index: DashMap::new(),
        }
    }
}

impl Default for InMemoryRouteRepo {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl RouteRepository for InMemoryRouteRepo {
    async fn create(&self, route: Route) -> Result<Route, RepositoryError> {
        let route_id = route.id;
        let upstream_id = route.upstream_id;

        self.store.insert(route_id, route.clone());

        // Update upstream index.
        self.upstream_index
            .entry(upstream_id)
            .or_default()
            .push(route_id);

        Ok(route)
    }

    async fn get_by_id(&self, tenant_id: Uuid, id: Uuid) -> Result<Route, RepositoryError> {
        self.store
            .get(&id)
            .filter(|r| r.tenant_id == tenant_id)
            .map(|r| r.clone())
            .ok_or(RepositoryError::NotFound {
                entity: "route",
                id,
            })
    }

    async fn list_by_upstream(
        &self,
        tenant_id: Uuid,
        upstream_id: Uuid,
        query: &ListQuery,
    ) -> Result<Vec<Route>, RepositoryError> {
        let route_ids: Vec<Uuid> = self
            .upstream_index
            .get(&upstream_id)
            .map(|ids| ids.clone())
            .unwrap_or_default();

        let mut routes: Vec<Route> = route_ids
            .iter()
            .filter_map(|id| {
                self.store
                    .get(id)
                    .filter(|r| r.tenant_id == tenant_id)
                    .map(|r| r.clone())
            })
            .collect();

        routes.sort_by_key(|r| r.id);

        let skip = query.skip as usize;
        let top = query.top as usize;
        Ok(routes.into_iter().skip(skip).take(top).collect())
    }

    async fn find_matching(
        &self,
        tenant_id: Uuid,
        upstream_id: Uuid,
        method: &str,
        path: &str,
    ) -> Result<Route, RepositoryError> {
        let route_ids: Vec<Uuid> = self
            .upstream_index
            .get(&upstream_id)
            .map(|ids| ids.clone())
            .unwrap_or_default();

        let request_method = parse_method(method);

        let mut best: Option<Route> = None;
        let mut best_path_len = 0;
        let mut best_priority = i32::MIN;

        for id in &route_ids {
            let Some(route_ref) = self.store.get(id) else {
                continue;
            };
            let route = route_ref.value();

            // Must match tenant.
            if route.tenant_id != tenant_id {
                continue;
            }
            // Must be enabled.
            if !route.enabled {
                continue;
            }
            // Must have HTTP match rules.
            let Some(http_match) = &route.match_rules.http else {
                continue;
            };
            // Method must match (unknown methods never match).
            let Some(req_method) = &request_method else {
                continue;
            };
            if !http_match.methods.contains(req_method) {
                continue;
            }
            // Path must be a prefix match.
            if !path.starts_with(&http_match.path) {
                continue;
            }

            let path_len = http_match.path.len();
            let priority = route.priority;

            // Select by longest path prefix, then highest priority.
            if path_len > best_path_len || (path_len == best_path_len && priority > best_priority) {
                best_path_len = path_len;
                best_priority = priority;
                best = Some(route.clone());
            }
        }

        best.ok_or(RepositoryError::NotFound {
            entity: "route",
            id: Uuid::nil(),
        })
    }

    async fn update(&self, route: Route) -> Result<Route, RepositoryError> {
        if !self.store.contains_key(&route.id) {
            return Err(RepositoryError::NotFound {
                entity: "route",
                id: route.id,
            });
        }
        self.store.insert(route.id, route.clone());
        Ok(route)
    }

    async fn delete(&self, tenant_id: Uuid, id: Uuid) -> Result<(), RepositoryError> {
        // Verify tenant ownership before removing to prevent cross-tenant deletion.
        let entry = self
            .store
            .get(&id)
            .filter(|r| r.tenant_id == tenant_id)
            .ok_or(RepositoryError::NotFound {
                entity: "route",
                id,
            })?;
        let upstream_id = entry.upstream_id;
        drop(entry);

        self.store.remove(&id);
        if let Some(mut ids) = self.upstream_index.get_mut(&upstream_id) {
            ids.retain(|rid| *rid != id);
        }
        Ok(())
    }

    async fn delete_by_upstream(
        &self,
        tenant_id: Uuid,
        upstream_id: Uuid,
    ) -> Result<u64, RepositoryError> {
        let route_ids: Vec<Uuid> = self
            .upstream_index
            .remove(&upstream_id)
            .map(|(_, ids)| ids)
            .unwrap_or_default();

        let mut deleted = 0u64;
        let mut surviving_ids: Vec<Uuid> = Vec::new();

        for id in route_ids {
            if let Some((_, route)) = self.store.remove(&id) {
                if route.tenant_id == tenant_id {
                    deleted += 1;
                } else {
                    // Put it back — wrong tenant.
                    self.store.insert(id, route);
                    surviving_ids.push(id);
                }
            }
        }

        // Rebuild the upstream index for surviving routes.
        if !surviving_ids.is_empty() {
            self.upstream_index.insert(upstream_id, surviving_ids);
        }

        Ok(deleted)
    }
}

fn parse_method(s: &str) -> Option<HttpMethod> {
    match s.to_uppercase().as_str() {
        "GET" => Some(HttpMethod::Get),
        "POST" => Some(HttpMethod::Post),
        "PUT" => Some(HttpMethod::Put),
        "DELETE" => Some(HttpMethod::Delete),
        "PATCH" => Some(HttpMethod::Patch),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
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
            .list_by_upstream(tenant, u1, &ListQuery { top: 50, skip: 0 })
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
            .list_by_upstream(owner, upstream, &ListQuery { top: 50, skip: 0 })
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
            .list_by_upstream(tenant_b, upstream, &ListQuery { top: 50, skip: 0 })
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
}
