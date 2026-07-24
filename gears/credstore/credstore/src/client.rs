use std::sync::Arc;

use async_trait::async_trait;
use credstore_sdk::{
    CredStoreClientV1, CredStoreError, GetSecretResponse, SecretRef, SecretValue, SharingMode,
    WriteOptions,
};
use toolkit_security::SecurityContext;

use crate::domain::error::DomainError;
use crate::domain::secret::model::{WritePrecondition, WriteSpec};
use crate::domain::secret::service::Service;

/// Map the SDK's optimistic-concurrency precondition onto the domain one. The
/// typed `ClientHub` precondition only expresses the single-generation cases
/// (`Exists` / `Matches`); the multi-validator `AnyVersion` is REST-only.
fn to_domain_precondition(p: credstore_sdk::WritePrecondition) -> WritePrecondition {
    match p {
        credstore_sdk::WritePrecondition::Exists => WritePrecondition::Exists,
        credstore_sdk::WritePrecondition::Matches { id, version } => {
            WritePrecondition::Version { id, version }
        }
    }
}

impl From<DomainError> for CredStoreError {
    fn from(err: DomainError) -> Self {
        match err {
            DomainError::NotFound => CredStoreError::NotFound,
            // Both are 409-class; the SDK has no distinct optimistic-lock variant.
            DomainError::Conflict | DomainError::VersionConflict => CredStoreError::Conflict,
            DomainError::InvalidSecretRef { detail } => CredStoreError::invalid_ref(detail),
            DomainError::UnsupportedTransition { detail } => {
                CredStoreError::unsupported_transition(detail)
            }
            DomainError::TypeViolation { reason, detail, .. } => CredStoreError::TypeViolation {
                reason: reason.to_owned(),
                detail,
            },
            DomainError::AccessDenied { .. } => CredStoreError::AccessDenied,
            DomainError::ServiceUnavailable {
                detail,
                retry_after,
                ..
            } => CredStoreError::ServiceUnavailable {
                detail,
                retry_after,
            },
            DomainError::Internal { diagnostic, .. } => CredStoreError::internal(diagnostic),
            // The ClientHub API sends only typed, always-valid preconditions
            // (`Exists`/`Matches`); a malformed one can originate only from
            // REST `If-Match` parsing, so `InvalidPrecondition` crossing the
            // in-process boundary is a gear-internal invariant breach.
            DomainError::InvalidPrecondition { detail } => {
                CredStoreError::internal(format!("invalid precondition: {detail}"))
            }
            #[allow(unreachable_patterns)]
            other => CredStoreError::internal(format!("unmapped DomainError variant: {other}")),
        }
    }
}

/// In-process [`CredStoreClientV1`] that delegates to the domain [`Service`].
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
impl CredStoreClientV1 for CredStoreLocalClient {
    async fn get(
        &self,
        ctx: &SecurityContext,
        key: &SecretRef,
    ) -> Result<Option<GetSecretResponse>, CredStoreError> {
        match self.svc.get(ctx, key).await {
            // The SDK `get` contract is a single 404 surface: `Ok(None)`
            // covers "does not exist" and "inaccessible" alike. The service's
            // `NotFound` (a resolved row whose value is absent, e.g. mid-saga)
            // is the same surface, so fold it rather than leak an error the
            // contract does not admit.
            Err(DomainError::NotFound) => Ok(None),
            other => other.map_err(Into::into),
        }
    }

    async fn put_opts(
        &self,
        ctx: &SecurityContext,
        key: &SecretRef,
        value: SecretValue,
        sharing: SharingMode,
        opts: WriteOptions,
    ) -> Result<(), CredStoreError> {
        let precondition = opts.precondition.map(to_domain_precondition);
        self.svc
            .put(
                ctx,
                key,
                value,
                WriteSpec::upsert(sharing)
                    .with_opts(opts)
                    .with_precondition(precondition),
            )
            .await
            .map_err(Into::into)
    }

    async fn create_opts(
        &self,
        ctx: &SecurityContext,
        key: &SecretRef,
        value: SecretValue,
        sharing: SharingMode,
        opts: WriteOptions,
    ) -> Result<(), CredStoreError> {
        // create-only: Conflict if a secret of this sharing class exists.
        let precondition = opts.precondition.map(to_domain_precondition);
        self.svc
            .put(
                ctx,
                key,
                value,
                WriteSpec::create(sharing)
                    .with_opts(opts)
                    .with_precondition(precondition),
            )
            .await
            .map_err(Into::into)
    }

    async fn delete_opts(
        &self,
        ctx: &SecurityContext,
        key: &SecretRef,
        precondition: Option<credstore_sdk::WritePrecondition>,
    ) -> Result<(), CredStoreError> {
        self.svc
            .delete(ctx, key, precondition.map(to_domain_precondition))
            .await
            .map_err(Into::into)
    }
}

#[cfg(test)]
#[path = "client_tests.rs"]
mod client_tests;
