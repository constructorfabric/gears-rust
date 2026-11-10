// Updated: 2026-10-06 by Constructor Tech
//! In-process SDK adapter for the credential store.
//!
//! [`CredStoreLocalClient`] maps the public SDK contract onto the domain
//! [`Service`] and translates domain
//! failures into [`CanonicalError`] once, with the same
//! `From<DomainError> for CanonicalError` mapping the REST layer uses
//! (`infra::sdk_error_mapping`).

use std::sync::Arc;

use async_trait::async_trait;
use credstore_sdk::{
    CredStoreClientV2, Credential, CredentialListItem, CredentialPatch, CredentialWrite,
    PutOutcome, PutPrecondition, Secret, SecretRef, Validator,
};
use toolkit_canonical_errors::CanonicalError;
use toolkit_odata::{ODataQuery, Page};
use toolkit_security::SecurityContext;

use crate::domain::error::DomainError;
use crate::domain::secret::model::{PutPrecondition as DomainPutPrecondition, WritePrecondition};
use crate::domain::secret::service::Service;

/// Map the SDK's `patch`/`delete` optimistic-concurrency precondition onto
/// the domain one. The typed `ClientHub` precondition only expresses the
/// single-generation cases (`Exists` / `Matches`); the multi-validator
/// `AnyVersion` is REST-only.
fn to_domain_precondition(p: credstore_sdk::WritePrecondition) -> WritePrecondition {
    match p {
        credstore_sdk::WritePrecondition::Exists => WritePrecondition::Exists,
        credstore_sdk::WritePrecondition::Matches { id, version } => {
            WritePrecondition::Version { id, version }
        }
    }
}

/// Map the SDK's `put` precondition onto the domain one.
fn to_domain_put_precondition(p: PutPrecondition) -> DomainPutPrecondition {
    match p {
        PutPrecondition::CreateOnly => DomainPutPrecondition::CreateOnly,
        PutPrecondition::Exists => DomainPutPrecondition::Exists,
        PutPrecondition::Matches(Validator { id, version }) => {
            DomainPutPrecondition::Version { id, version }
        }
    }
}

/// In-process [`CredStoreClientV2`] that delegates to the domain [`Service`].
pub struct CredStoreLocalClient {
    svc: Arc<Service>,
}

impl CredStoreLocalClient {
    #[must_use]
    pub fn new(svc: Arc<Service>) -> Self {
        Self { svc }
    }
}

#[async_trait]
impl CredStoreClientV2 for CredStoreLocalClient {
    async fn get_record(
        &self,
        ctx: &SecurityContext,
        key: &SecretRef,
    ) -> Result<Option<Credential>, CanonicalError> {
        self.svc.get_record(ctx, key).await.map_err(Into::into)
    }

    async fn get_secret(
        &self,
        ctx: &SecurityContext,
        key: &SecretRef,
    ) -> Result<Option<Secret>, CanonicalError> {
        match self.svc.get_secret(ctx, key).await {
            // The SDK `get_secret` contract is a single 404 surface:
            // `Ok(None)` covers "does not exist", "inaccessible", and
            // "suppressed" alike. The service's `NotFound` (a resolved row
            // whose backend value is missing even after the read protocol's
            // one re-read of its current `value_version` — ADR-0006) is the
            // same surface, so fold it rather than leak an error the
            // contract does not admit.
            Err(DomainError::NotFound) => Ok(None),
            other => other.map_err(Into::into),
        }
    }

    async fn put(
        &self,
        ctx: &SecurityContext,
        key: &SecretRef,
        write: CredentialWrite,
        precondition: PutPrecondition,
    ) -> Result<PutOutcome, CanonicalError> {
        self.svc
            .put(ctx, key, write, to_domain_put_precondition(precondition))
            .await
            .map_err(Into::into)
    }

    async fn patch(
        &self,
        ctx: &SecurityContext,
        key: &SecretRef,
        patch: CredentialPatch,
        precondition: credstore_sdk::WritePrecondition,
    ) -> Result<Validator, CanonicalError> {
        self.svc
            .patch(ctx, key, patch, to_domain_precondition(precondition))
            .await
            .map_err(Into::into)
    }

    async fn delete(
        &self,
        ctx: &SecurityContext,
        key: &SecretRef,
        precondition: credstore_sdk::WritePrecondition,
    ) -> Result<(), CanonicalError> {
        self.svc
            .delete(ctx, key, to_domain_precondition(precondition))
            .await
            .map_err(Into::into)
    }

    async fn list(
        &self,
        ctx: &SecurityContext,
        query: &ODataQuery,
    ) -> Result<Page<CredentialListItem>, CanonicalError> {
        self.svc.list(ctx, query).await.map_err(Into::into)
    }
}

#[cfg(test)]
#[path = "client_tests.rs"]
mod client_tests;
