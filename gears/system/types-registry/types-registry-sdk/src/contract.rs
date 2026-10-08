//! Platform contract (SPEC §10.1, D15); helpers live in `PlatformTypesRegistryApiExt`.
//! REST attaches the process token; local contexts stay unvalidated with no principal (C2).
//! Mutations read back accepted operations; failures name `operation_id` for same-key replay (D19).

use toolkit_canonical_errors::CanonicalError;
use toolkit_security::PlatformSecurityContext;
use uuid::Uuid;

use crate::entity_models::{
    BatchGetEntitiesRequest, BatchGetEntitiesResponse, DeleteEntitiesRequest, DeletionOperation,
    IdempotencyKey, ListEntitiesRequest, ListEntitiesResponse, Operation, RegisterEntitiesRequest,
    RegistrationOperation,
};

/// The platform-plane Types Registry contract.
///
/// Every method fails with `CanonicalError`. For typed dispatch, project it with
/// [`TypesRegistryError::from`](crate::TypesRegistryError); an operation item's failure
/// decodes with [`AdmissionFailure::from_canonical`](crate::AdmissionFailure::from_canonical).
#[toolkit::contract(gear = "types-registry", version = "v1")]
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
}

#[cfg(test)]
#[path = "contract_tests.rs"]
mod contract_tests;
