//! Tenant contract (SPEC §10.1, D17): the entity reads only, `SecurityContext` first.
//! Helpers live in `TypesRegistryApiExt`. No mutation and no operation access: a tenant
//! cannot submit, so it has no operation to read.

use toolkit_canonical_errors::CanonicalError;
use toolkit_security::SecurityContext;

use crate::models::{
    BatchGetEntitiesRequest, BatchGetEntitiesResponse, ListEntitiesRequest, ListEntitiesResponse,
};

/// The tenant-plane Types Registry contract: the platform's reads, with the same models,
/// projection and validators, under a tenant's [`SecurityContext`].
///
/// Every method fails with `CanonicalError`; project it with
/// [`TypesRegistryError::from`](crate::TypesRegistryError) for typed dispatch.
#[toolkit::contract(gear = "types-registry", version = "v1")]
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
}

#[cfg(test)]
#[path = "tenant_contract_tests.rs"]
mod tenant_contract_tests;
