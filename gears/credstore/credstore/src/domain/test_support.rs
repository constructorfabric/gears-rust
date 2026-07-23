//! Shared test infrastructure for domain-layer unit tests.
//!
//! For the GTS registry mock, use `MockTypesRegistryClient` and
//! `make_test_instance` from `types_registry_sdk::testing` directly.

use std::sync::Arc;

use async_trait::async_trait;
use credstore_sdk::{
    CredStoreError, CredStorePluginClientV1, OwnerId, SecretMetadata, SecretValue, SharingMode,
    TenantId,
};
use toolkit_security::SecurityContext;
use uuid::Uuid;

use credstore_sdk::SecretRef;

type WriteFn = Arc<dyn Fn() -> Result<(), CredStoreError> + Send + Sync>;

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

// ── MockPlugin ────────────────────────────────────────────────────────────────

type PluginFn = Arc<dyn Fn() -> Result<Option<SecretMetadata>, CredStoreError> + Send + Sync>;

pub struct MockPlugin {
    handler: PluginFn,
    write_handler: WriteFn,
}

fn ok_write() -> WriteFn {
    Arc::new(|| Ok(()))
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
            write_handler: ok_write(),
        })
    }

    #[must_use]
    pub fn errors_not_found() -> Arc<Self> {
        Arc::new(Self {
            handler: Arc::new(|| Err(CredStoreError::NotFound)),
            write_handler: ok_write(),
        })
    }

    #[must_use]
    pub fn errors_internal(msg: &'static str) -> Arc<Self> {
        Arc::new(Self {
            handler: Arc::new(move || Err(CredStoreError::Internal(msg.into()))),
            write_handler: ok_write(),
        })
    }

    /// A plugin whose `get` returns `None` but whose writes (`put`/`delete`)
    /// fail with `CredStoreError::Internal(msg)`.
    #[must_use]
    pub fn write_errors_internal(msg: &'static str) -> Arc<Self> {
        Arc::new(Self {
            handler: Arc::new(|| Ok(None)),
            write_handler: Arc::new(move || Err(CredStoreError::Internal(msg.into()))),
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

    async fn put(
        &self,
        _ctx: &SecurityContext,
        _key: &SecretRef,
        _value: SecretValue,
        _sharing: SharingMode,
    ) -> Result<(), CredStoreError> {
        (self.write_handler)()
    }

    async fn delete(&self, _ctx: &SecurityContext, _key: &SecretRef) -> Result<(), CredStoreError> {
        (self.write_handler)()
    }
}
