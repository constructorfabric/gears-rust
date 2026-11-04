//! Directory-resolving wrappers over both REST clients, as `#[toolkit::consumes]` registers
//! them for a consumer the registry is not linked into (SPEC §8.4).
//!
//! Each call resolves the registry anew and rebuilds the client only when the endpoint
//! changes (`DirectoryResolvingClient`). While the directory has no live registry, or cannot
//! answer, a call is a canonical `ServiceUnavailable`; nothing falls back to another
//! implementation.

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
use crate::contract::{PlatformTypesRegistryApi, TypesRegistryApi};
use crate::models::{
    BatchGetEntitiesRequest, BatchGetEntitiesResponse, DeleteEntitiesRequest, DeletionOperation,
    IdempotencyKey, ListEntitiesRequest, ListEntitiesResponse, Operation, RegisterEntitiesRequest,
    RegistrationOperation,
};

/// [`PlatformTypesRegistryApi`] on whichever registry the directory names. The tuning's
/// internal-token source rides on every client built, so each attempt carries the token.
pub struct PlatformTypesRegistryApiRestResolvingClient {
    inner: DirectoryResolvingClient<PlatformTypesRegistryApiRestClient>,
}

impl PlatformTypesRegistryApiRestResolvingClient {
    /// Resolve `from_gear` through `resolver` on every call.
    #[must_use]
    pub fn new(
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
impl PlatformTypesRegistryApi for PlatformTypesRegistryApiRestResolvingClient {
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
}

/// [`TypesRegistryApi`] on whichever registry the directory names. Each call forwards its
/// own caller's bearer; the tuning's internal-token source is never used.
pub struct TypesRegistryApiRestResolvingClient {
    inner: DirectoryResolvingClient<TypesRegistryApiRestClient>,
}

impl TypesRegistryApiRestResolvingClient {
    /// Resolve `from_gear` through `resolver` on every call.
    #[must_use]
    pub fn new(
        resolver: Arc<dyn EndpointResolver>,
        from_gear: impl Into<String>,
        tuning: ClientTuning,
    ) -> Self {
        let inner = DirectoryResolvingClient::new(resolver, from_gear, tuning, |config| {
            TypesRegistryApiRestClient::new(config).map_err(TransportError::network)
        });
        Self { inner }
    }

    async fn client(&self) -> Result<Arc<TypesRegistryApiRestClient>, CanonicalError> {
        Ok(self.inner.resolved().await?)
    }
}

#[async_trait]
impl TypesRegistryApi for TypesRegistryApiRestResolvingClient {
    async fn batch_get_entities(
        &self,
        ctx: &SecurityContext,
        request: BatchGetEntitiesRequest,
    ) -> Result<BatchGetEntitiesResponse, CanonicalError> {
        self.client().await?.batch_get_entities(ctx, request).await
    }

    async fn list_entities(
        &self,
        ctx: &SecurityContext,
        request: ListEntitiesRequest,
    ) -> Result<ListEntitiesResponse, CanonicalError> {
        self.client().await?.list_entities(ctx, request).await
    }
}
