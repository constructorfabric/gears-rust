//! Platform and tenant Types Registry contracts.
//!
//! The platform contract supports all operations; the tenant contract supports entity reads.
//! Mutations return operations; retry an accepted request with the same key after a read failure.

use toolkit_canonical_errors::CanonicalError;
use toolkit_security::{PlatformSecurityContext, SecurityContext};
use uuid::Uuid;

use std::collections::HashMap;

use crate::models::{
    BatchGetEntitiesRequest, BatchGetEntitiesResponse, DeleteEntitiesRequest, DeletionOperation,
    EntityIdentity, EntityKey, IdempotencyKey, ListEntitiesRequest, ListEntitiesResponse,
    Operation, RegisterEntitiesRequest, RegistrationOperation,
};

/// The platform-plane Types Registry contract.
///
/// Reads, discovery and identity resolution see global entities only: a tenant-owned
/// key answers `not_found` (or `None`) like an absent one. Work on a tenant's data
/// runs under that tenant's service account through [`TypesRegistryApi`].
///
/// Every method fails with `CanonicalError`. For typed dispatch, project it with
/// [`TypesRegistryError::from`](crate::TypesRegistryError); an operation item's failure
/// decodes with [`AdmissionFailure::from_canonical`](crate::AdmissionFailure::from_canonical).
#[toolkit_contract::contract(gear = "types-registry", version = "v1")]
pub trait PlatformTypesRegistryApi: Send + Sync {
    /// Read bounded keys under one projection.
    ///
    /// # Errors
    /// `CanonicalError`; absent keys are answers, not errors.
    #[idempotency(SafeRead)]
    async fn batch_get_entities(
        &self,
        #[secctx] ctx: &PlatformSecurityContext,
        request: BatchGetEntitiesRequest,
    ) -> Result<BatchGetEntitiesResponse, CanonicalError>;

    /// One bounded discovery page.
    ///
    /// # Errors
    /// A `CanonicalError`, including a refused cursor.
    #[idempotency(SafeRead)]
    async fn list_entities(
        &self,
        #[secctx] ctx: &PlatformSecurityContext,
        request: ListEntitiesRequest,
    ) -> Result<ListEntitiesResponse, CanonicalError>;

    /// Submit registration and read back its operation.
    ///
    /// # Errors
    /// `CanonicalError` for synchronous refusal; after acceptance, read failures name
    /// `operation_id`.
    #[idempotency(IdempotentWrite)]
    async fn register_entities(
        &self,
        #[secctx] ctx: &PlatformSecurityContext,
        key: IdempotencyKey,
        request: RegisterEntitiesRequest,
    ) -> Result<RegistrationOperation, CanonicalError>;

    /// Submits a deletion and returns the operation as read back.
    ///
    /// # Errors
    /// As [`Self::register_entities`].
    #[idempotency(IdempotentWrite)]
    async fn delete_entities(
        &self,
        #[secctx] ctx: &PlatformSecurityContext,
        key: IdempotencyKey,
        request: DeleteEntitiesRequest,
    ) -> Result<DeletionOperation, CanonicalError>;

    /// Reads one operation and its per-candidate outcomes.
    ///
    /// # Errors
    /// `NotFound` for an unknown operation, or another `CanonicalError`.
    #[idempotency(SafeRead)]
    async fn get_operation(
        &self,
        #[secctx] ctx: &PlatformSecurityContext,
        operation_id: Uuid,
    ) -> Result<Operation, CanonicalError>;

    /// Resolve each distinct key to a registered GTS ID and Registry Reference,
    /// including deleted entities, or `None`. Uses [`Self::batch_get_entities`]'s
    /// bounds and key rules. The `resolve_*` helpers use this method.
    ///
    /// # Errors
    /// Returns `Internal` for incomplete or inconsistent replies.
    #[idempotency(SafeRead)]
    async fn batch_resolve_identities(
        &self,
        #[secctx] ctx: &PlatformSecurityContext,
        keys: Vec<EntityKey>,
    ) -> Result<HashMap<EntityKey, Option<EntityIdentity>>, CanonicalError>;
}

/// The tenant-plane Types Registry contract: the platform's reads, with the same models,
/// projection and validators, under a tenant's [`SecurityContext`].
///
/// Pass the host-authenticated caller's context. [`CachedTypesRegistry`](crate::CachedTypesRegistry)
/// keeps visibility confirmations for tenant-owned entities per read context: subject,
/// subject type, home tenant and the raw token scope list; global entities serve every
/// authenticated reader. It trusts the context it is given, so a context reused
/// after the registry refused its bearer can still hit within the freshness window.
/// Anonymous calls bypass the cache. REST requests forward the caller's bearer.
///
/// An implementation lets every authenticated reader see every global entity — `read`
/// and `list` authorization constrains tenant-owned results only — and applies one read
/// gate to batch reads and resolutions; the cache relies on both.
///
/// Every method fails with `CanonicalError`; project it with
/// [`TypesRegistryError::from`](crate::TypesRegistryError) for typed dispatch.
#[toolkit_contract::contract(gear = "types-registry", version = "v1")]
pub trait TypesRegistryApi: Send + Sync {
    /// Read bounded keys under one projection. The single exact read is an extension
    /// helper over it, as on the platform contract.
    ///
    /// # Errors
    /// `CanonicalError`; absent keys are answers, not errors.
    #[idempotency(SafeRead)]
    async fn batch_get_entities(
        &self,
        #[secctx] ctx: &SecurityContext,
        request: BatchGetEntitiesRequest,
    ) -> Result<BatchGetEntitiesResponse, CanonicalError>;

    /// One bounded discovery page.
    ///
    /// # Errors
    /// A `CanonicalError`, including a refused cursor.
    #[idempotency(SafeRead)]
    async fn list_entities(
        &self,
        #[secctx] ctx: &SecurityContext,
        request: ListEntitiesRequest,
    ) -> Result<ListEntitiesResponse, CanonicalError>;

    /// Like [`PlatformTypesRegistryApi::batch_resolve_identities`] under tenant read access.
    ///
    /// # Errors
    /// As [`PlatformTypesRegistryApi::batch_resolve_identities`].
    #[idempotency(SafeRead)]
    async fn batch_resolve_identities(
        &self,
        #[secctx] ctx: &SecurityContext,
        keys: Vec<EntityKey>,
    ) -> Result<HashMap<EntityKey, Option<EntityIdentity>>, CanonicalError>;
}

#[cfg(test)]
#[path = "contract_tests.rs"]
mod contract_tests;
