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
#[path = "route_repo_tests.rs"]
mod route_repo_tests;
