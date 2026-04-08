//! Shared test infrastructure for domain-layer unit tests.
//!
//! Provides `MockRegistry` and `MockPlugin` used by both `service` and
//! `local_client` test modules.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use credstore_sdk::{
    CredStoreError, CredStorePluginClientV1, OwnerId, SecretMetadata, SecretValue, SharingMode,
    TenantId,
};
use modkit_security::SecurityContext;
use types_registry_sdk::{
    GtsEntity, ListQuery, RegisterResult, TypesRegistryClient, TypesRegistryError,
};
use uuid::Uuid;

use credstore_sdk::SecretRef;

// ── SecurityContext ───────────────────────────────────────────────────────────

/// Build a minimal [`SecurityContext`] suitable for unit tests.
///
/// # Panics
///
/// Panics if the builder fails, which cannot happen with `Uuid::nil()` inputs.
#[must_use]
pub fn test_ctx() -> SecurityContext {
    SecurityContext::builder()
        .subject_id(Uuid::nil())
        .subject_tenant_id(Uuid::nil())
        .build()
        .unwrap()
}

// ── MockRegistry ──────────────────────────────────────────────────────────────

pub struct MockRegistry {
    pub instances: Vec<GtsEntity>,
    pub list_calls: AtomicUsize,
    list_error: Option<TypesRegistryError>,
}

impl MockRegistry {
    #[must_use]
    pub fn new(instances: Vec<GtsEntity>) -> Self {
        Self {
            instances,
            list_calls: AtomicUsize::new(0),
            list_error: None,
        }
    }

    #[must_use]
    pub fn failing(err: TypesRegistryError) -> Self {
        Self {
            instances: vec![],
            list_calls: AtomicUsize::new(0),
            list_error: Some(err),
        }
    }
}

#[async_trait]
impl TypesRegistryClient for MockRegistry {
    async fn list(&self, _query: ListQuery) -> Result<Vec<GtsEntity>, TypesRegistryError> {
        self.list_calls.fetch_add(1, Ordering::SeqCst);
        if let Some(ref e) = self.list_error {
            return Err(e.clone());
        }
        Ok(self.instances.clone())
    }

    async fn get(&self, gts_id: &str) -> Result<GtsEntity, TypesRegistryError> {
        self.instances
            .iter()
            .find(|e| e.gts_id == gts_id)
            .cloned()
            .ok_or_else(|| TypesRegistryError::not_found(gts_id))
    }

    async fn register(
        &self,
        _entities: Vec<serde_json::Value>,
    ) -> Result<Vec<RegisterResult>, TypesRegistryError> {
        Ok(vec![])
    }
}

// ── MockPlugin ────────────────────────────────────────────────────────────────

type PluginFn = Arc<dyn Fn() -> Result<Option<SecretMetadata>, CredStoreError> + Send + Sync>;

pub struct MockPlugin {
    handler: PluginFn,
}

impl MockPlugin {
    #[must_use]
    pub fn returns(meta: Option<&SecretMetadata>) -> Arc<Self> {
        let bytes = meta.map(|m| m.value.as_bytes().to_vec());
        let owner_id = meta.map_or(OwnerId::nil(), |m| m.owner_id);
        let sharing = meta.map_or(SharingMode::Tenant, |m| m.sharing);
        let owner_tenant_id = meta.map_or(TenantId::nil(), |m| m.owner_tenant_id);
        Arc::new(Self {
            handler: Arc::new(move || {
                Ok(bytes.as_ref().map(|b| SecretMetadata {
                    value: SecretValue::new(b.clone()),
                    owner_id,
                    sharing,
                    owner_tenant_id,
                }))
            }),
        })
    }

    #[must_use]
    pub fn errors_not_found() -> Arc<Self> {
        Arc::new(Self {
            handler: Arc::new(|| Err(CredStoreError::NotFound)),
        })
    }

    #[must_use]
    pub fn errors_internal(msg: &'static str) -> Arc<Self> {
        Arc::new(Self {
            handler: Arc::new(move || Err(CredStoreError::Internal(msg.into()))),
        })
    }
}

#[async_trait]
impl CredStorePluginClientV1 for MockPlugin {
    async fn get(
        &self,
        _ctx: &SecurityContext,
        _key: &SecretRef,
    ) -> Result<Option<SecretMetadata>, CredStoreError> {
        (self.handler)()
    }
}
