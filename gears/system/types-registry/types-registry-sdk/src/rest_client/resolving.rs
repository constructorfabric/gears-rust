//! Directory-resolving wrappers over both REST clients, as `#[toolkit::consumes]` registers
//! them for a consumer the registry is not linked into (SPEC §8.4).
//!
//! Forwarded calls resolve through the directory. The SDK rebuilds the client
//! only when the endpoint changes. A missing registry or directory failure
//! returns `ServiceUnavailable`. Cache hits bypass resolution.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use toolkit_canonical_errors::CanonicalError;
use toolkit_contract::runtime::resolving::{DirectoryResolvingClient, EndpointResolver};
use toolkit_contract::runtime::transport_error::TransportError;
use toolkit_contract::wiring::ClientTuning;
use toolkit_security::{PlatformSecurityContext, SecurityContext};
use uuid::Uuid;

use super::client::TypesRegistryApiRestClient;
use super::platform_client::PlatformTypesRegistryApiRestClient;
use crate::cache::{CacheSettings, CachedPlatformTypesRegistry, CachedTypesRegistry};
use crate::contract::{PlatformTypesRegistryApi, TypesRegistryApi};
use crate::models::{
    BatchGetEntitiesRequest, BatchGetEntitiesResponse, DeleteEntitiesRequest, DeletionOperation,
    EntityIdentity, EntityKey, IdempotencyKey, ListEntitiesRequest, ListEntitiesResponse,
    Operation, RegisterEntitiesRequest, RegistrationOperation,
};

/// One platform cache across resolved replicas. Registry requests use the internal
/// token. Set both entry limits to zero to disable retention.
pub struct PlatformTypesRegistryApiRestResolvingClient {
    inner: CachedPlatformTypesRegistry,
}

impl PlatformTypesRegistryApiRestResolvingClient {
    /// Use [`CacheSettings::default`], as in `#[toolkit::consumes]`.
    #[must_use]
    pub fn new(
        resolver: Arc<dyn EndpointResolver>,
        from_gear: impl Into<String>,
        tuning: ClientTuning,
    ) -> Self {
        Self::with_cache_settings(resolver, from_gear, tuning, CacheSettings::default())
    }

    /// Use explicit cache settings.
    #[must_use]
    pub fn with_cache_settings(
        resolver: Arc<dyn EndpointResolver>,
        from_gear: impl Into<String>,
        tuning: ClientTuning,
        cache: CacheSettings,
    ) -> Self {
        let resolving = PlatformResolving::new(resolver, from_gear, tuning);
        Self {
            inner: CachedPlatformTypesRegistry::new(Arc::new(resolving), cache),
        }
    }
}

#[async_trait]
impl PlatformTypesRegistryApi for PlatformTypesRegistryApiRestResolvingClient {
    async fn batch_get_entities(
        &self,
        ctx: &PlatformSecurityContext,
        request: BatchGetEntitiesRequest,
    ) -> Result<BatchGetEntitiesResponse, CanonicalError> {
        self.inner.batch_get_entities(ctx, request).await
    }

    async fn list_entities(
        &self,
        ctx: &PlatformSecurityContext,
        request: ListEntitiesRequest,
    ) -> Result<ListEntitiesResponse, CanonicalError> {
        self.inner.list_entities(ctx, request).await
    }

    async fn register_entities(
        &self,
        ctx: &PlatformSecurityContext,
        key: IdempotencyKey,
        request: RegisterEntitiesRequest,
    ) -> Result<RegistrationOperation, CanonicalError> {
        self.inner.register_entities(ctx, key, request).await
    }

    async fn delete_entities(
        &self,
        ctx: &PlatformSecurityContext,
        key: IdempotencyKey,
        request: DeleteEntitiesRequest,
    ) -> Result<DeletionOperation, CanonicalError> {
        self.inner.delete_entities(ctx, key, request).await
    }

    async fn get_operation(
        &self,
        ctx: &PlatformSecurityContext,
        operation_id: Uuid,
    ) -> Result<Operation, CanonicalError> {
        self.inner.get_operation(ctx, operation_id).await
    }

    /// The cache's identity store, so a known identity resolves nothing.
    async fn batch_resolve_identities(
        &self,
        ctx: &PlatformSecurityContext,
        keys: Vec<EntityKey>,
    ) -> Result<HashMap<EntityKey, Option<EntityIdentity>>, CanonicalError> {
        self.inner.batch_resolve_identities(ctx, keys).await
    }
}

/// The uncached platform client on whichever registry the directory names.
struct PlatformResolving {
    inner: DirectoryResolvingClient<PlatformTypesRegistryApiRestClient>,
}

impl PlatformResolving {
    fn new(
        resolver: Arc<dyn EndpointResolver>,
        from_gear: impl Into<String>,
        tuning: ClientTuning,
    ) -> Self {
        let inner = DirectoryResolvingClient::new(resolver, from_gear, tuning, |config| {
            PlatformTypesRegistryApiRestClient::new(config).map_err(TransportError::network)
        });
        Self { inner }
    }

    async fn client(&self) -> Result<Arc<PlatformTypesRegistryApiRestClient>, CanonicalError> {
        Ok(self.inner.resolved().await?)
    }
}

#[async_trait]
impl PlatformTypesRegistryApi for PlatformResolving {
    async fn batch_get_entities(
        &self,
        ctx: &PlatformSecurityContext,
        request: BatchGetEntitiesRequest,
    ) -> Result<BatchGetEntitiesResponse, CanonicalError> {
        self.client().await?.batch_get_entities(ctx, request).await
    }

    async fn list_entities(
        &self,
        ctx: &PlatformSecurityContext,
        request: ListEntitiesRequest,
    ) -> Result<ListEntitiesResponse, CanonicalError> {
        self.client().await?.list_entities(ctx, request).await
    }

    async fn register_entities(
        &self,
        ctx: &PlatformSecurityContext,
        key: IdempotencyKey,
        request: RegisterEntitiesRequest,
    ) -> Result<RegistrationOperation, CanonicalError> {
        self.client()
            .await?
            .register_entities(ctx, key, request)
            .await
    }

    async fn delete_entities(
        &self,
        ctx: &PlatformSecurityContext,
        key: IdempotencyKey,
        request: DeleteEntitiesRequest,
    ) -> Result<DeletionOperation, CanonicalError> {
        self.client()
            .await?
            .delete_entities(ctx, key, request)
            .await
    }

    async fn get_operation(
        &self,
        ctx: &PlatformSecurityContext,
        operation_id: Uuid,
    ) -> Result<Operation, CanonicalError> {
        self.client().await?.get_operation(ctx, operation_id).await
    }

    async fn batch_resolve_identities(
        &self,
        ctx: &PlatformSecurityContext,
        keys: Vec<EntityKey>,
    ) -> Result<HashMap<EntityKey, Option<EntityIdentity>>, CanonicalError> {
        self.client()
            .await?
            .batch_resolve_identities(ctx, keys)
            .await
    }
}

/// Tenant cache over directory-resolved replicas. Hits follow [`crate::CachedTypesRegistry`]
/// visibility rules. REST misses forward the caller's bearer. See [`CacheSettings`] for limits.
pub struct TypesRegistryApiRestResolvingClient {
    inner: CachedTypesRegistry,
}

impl TypesRegistryApiRestResolvingClient {
    /// Use [`CacheSettings::default`], as in `#[toolkit::consumes]`.
    #[must_use]
    pub fn new(
        resolver: Arc<dyn EndpointResolver>,
        from_gear: impl Into<String>,
        tuning: ClientTuning,
    ) -> Self {
        Self::with_cache_settings(resolver, from_gear, tuning, CacheSettings::default())
    }

    /// Use explicit cache settings.
    #[must_use]
    pub fn with_cache_settings(
        resolver: Arc<dyn EndpointResolver>,
        from_gear: impl Into<String>,
        tuning: ClientTuning,
        cache: CacheSettings,
    ) -> Self {
        let inner = DirectoryResolvingClient::new(resolver, from_gear, tuning, |config| {
            TypesRegistryApiRestClient::new(config).map_err(TransportError::network)
        });
        Self {
            inner: CachedTypesRegistry::new(Arc::new(TenantResolving { inner }), cache),
        }
    }
}

#[async_trait]
impl TypesRegistryApi for TypesRegistryApiRestResolvingClient {
    async fn batch_get_entities(
        &self,
        ctx: &SecurityContext,
        request: BatchGetEntitiesRequest,
    ) -> Result<BatchGetEntitiesResponse, CanonicalError> {
        self.inner.batch_get_entities(ctx, request).await
    }

    async fn list_entities(
        &self,
        ctx: &SecurityContext,
        request: ListEntitiesRequest,
    ) -> Result<ListEntitiesResponse, CanonicalError> {
        self.inner.list_entities(ctx, request).await
    }

    /// Confirmed identity hits bypass directory resolution.
    async fn batch_resolve_identities(
        &self,
        ctx: &SecurityContext,
        keys: Vec<EntityKey>,
    ) -> Result<HashMap<EntityKey, Option<EntityIdentity>>, CanonicalError> {
        self.inner.batch_resolve_identities(ctx, keys).await
    }
}

struct TenantResolving {
    inner: DirectoryResolvingClient<TypesRegistryApiRestClient>,
}

#[async_trait]
impl TypesRegistryApi for TenantResolving {
    async fn batch_get_entities(
        &self,
        ctx: &SecurityContext,
        request: BatchGetEntitiesRequest,
    ) -> Result<BatchGetEntitiesResponse, CanonicalError> {
        self.inner
            .resolved()
            .await?
            .batch_get_entities(ctx, request)
            .await
    }

    async fn list_entities(
        &self,
        ctx: &SecurityContext,
        request: ListEntitiesRequest,
    ) -> Result<ListEntitiesResponse, CanonicalError> {
        self.inner
            .resolved()
            .await?
            .list_entities(ctx, request)
            .await
    }

    async fn batch_resolve_identities(
        &self,
        ctx: &SecurityContext,
        keys: Vec<EntityKey>,
    ) -> Result<HashMap<EntityKey, Option<EntityIdentity>>, CanonicalError> {
        self.inner
            .resolved()
            .await?
            .batch_resolve_identities(ctx, keys)
            .await
    }
}
