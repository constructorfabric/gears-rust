// Updated: 2026-06-06 — adapted to the per-tenant value-store `CredStorePluginClientV1`.
use std::collections::HashMap;
use std::sync::RwLock;

use credstore_sdk::{OwnerId, SecretRef, SecretValue, SharingMode, TenantId};
use toolkit_macros::domain_model;
use uuid::Uuid;

use crate::config::StaticCredStorePluginConfig;

/// In-memory key classes backing the static plugin.
///
/// The `private`/`tenant` classes mirror the two classes the gear selects
/// via `owner_id` (`Some`/`None`). The `shared`/`global` maps hold
/// config-seeded entries that have no equivalent at write time; they are kept
/// as read-only fallbacks for `owner_id = None` lookups so existing
/// development configs keep resolving.
#[domain_model]
#[derive(Debug, Default)]
struct Store {
    /// Private key class — keyed by `(tenant, owner, key)`.
    private: HashMap<(TenantId, OwnerId, SecretRef), SecretValue>,
    /// Tenant key class — keyed by `(tenant, key)`.
    tenant: HashMap<(TenantId, SecretRef), SecretValue>,
    /// Config-seeded `shared` secrets — read fallback for the tenant key class.
    shared: HashMap<(TenantId, SecretRef), SecretValue>,
    /// Config-seeded global secrets — final read fallback for the tenant class.
    global: HashMap<SecretRef, SecretValue>,
}

/// Static credstore backend.
///
/// A pure per-tenant value store implementing the `CredStorePluginClientV1`
/// contract: `owner_id = Some` selects the private key class, `None` the
/// tenant key class. Sharing, hierarchy and policy live in the gear, not
/// here.
///
/// The store is seeded from configuration and stays mutable at runtime, so the
/// stateful gear's write saga (`put`/`delete`) can use it as a development
/// backend. Config-seeded `shared`/global entries remain read-only fallbacks
/// for `owner_id = None` lookups.
#[domain_model]
#[derive(Debug, Default)]
pub struct Service {
    inner: RwLock<Store>,
}

impl Service {
    /// Create a service from plugin configuration.
    ///
    /// Validates each configured key via `SecretRef::new` and seeds the
    /// in-memory key classes from the resolved sharing mode.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - any configured key fails `SecretRef` validation
    /// - duplicate keys within the same sharing scope
    /// - a secret without `owner_id` has an explicit `SharingMode::Private`
    /// - `tenant_id` or `owner_id` is an explicit nil UUID
    /// - `owner_id` is set without `tenant_id`
    pub fn from_config(cfg: &StaticCredStorePluginConfig) -> anyhow::Result<Self> {
        let mut store = Store::default();

        for entry in &cfg.secrets {
            if entry.tenant_id == Some(Uuid::nil()) {
                anyhow::bail!("secret '{}': tenant_id must not be nil UUID", entry.key);
            }
            if entry.owner_id == Some(Uuid::nil()) {
                anyhow::bail!("secret '{}': owner_id must not be nil UUID", entry.key);
            }
            if entry.tenant_id.is_none() && entry.owner_id.is_some() {
                anyhow::bail!(
                    "secret '{}': owner_id cannot be set without tenant_id",
                    entry.key
                );
            }

            let sharing = entry.resolve_sharing();

            if entry.owner_id.is_some() && sharing != SharingMode::Private {
                anyhow::bail!(
                    "secret '{}': owner_id is only valid for private sharing mode, \
                     but resolved sharing is {sharing:?}",
                    entry.key
                );
            }
            if entry.owner_id.is_none() && sharing == SharingMode::Private {
                anyhow::bail!(
                    "secret '{}' with sharing mode 'private' requires an explicit owner_id",
                    entry.key
                );
            }

            let key = SecretRef::new(&entry.key)?;
            let value = SecretValue::from(entry.value.as_str());

            match (sharing, entry.tenant_id) {
                (SharingMode::Shared, None) => {
                    if store.global.contains_key(&key) {
                        anyhow::bail!("duplicate global secret key '{}'", entry.key);
                    }
                    store.global.insert(key, value);
                }
                (SharingMode::Shared, Some(raw_tenant_id)) => {
                    let tenant_id = TenantId(raw_tenant_id);
                    let map_key = (tenant_id, key);
                    if store.shared.contains_key(&map_key) {
                        anyhow::bail!(
                            "duplicate shared secret key '{}' for tenant {}",
                            entry.key,
                            tenant_id
                        );
                    }
                    store.shared.insert(map_key, value);
                }
                (SharingMode::Tenant, _) => {
                    let tenant_id = TenantId(entry.tenant_id.ok_or_else(|| {
                        anyhow::anyhow!(
                            "secret '{}': tenant sharing mode requires tenant_id",
                            entry.key
                        )
                    })?);
                    let map_key = (tenant_id, key);
                    if store.tenant.contains_key(&map_key) {
                        anyhow::bail!(
                            "duplicate tenant secret key '{}' for tenant {}",
                            entry.key,
                            tenant_id
                        );
                    }
                    store.tenant.insert(map_key, value);
                }
                (SharingMode::Private, _) => {
                    let tenant_id = TenantId(entry.tenant_id.ok_or_else(|| {
                        anyhow::anyhow!(
                            "secret '{}': private sharing mode requires tenant_id",
                            entry.key
                        )
                    })?);
                    let owner_id = OwnerId(entry.owner_id.ok_or_else(|| {
                        anyhow::anyhow!(
                            "secret '{}': private sharing mode requires owner_id",
                            entry.key
                        )
                    })?);
                    let map_key = (tenant_id, owner_id, key);
                    if store.private.contains_key(&map_key) {
                        anyhow::bail!(
                            "duplicate private secret key '{}' for tenant {} owner {}",
                            entry.key,
                            tenant_id,
                            owner_id
                        );
                    }
                    store.private.insert(map_key, value);
                }
            }
        }

        Ok(Self {
            inner: RwLock::new(store),
        })
    }

    /// Read a value for the selected key class.
    ///
    /// `owner_id = Some` reads the private class; `None` reads the tenant class
    /// and falls back to config-seeded `shared` then global entries.
    #[must_use]
    pub fn get_value(
        &self,
        tenant_id: &TenantId,
        key: &SecretRef,
        owner_id: Option<&OwnerId>,
    ) -> Option<SecretValue> {
        let store = self
            .inner
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let bytes = match owner_id {
            Some(owner) => store
                .private
                .get(&(*tenant_id, *owner, key.clone()))
                .map(SecretValue::as_bytes),
            None => store
                .tenant
                .get(&(*tenant_id, key.clone()))
                .or_else(|| store.shared.get(&(*tenant_id, key.clone())))
                .or_else(|| store.global.get(key))
                .map(SecretValue::as_bytes),
        };
        // `SecretValue` is not `Clone` (it zeroizes on drop), so reconstruct.
        bytes.map(|b| SecretValue::new(b.to_vec()))
    }

    /// Insert or overwrite a value in the selected key class.
    pub fn put_value(
        &self,
        tenant_id: &TenantId,
        key: &SecretRef,
        value: SecretValue,
        owner_id: Option<&OwnerId>,
    ) {
        let mut store = self
            .inner
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match owner_id {
            Some(owner) => {
                store
                    .private
                    .insert((*tenant_id, *owner, key.clone()), value);
            }
            None => {
                store.tenant.insert((*tenant_id, key.clone()), value);
            }
        }
    }

    /// Remove a value from the selected key class.
    ///
    /// Deletes address the **runtime** maps only. The config-seeded `shared`
    /// and global fallbacks are cross-tenant reference data: a tenant-scoped
    /// delete (including gear saga retries and reaper reconciliation
    /// issued on behalf of one tenant) must never destroy an entry that
    /// serves other tenants. A miss is a no-op — the gear treats a
    /// missing backend value as success.
    pub fn delete_value(&self, tenant_id: &TenantId, key: &SecretRef, owner_id: Option<&OwnerId>) {
        let mut store = self
            .inner
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(owner) = owner_id {
            store.private.remove(&(*tenant_id, *owner, key.clone()));
        } else {
            store.tenant.remove(&(*tenant_id, key.clone()));
        }
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "service_tests.rs"]
mod service_tests;
