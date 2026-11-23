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

    /// Resolve bounded keys to the canonical identity of the registered entity each names,
    /// Active or deleted: its GTS identifier and Registry Reference, with no lifecycle,
    /// validator or document. `None` when nothing is registered under the key. The platform
    /// `resolve_*` helpers go through it.
    ///
    /// Bounded and refused as [`Self::batch_get_entities`] is. A client cache answers the
    /// identities it already knows and sends only the others.
    ///
    /// # Errors
    /// `CanonicalError`; absent keys are answers, not errors. `Internal` when the registry
    /// leaves a key unanswered or answers it with another entity's identity.
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

    /// As [`PlatformTypesRegistryApi::batch_resolve_identities`], under the tenant's read
    /// access. The tenant `resolve_*` helpers go through it.
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
