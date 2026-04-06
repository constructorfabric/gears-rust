use crate::domain::model::{ListQuery, Upstream};
use crate::domain::repo::{RepositoryError, UpstreamRepository};
use async_trait::async_trait;
use dashmap::DashMap;
use modkit_macros::domain_model;
use uuid::Uuid;

/// In-memory upstream repository backed by `DashMap`.
#[domain_model]
pub struct InMemoryUpstreamRepo {
    /// Primary store: id -> Upstream.
    store: DashMap<Uuid, Upstream>,
    /// Alias index: (tenant_id, alias) -> upstream_id.
    alias_index: DashMap<(Uuid, String), Uuid>,
}

impl InMemoryUpstreamRepo {
    #[must_use]
    pub fn new() -> Self {
        Self {
            store: DashMap::new(),
            alias_index: DashMap::new(),
        }
    }
}

impl Default for InMemoryUpstreamRepo {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl UpstreamRepository for InMemoryUpstreamRepo {
    async fn create(&self, upstream: Upstream) -> Result<Upstream, RepositoryError> {
        let alias_key = (upstream.tenant_id, upstream.alias.clone());

        // Atomic alias uniqueness check via entry API.
        match self.alias_index.entry(alias_key) {
            dashmap::mapref::entry::Entry::Occupied(_) => {
                return Err(RepositoryError::Conflict(format!(
                    "alias '{}' already exists for tenant",
                    upstream.alias
                )));
            }
            dashmap::mapref::entry::Entry::Vacant(entry) => {
                entry.insert(upstream.id);
            }
        }

        self.store.insert(upstream.id, upstream.clone());
        Ok(upstream)
    }

    async fn get_by_id(&self, tenant_id: Uuid, id: Uuid) -> Result<Upstream, RepositoryError> {
        self.store
            .get(&id)
            .filter(|u| u.tenant_id == tenant_id)
            .map(|u| u.clone())
            .ok_or(RepositoryError::NotFound {
                entity: "upstream",
                id,
            })
    }

    async fn get_by_alias(
        &self,
        tenant_id: Uuid,
        alias: &str,
    ) -> Result<Upstream, RepositoryError> {
        let id = self
            .alias_index
            .get(&(tenant_id, alias.to_string()))
            .map(|r| *r.value())
            .ok_or(RepositoryError::NotFound {
                entity: "upstream",
                id: Uuid::nil(),
            })?;
        self.get_by_id(tenant_id, id).await
    }

    async fn list(
        &self,
        tenant_id: Uuid,
        query: &ListQuery,
    ) -> Result<Vec<Upstream>, RepositoryError> {
        let mut all: Vec<Upstream> = self
            .store
            .iter()
            .filter(|e| e.value().tenant_id == tenant_id)
            .map(|e| e.value().clone())
            .collect();

        all.sort_by_key(|u| u.id);

        let skip = query.skip as usize;
        let top = query.top as usize;
        Ok(all.into_iter().skip(skip).take(top).collect())
    }

    async fn update(&self, upstream: Upstream) -> Result<Upstream, RepositoryError> {
        let id = upstream.id;
        let tenant_id = upstream.tenant_id;

        // Get the old upstream to remove old alias if changed.
        let old = self
            .store
            .get(&id)
            .filter(|u| u.tenant_id == tenant_id)
            .map(|u| u.clone())
            .ok_or(RepositoryError::NotFound {
                entity: "upstream",
                id,
            })?;

        // If alias changed, swap in the alias index.
        // Note: we avoid using entry() + remove() on the same DashMap because
        // holding a shard lock from entry() while remove() tries to lock
        // another key can deadlock if both keys hash to the same shard.
        if old.alias != upstream.alias {
            let new_alias_key = (tenant_id, upstream.alias.clone());
            if self.alias_index.contains_key(&new_alias_key) {
                return Err(RepositoryError::Conflict(format!(
                    "alias '{}' already exists for tenant",
                    upstream.alias
                )));
            }
            self.alias_index.remove(&(tenant_id, old.alias.clone()));
            self.alias_index.insert(new_alias_key, id);
        }

        self.store.insert(id, upstream.clone());
        Ok(upstream)
    }

    async fn delete(&self, tenant_id: Uuid, id: Uuid) -> Result<(), RepositoryError> {
        // Atomically remove first, then verify tenant ownership.
        let (_, upstream) = self.store.remove(&id).ok_or(RepositoryError::NotFound {
            entity: "upstream",
            id,
        })?;

        if upstream.tenant_id != tenant_id {
            // Wrong tenant — put it back and report not-found.
            self.store.insert(id, upstream);
            return Err(RepositoryError::NotFound {
                entity: "upstream",
                id,
            });
        }

        self.alias_index.remove(&(tenant_id, upstream.alias));
        Ok(())
    }
}

#[cfg(test)]
#[path = "upstream_repo_tests.rs"]
mod upstream_repo_tests;
