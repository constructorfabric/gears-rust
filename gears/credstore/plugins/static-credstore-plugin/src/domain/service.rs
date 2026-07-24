// Updated: 2026-04-07 by Constructor Tech
use std::collections::HashMap;
use std::sync::{PoisonError, RwLock};

use credstore_sdk::{OwnerId, SecretRef, SecretValue, SharingMode, TenantId};
use toolkit_macros::domain_model;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use crate::config::StaticCredStorePluginConfig;

/// Pre-built secret entry for O(1) lookup.
#[domain_model]
pub struct SecretEntry {
    pub value: SecretValue,
    pub sharing: SharingMode,
    pub owner_id: OwnerId,
    pub owner_tenant_id: TenantId,
}

impl Clone for SecretEntry {
    fn clone(&self) -> Self {
        // `SecretValue` intentionally does not derive `Clone` (to discourage
        // copies of secret material); rebuild it explicitly from the bytes.
        Self {
            value: SecretValue::new(self.value.as_bytes().to_vec()),
            sharing: self.sharing,
            owner_id: self.owner_id,
            owner_tenant_id: self.owner_tenant_id,
        }
    }
}

/// Mutable in-memory secret store, guarded by a single [`RwLock`].
///
/// Config-seeded secrets and runtime writes (via `put`) share these maps.
#[derive(Default)]
#[allow(clippy::struct_field_names)]
struct Store {
    private_secrets: HashMap<(TenantId, OwnerId, SecretRef), SecretEntry>,
    tenant_secrets: HashMap<(TenantId, SecretRef), SecretEntry>,
    shared_secrets: HashMap<(TenantId, SecretRef), SecretEntry>,
    global_secrets: HashMap<SecretRef, SecretEntry>,
}

/// Static credstore service.
///
/// Secrets are stored in four maps based on their resolved `SharingMode`
/// and whether a `tenant_id` is present:
///
/// - **`Private`**: keyed by `(TenantId, OwnerId, SecretRef)` — accessible only
///   when both tenant and subject match.
/// - **`Tenant`**: keyed by `(TenantId, SecretRef)` — accessible by any subject
///   within the matching tenant.
/// - **`Shared`**: keyed by `(TenantId, SecretRef)` — tenant-scoped but
///   accessible by descendant tenants via hierarchical resolution in the
///   gateway. The plugin stores them per-tenant; walk-up is the gateway's job.
/// - **Global**: keyed by `SecretRef` only — no `tenant_id`; returned as
///   fallback for any caller. Not a `SharingMode` variant; it is an
///   operational shortcut specific to the static plugin.
///
/// Lookup order: **Private → Tenant → Shared → Global** (most specific first).
///
/// The store is mutable at runtime: [`Service::put`] and [`Service::delete`]
/// update the maps behind an [`RwLock`], while config-seeded secrets are
/// inserted once at construction. Storage is in-memory only (not durable
/// across restarts).
#[domain_model]
pub struct Service {
    store: RwLock<Store>,
}

impl Service {
    /// Create a service from plugin configuration.
    ///
    /// Validates each secret key via `SecretRef::new` and builds the lookup maps.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - any configured key fails `SecretRef` validation
    /// - duplicate keys within the same sharing scope
    /// - a global secret has an explicit sharing mode other than `Shared`
    /// - a secret without `owner_id` has an explicit `SharingMode::Private`
    /// - `tenant_id` or `owner_id` is an explicit nil UUID
    /// - `owner_id` is set without `tenant_id`
    pub fn from_config(cfg: &StaticCredStorePluginConfig) -> anyhow::Result<Self> {
        let mut private_secrets: HashMap<(TenantId, OwnerId, SecretRef), SecretEntry> =
            HashMap::new();
        let mut tenant_secrets: HashMap<(TenantId, SecretRef), SecretEntry> = HashMap::new();
        let mut shared_secrets: HashMap<(TenantId, SecretRef), SecretEntry> = HashMap::new();
        let mut global_secrets: HashMap<SecretRef, SecretEntry> = HashMap::new();

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

            match (sharing, entry.tenant_id) {
                (SharingMode::Shared, None) => {
                    // Global secret: no tenant_id, accessible by any caller.
                    let secret_entry = SecretEntry {
                        value: SecretValue::from(entry.value.as_str()),
                        sharing,
                        owner_id: OwnerId::nil(),
                        owner_tenant_id: TenantId::nil(),
                    };
                    if global_secrets.contains_key(&key) {
                        anyhow::bail!("duplicate global secret key '{}'", entry.key);
                    }
                    global_secrets.insert(key, secret_entry);
                }
                (SharingMode::Shared, Some(raw_tenant_id)) => {
                    // Shared secret: tenant-scoped, visible to descendants
                    // via gateway hierarchical resolution.
                    let tenant_id = TenantId(raw_tenant_id);
                    let secret_entry = SecretEntry {
                        value: SecretValue::from(entry.value.as_str()),
                        sharing,
                        owner_id: OwnerId::nil(),
                        owner_tenant_id: tenant_id,
                    };
                    let map_key = (tenant_id, key);
                    if shared_secrets.contains_key(&map_key) {
                        anyhow::bail!(
                            "duplicate shared secret key '{}' for tenant {}",
                            entry.key,
                            tenant_id
                        );
                    }
                    shared_secrets.insert(map_key, secret_entry);
                }
                (SharingMode::Tenant, _) => {
                    let tenant_id = TenantId(entry.tenant_id.ok_or_else(|| {
                        anyhow::anyhow!(
                            "secret '{}': tenant sharing mode requires tenant_id",
                            entry.key
                        )
                    })?);
                    let secret_entry = SecretEntry {
                        value: SecretValue::from(entry.value.as_str()),
                        sharing,
                        owner_id: OwnerId::nil(),
                        owner_tenant_id: tenant_id,
                    };
                    let map_key = (tenant_id, key);
                    if tenant_secrets.contains_key(&map_key) {
                        anyhow::bail!(
                            "duplicate tenant secret key '{}' for tenant {}",
                            entry.key,
                            tenant_id
                        );
                    }
                    tenant_secrets.insert(map_key, secret_entry);
                }
                (SharingMode::Private, _) => {
                    let tenant_id = TenantId(entry.tenant_id.ok_or_else(|| {
                        anyhow::anyhow!(
                            "secret '{}': private sharing mode requires tenant_id",
                            entry.key
                        )
                    })?);
                    // owner_id is guaranteed Some by the validation above.
                    let owner_id = OwnerId(entry.owner_id.ok_or_else(|| {
                        anyhow::anyhow!(
                            "secret '{}': private sharing mode requires owner_id",
                            entry.key
                        )
                    })?);
                    let secret_entry = SecretEntry {
                        value: SecretValue::from(entry.value.as_str()),
                        sharing,
                        owner_id,
                        owner_tenant_id: tenant_id,
                    };
                    let map_key = (tenant_id, owner_id, key);
                    if private_secrets.contains_key(&map_key) {
                        anyhow::bail!(
                            "duplicate private secret key '{}' for tenant {} owner {}",
                            entry.key,
                            tenant_id,
                            owner_id
                        );
                    }
                    private_secrets.insert(map_key, secret_entry);
                }
            }
        }

        Ok(Self {
            store: RwLock::new(Store {
                private_secrets,
                tenant_secrets,
                shared_secrets,
                global_secrets,
            }),
        })
    }

    /// Look up a secret using the caller's security context.
    ///
    /// Lookup order: **Private → Tenant → Shared → Global** (most specific first).
    ///
    /// Returns an owned entry (the value bytes are cloned) because the backing
    /// maps live behind an [`RwLock`].
    #[must_use]
    pub fn get(&self, ctx: &SecurityContext, key: &SecretRef) -> Option<SecretEntry> {
        let tenant_id = TenantId(ctx.subject_tenant_id());
        let subject_id = OwnerId(ctx.subject_id());

        let store = self.store.read().unwrap_or_else(PoisonError::into_inner);
        store
            .private_secrets
            .get(&(tenant_id, subject_id, key.clone()))
            .or_else(|| store.tenant_secrets.get(&(tenant_id, key.clone())))
            .or_else(|| store.shared_secrets.get(&(tenant_id, key.clone())))
            .or_else(|| store.global_secrets.get(key))
            .cloned()
    }

    /// Store (create or overwrite) a secret scoped by the caller's context.
    ///
    /// - [`SharingMode::Private`] keys by `(tenant, owner, key)`, where owner is
    ///   the caller's subject.
    /// - [`SharingMode::Tenant`] and [`SharingMode::Shared`] key by
    ///   `(tenant, key)`; the stored `owner_id` is a nil placeholder resolved
    ///   from the caller's context at read time (mirrors config-seeded entries).
    ///
    /// There is no consumer-facing path to write a global secret; those are
    /// config-only.
    pub fn put(
        &self,
        ctx: &SecurityContext,
        key: &SecretRef,
        value: SecretValue,
        sharing: SharingMode,
    ) {
        let tenant_id = TenantId(ctx.subject_tenant_id());
        let owner_id = OwnerId(ctx.subject_id());

        let mut store = self.store.write().unwrap_or_else(PoisonError::into_inner);
        match sharing {
            SharingMode::Private => {
                store.private_secrets.insert(
                    (tenant_id, owner_id, key.clone()),
                    SecretEntry {
                        value,
                        sharing,
                        owner_id,
                        owner_tenant_id: tenant_id,
                    },
                );
            }
            SharingMode::Tenant => {
                store.tenant_secrets.insert(
                    (tenant_id, key.clone()),
                    SecretEntry {
                        value,
                        sharing,
                        owner_id: OwnerId::nil(),
                        owner_tenant_id: tenant_id,
                    },
                );
            }
            SharingMode::Shared => {
                store.shared_secrets.insert(
                    (tenant_id, key.clone()),
                    SecretEntry {
                        value,
                        sharing,
                        owner_id: OwnerId::nil(),
                        owner_tenant_id: tenant_id,
                    },
                );
            }
        }
    }

    /// Delete the secret resolved for the caller's context, following the
    /// scoped **Private → Tenant → Shared** precedence as [`Service::get`].
    ///
    /// Global secrets are config-only (see [`Service::put`]) and are never
    /// removed through this consumer-facing path: a missed scoped lookup
    /// returns without mutating global configuration.
    ///
    /// Idempotent: does nothing if no scoped entry matches.
    pub fn delete(&self, ctx: &SecurityContext, key: &SecretRef) {
        let tenant_id = TenantId(ctx.subject_tenant_id());
        let owner_id = OwnerId(ctx.subject_id());

        let mut store = self.store.write().unwrap_or_else(PoisonError::into_inner);
        if store
            .private_secrets
            .remove(&(tenant_id, owner_id, key.clone()))
            .is_some()
        {
            return;
        }
        if store
            .tenant_secrets
            .remove(&(tenant_id, key.clone()))
            .is_some()
        {
            return;
        }
        store.shared_secrets.remove(&(tenant_id, key.clone()));
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "service_tests.rs"]
mod service_tests;
