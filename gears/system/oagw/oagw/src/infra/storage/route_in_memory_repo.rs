use crate::domain::model::{ListQuery, ManagedBy, Route};
use crate::domain::repo::{RepositoryError, RouteRepository, RowKey, Tags};
use crate::domain::route_matching::{match_rank, parse_method};
use async_trait::async_trait;
use dashmap::DashMap;
use toolkit_macros::domain_model;
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
    /// Create an empty repository.
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

        match self.store.entry(route_id) {
            dashmap::mapref::entry::Entry::Occupied(_) => {
                return Err(RepositoryError::Conflict {
                    entity: "route",
                    resource: route_id.to_string(),
                    detail: format!("route id '{route_id}' already exists"),
                });
            }
            dashmap::mapref::entry::Entry::Vacant(entry) => {
                entry.insert(route.clone());
            }
        }

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

    async fn list(
        &self,
        tenant_id: Uuid,
        upstream_id: Option<Uuid>,
        query: &ListQuery,
    ) -> Result<Vec<Route>, RepositoryError> {
        let mut routes: Vec<Route> = if let Some(uid) = upstream_id {
            let route_ids: Vec<Uuid> = self
                .upstream_index
                .get(&uid)
                .map(|ids| ids.clone())
                .unwrap_or_default();

            route_ids
                .iter()
                .filter_map(|id| {
                    self.store
                        .get(id)
                        .filter(|r| r.tenant_id == tenant_id)
                        .map(|r| r.clone())
                })
                .collect()
        } else {
            self.store
                .iter()
                .filter(|r| r.tenant_id == tenant_id)
                .map(|r| r.clone())
                .collect()
        };

        routes.sort_by_key(|r| r.id);

        let skip = query.skip as usize;
        let top = query.top as usize;
        Ok(routes.into_iter().skip(skip).take(top).collect())
    }

    async fn find_matching_in_tenants(
        &self,
        tenant_chain: &[Uuid],
        upstream_ids: &[Uuid],
        method: &str,
        path: &str,
        tags: Tags,
    ) -> Result<Route, RepositoryError> {
        let not_found = RepositoryError::NotFound {
            entity: "route",
            id: Uuid::nil(),
        };
        let Some(method) = parse_method(method) else {
            return Err(not_found);
        };
        let route_ids: Vec<Uuid> = upstream_ids
            .iter()
            .filter_map(|uid| self.upstream_index.get(uid).map(|ids| ids.clone()))
            .flatten()
            .collect();
        // Rank each candidate in place; clone only a new best.
        let mut best = None;
        for id in route_ids {
            let Some(route) = self.store.get(&id) else {
                continue;
            };
            let Some(rank) = match_rank(&route, tenant_chain, upstream_ids, method, path) else {
                continue;
            };
            if best.as_ref().is_none_or(|(best_rank, _)| rank < *best_rank) {
                best = Some((rank, route.clone()));
            }
        }
        let (_, mut route) = best.ok_or(not_found)?;
        if tags == Tags::Skip {
            route.tags.clear();
        }
        Ok(route)
    }

    async fn update(&self, mut route: Route) -> Result<Route, RepositoryError> {
        let mut stored = self
            .store
            .get_mut(&route.id)
            .filter(|r| r.tenant_id == route.tenant_id)
            .ok_or(RepositoryError::NotFound {
                entity: "route",
                id: route.id,
            })?;
        // The upstream a route belongs to never changes; keep the index valid.
        route.upstream_id = stored.upstream_id;
        route.managed_by = stored.managed_by;
        *stored = route.clone();
        Ok(route)
    }

    async fn delete(&self, tenant_id: Uuid, id: Uuid) -> Result<(), RepositoryError> {
        // Check the tenant and remove under one lock, so of two concurrent
        // deletes only one succeeds, as on the database.
        let (_, route) = self
            .store
            .remove_if(&id, |_, r| r.tenant_id == tenant_id)
            .ok_or(RepositoryError::NotFound {
                entity: "route",
                id,
            })?;

        if let Some(mut ids) = self.upstream_index.get_mut(&route.upstream_id) {
            ids.retain(|rid| *rid != id);
        }
        Ok(())
    }

    async fn delete_by_upstream(
        &self,
        tenant_id: Uuid,
        upstream_id: Uuid,
    ) -> Result<(), RepositoryError> {
        let route_ids: Vec<Uuid> = self
            .upstream_index
            .remove(&upstream_id)
            .map(|(_, ids)| ids)
            .unwrap_or_default();

        let (to_delete, surviving_ids): (Vec<_>, Vec<_>) = route_ids
            .into_iter()
            .partition(|id| self.store.get(id).is_some_and(|r| r.tenant_id == tenant_id));

        for id in &to_delete {
            self.store.remove(id);
        }

        // Rebuild the upstream index for surviving routes.
        if !surviving_ids.is_empty() {
            self.upstream_index.insert(upstream_id, surviving_ids);
        }

        Ok(())
    }

    async fn list_registry_keys(&self) -> Result<Vec<RowKey>, RepositoryError> {
        let mut keys: Vec<RowKey> = self
            .store
            .iter()
            .filter(|e| e.managed_by == ManagedBy::Registry)
            .map(|e| RowKey {
                tenant_id: e.tenant_id,
                id: e.id,
            })
            .collect();
        keys.sort();
        Ok(keys)
    }
}

// The repository contract is covered by `conformance_tests`. These tests
// check that the in-memory upstream index stays consistent when tenants share
// one upstream ID, which the database schema rules out.
#[cfg(test)]
mod tests {
    use crate::domain::model::{HttpMatch, HttpMethod, MatchRules, PathSuffixMode};

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
            managed_by: ManagedBy::Api,
        }
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
        repo.delete_by_upstream(tenant_a, upstream).await.unwrap();

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

        // tenant_b's route is still found by find_matching_in_tenants.
        let matched = repo
            .find_matching_in_tenants(&[tenant_b], &[upstream], "GET", "/v1/models", Tags::Load)
            .await
            .unwrap();
        assert_eq!(matched.id, route_b.id);
    }

    /// Of two concurrent deletes of one route, exactly one succeeds; the other
    /// is `NotFound`, as on the database.
    #[test]
    fn concurrent_deletes_of_one_route_succeed_once() {
        let repo = InMemoryRouteRepo::new();
        let tenant_id = Uuid::new_v4();
        let upstream_id = Uuid::new_v4();
        let barrier = std::sync::Barrier::new(2);
        let delete = |id| {
            let rt = tokio::runtime::Builder::new_current_thread()
                .build()
                .expect("runtime");
            barrier.wait();
            rt.block_on(repo.delete(tenant_id, id)).is_ok()
        };
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("runtime");
        for _ in 0..2_000 {
            let route = make_route(tenant_id, upstream_id, vec![HttpMethod::Get], "/r", 0);
            let id = route.id;
            rt.block_on(repo.create(route)).unwrap();
            let succeeded = std::thread::scope(|s| {
                let first = s.spawn(|| delete(id));
                let second = s.spawn(|| delete(id));
                [first.join().unwrap(), second.join().unwrap()]
            });
            assert_eq!(
                succeeded.iter().filter(|ok| **ok).count(),
                1,
                "{succeeded:?}"
            );
        }
        assert!(
            repo.upstream_index
                .get(&upstream_id)
                .is_none_or(|ids| ids.is_empty())
        );
    }
}
