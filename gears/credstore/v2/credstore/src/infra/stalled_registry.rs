//! A registry that never answers, for the tests that expire a budget.
//!
//! Every call stays pending forever without a timer, so a test that gives the
//! code under test a zero budget waits for nothing.

use std::collections::HashMap;

use async_trait::async_trait;
use toolkit_canonical_errors::CanonicalError;
use toolkit_security::PlatformSecurityContext;
use types_registry_sdk::{
    BatchGetEntitiesRequest, BatchGetEntitiesResponse, DeleteEntitiesRequest, DeletionOperation,
    EntityIdentity, EntityKey, IdempotencyKey, ListEntitiesRequest, ListEntitiesResponse,
    Operation, PlatformTypesRegistryApi, RegisterEntitiesRequest, RegistrationOperation,
};
use uuid::Uuid;

/// Answers no call.
pub struct StalledRegistry;

#[async_trait]
impl PlatformTypesRegistryApi for StalledRegistry {
    async fn batch_get_entities(
        &self,
        _ctx: &PlatformSecurityContext,
        _request: BatchGetEntitiesRequest,
    ) -> Result<BatchGetEntitiesResponse, CanonicalError> {
        std::future::pending().await
    }

    async fn list_entities(
        &self,
        _ctx: &PlatformSecurityContext,
        _request: ListEntitiesRequest,
    ) -> Result<ListEntitiesResponse, CanonicalError> {
        std::future::pending().await
    }

    async fn register_entities(
        &self,
        _ctx: &PlatformSecurityContext,
        _key: IdempotencyKey,
        _request: RegisterEntitiesRequest,
    ) -> Result<RegistrationOperation, CanonicalError> {
        std::future::pending().await
    }

    async fn delete_entities(
        &self,
        _ctx: &PlatformSecurityContext,
        _key: IdempotencyKey,
        _request: DeleteEntitiesRequest,
    ) -> Result<DeletionOperation, CanonicalError> {
        std::future::pending().await
    }

    async fn get_operation(
        &self,
        _ctx: &PlatformSecurityContext,
        _operation_id: Uuid,
    ) -> Result<Operation, CanonicalError> {
        std::future::pending().await
    }

    async fn batch_resolve_identities(
        &self,
        _ctx: &PlatformSecurityContext,
        _keys: Vec<EntityKey>,
    ) -> Result<HashMap<EntityKey, Option<EntityIdentity>>, CanonicalError> {
        std::future::pending().await
    }
}
