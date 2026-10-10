use crate::domain::model::{ListQuery, ManagedBy, Upstream};
use crate::domain::repo::{RepositoryError, RowKey, Tags, UpstreamRepository};
use async_trait::async_trait;
use dashmap::DashMap;
use toolkit_macros::domain_model;
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
    /// Create an empty repository.
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
        // Hold the ID slot while reserving the alias: a taken ID never touches
        // the alias index, so an alias never points at another upstream.
        // Lock order: `store`, then `alias_index`; nothing takes them the other way.
        let dashmap::mapref::entry::Entry::Vacant(slot) = self.store.entry(upstream.id) else {
            return Err(RepositoryError::Conflict {
                entity: "upstream",
                resource: upstream.id.to_string(),
                detail: format!("upstream id '{}' already exists", upstream.id),
            });
        };
        match self
            .alias_index
            .entry((upstream.tenant_id, upstream.alias.clone()))
        {
            dashmap::mapref::entry::Entry::Occupied(_) => Err(RepositoryError::Conflict {
                entity: "upstream",
                resource: upstream.alias.clone(),
                detail: format!("alias '{}' already exists for tenant", upstream.alias),
            }),
            dashmap::mapref::entry::Entry::Vacant(alias) => {
                alias.insert(upstream.id);
                slot.insert(upstream.clone());
                Ok(upstream)
            }
        }
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

    async fn update(&self, mut upstream: Upstream) -> Result<Upstream, RepositoryError> {
        let id = upstream.id;
        let tenant_id = upstream.tenant_id;

        // Hold the row for the whole update, so a concurrent delete either
        // runs first (NotFound here) or removes the updated row; it never
        // comes back. Lock order: `store`, then `alias_index`, as in `create`.
        let mut stored = self
            .store
            .get_mut(&id)
            .filter(|u| u.tenant_id == tenant_id)
            .ok_or(RepositoryError::NotFound {
                entity: "upstream",
                id,
            })?;
        upstream.managed_by = stored.managed_by;

        if stored.alias != upstream.alias {
            // Reserve the new alias in one step, so two writers cannot both
            // take it. The entry guard is dropped before the old key is
            // removed: holding it while `remove` locks another key deadlocks
            // when both keys hash to the same shard.
            match self.alias_index.entry((tenant_id, upstream.alias.clone())) {
                dashmap::mapref::entry::Entry::Occupied(_) => {
                    return Err(RepositoryError::Conflict {
                        entity: "upstream",
                        resource: upstream.alias.clone(),
                        detail: format!("alias '{}' already exists for tenant", upstream.alias),
                    });
                }
                dashmap::mapref::entry::Entry::Vacant(slot) => {
                    slot.insert(id);
                }
            }
            self.alias_index.remove(&(tenant_id, stored.alias.clone()));
        }

        *stored = upstream.clone();
        Ok(upstream)
    }

    async fn delete(&self, tenant_id: Uuid, id: Uuid) -> Result<(), RepositoryError> {
        // Check the tenant and remove under one lock: another tenant's delete
        // never takes the row out, even briefly.
        let (_, upstream) = self
            .store
            .remove_if(&id, |_, u| u.tenant_id == tenant_id)
            .ok_or(RepositoryError::NotFound {
                entity: "upstream",
                id,
            })?;

        self.alias_index.remove(&(tenant_id, upstream.alias));
        Ok(())
    }

    async fn list_by_alias_for_tenants(
        &self,
        alias: &str,
        tenant_ids: &std::collections::HashSet<Uuid>,
        tags: Tags,
    ) -> Result<Vec<Upstream>, RepositoryError> {
        // One keyed lookup per tenant, not a scan of every alias.
        let mut key = (Uuid::nil(), alias.to_owned());
        let mut found = Vec::new();
        for tenant_id in tenant_ids {
            key.0 = *tenant_id;
            let Some(id) = self.alias_index.get(&key).map(|id| *id) else {
                continue;
            };
            if let Some(upstream) = self.store.get(&id) {
                let mut upstream = upstream.value().clone();
                if tags == Tags::Skip {
                    upstream.tags.clear();
                }
                found.push(upstream);
            }
        }
        Ok(found)
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

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;

    use super::*;
    use crate::domain::gts_helpers::HTTP_PROTOCOL_ID;
    use crate::domain::model::{Endpoint, Scheme, Server};

    fn upstream(id: Uuid, tenant_id: Uuid, alias: &str) -> Upstream {
        Upstream {
            id,
            tenant_id,
            alias: alias.into(),
            server: Server {
                endpoints: vec![Endpoint {
                    scheme: Scheme::Https,
                    host: "h".into(),
                    port: 443,
                }],
            },
            protocol: HTTP_PROTOCOL_ID.into(),
            enabled: true,
            auth: None,
            headers: None,
            plugins: None,
            rate_limit: None,
            cors: None,
            tags: vec![],
            managed_by: ManagedBy::Api,
        }
    }

    /// Creates, updates (some renaming) and deletes race on a few IDs and
    /// aliases. Afterwards every stored upstream owns exactly its own alias
    /// entry: an update never revives a deleted row or shares an alias.
    #[test]
    fn concurrent_writes_keep_the_alias_index_consistent() {
        // A deadlock would hang the test binary; fail it instead.
        let done = Arc::new(AtomicBool::new(false));
        let watchdog = Arc::clone(&done);
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_secs(120));
            if !watchdog.load(Ordering::SeqCst) {
                eprintln!("concurrent_writes_keep_the_alias_index_consistent deadlocked");
                std::process::abort();
            }
        });

        let repo = InMemoryUpstreamRepo::new();
        let tenant_id = Uuid::new_v4();
        let ids: Vec<Uuid> = (0..16).map(|_| Uuid::new_v4()).collect();
        let aliases: Vec<String> = (0..8).map(|i| format!("a{i}")).collect();
        std::thread::scope(|s| {
            for thread in 0..8u64 {
                let (repo, ids, aliases) = (&repo, &ids, &aliases);
                s.spawn(move || {
                    let rt = tokio::runtime::Builder::new_current_thread()
                        .build()
                        .expect("runtime");
                    // xorshift: a cheap, seeded operation mix per thread.
                    let mut x = thread.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
                    for _ in 0..20_000 {
                        x ^= x << 13;
                        x ^= x >> 7;
                        x ^= x << 17;
                        let id = ids[(x % 16) as usize];
                        let alias = &aliases[((x >> 8) % 8) as usize];
                        match (x >> 16) % 3 {
                            0 => drop(rt.block_on(repo.create(upstream(id, tenant_id, alias)))),
                            1 => drop(rt.block_on(repo.update(upstream(id, tenant_id, alias)))),
                            _ => drop(rt.block_on(repo.delete(tenant_id, id))),
                        }
                    }
                });
            }
        });
        done.store(true, Ordering::SeqCst);

        for row in &repo.store {
            assert_eq!(
                repo.alias_index
                    .get(&(tenant_id, row.alias.clone()))
                    .map(|id| *id),
                Some(row.id),
                "alias '{}' does not point at its upstream",
                row.alias
            );
        }
        assert_eq!(
            repo.store.len(),
            repo.alias_index.len(),
            "an alias entry has no upstream"
        );
    }

    /// Another tenant's delete is `NotFound` and never hides the row from its
    /// owner, not even while the delete runs.
    #[test]
    fn wrong_tenant_delete_never_hides_the_row() {
        let repo = InMemoryUpstreamRepo::new();
        let (owner, other) = (Uuid::new_v4(), Uuid::new_v4());
        let id = Uuid::new_v4();
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("runtime");
        rt.block_on(repo.create(upstream(id, owner, "svc")))
            .unwrap();

        let done = AtomicBool::new(false);
        std::thread::scope(|s| {
            s.spawn(|| {
                let rt = tokio::runtime::Builder::new_current_thread()
                    .build()
                    .expect("runtime");
                for _ in 0..20_000 {
                    assert!(matches!(
                        rt.block_on(repo.delete(other, id)),
                        Err(RepositoryError::NotFound { .. })
                    ));
                }
                done.store(true, Ordering::SeqCst);
            });
            while !done.load(Ordering::SeqCst) {
                assert!(
                    rt.block_on(repo.get_by_id(owner, id)).is_ok(),
                    "the owner lost its upstream to another tenant's delete"
                );
            }
        });
        rt.block_on(repo.delete(owner, id)).unwrap();
        assert!(repo.alias_index.is_empty());
    }
}
