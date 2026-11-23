//! Directory-resolving wrappers over both REST clients, as `#[toolkit::consumes]` registers
//! them for a consumer the registry is not linked into (SPEC §8.4).
//!
//! Each call that reaches the transport resolves the registry anew and rebuilds the client
//! only when the endpoint changes (`DirectoryResolvingClient`). While the directory has no
//! live registry, or cannot answer, such a call is a canonical `ServiceUnavailable`; nothing
//! falls back to another implementation. A read the client cache answers inside
//! its window does not reach the transport.

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

/// [`PlatformTypesRegistryApi`] on whichever registry the directory names, behind the SDK
/// client cache (SPEC §8.3) with [`CacheSettings::default`]: one cache for the logical
/// client, above endpoint resolution, so a read the cache answers resolves nothing and
/// replicas the directory rotates between share it. The tuning's internal-token source
/// rides on every client built, so each attempt carries the token.
///
/// `#[toolkit::consumes]` builds this type with no cache settings of its own; a consumer
/// needing others composes [`CachedPlatformTypesRegistry`] over its own client.
pub struct PlatformTypesRegistryApiRestResolvingClient {
    inner: CachedPlatformTypesRegistry,
}

impl PlatformTypesRegistryApiRestResolvingClient {
    /// Resolve `from_gear` through `resolver` on every call the cache does not answer.
    #[must_use]
    pub fn new(
        resolver: Arc<dyn EndpointResolver>,
        from_gear: impl Into<String>,
        tuning: ClientTuning,
    ) -> Self {
        let resolving = PlatformResolving::new(resolver, from_gear, tuning);
        Self {
            inner: CachedPlatformTypesRegistry::new(Arc::new(resolving), CacheSettings::default()),
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

/// [`TypesRegistryApi`] on whichever registry the directory names, with an SDK cache
/// shared across tenants. Each request to the registry forwards the caller's bearer.
pub struct TypesRegistryApiRestResolvingClient {
    inner: CachedTypesRegistry,
}

impl TypesRegistryApiRestResolvingClient {
    /// Resolve `from_gear` through `resolver` on calls that reach the registry.
    #[must_use]
    pub fn new(
        resolver: Arc<dyn EndpointResolver>,
        from_gear: impl Into<String>,
        tuning: ClientTuning,
    ) -> Self {
        let inner = DirectoryResolvingClient::new(resolver, from_gear, tuning, |config| {
            TypesRegistryApiRestClient::new(config).map_err(TransportError::network)
        });
        Self {
            inner: CachedTypesRegistry::new_rest(
                Arc::new(TenantResolving { inner }),
                CacheSettings::default(),
            ),
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

    /// The cache's visibility-confirmed snapshots, so a known identity resolves nothing.
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
