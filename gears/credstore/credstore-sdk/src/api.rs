use async_trait::async_trait;
use toolkit_security::SecurityContext;

use crate::error::CredStoreError;
use crate::models::{GetSecretResponse, SecretRef, SecretValue, SharingMode, WriteOptions};

/// Consumer-facing API trait for credential storage operations.
#[async_trait]
pub trait CredStoreClientV1: Send + Sync {
    /// Retrieves a secret by reference, applying hierarchical resolution.
    ///
    /// Returns `Ok(Some(_))` with the value and metadata when an accessible
    /// secret is found, `Ok(None)` when none exists or is inaccessible (a
    /// single 404 surface that prevents enumeration), and
    /// `Err(AccessDenied)` only when the caller lacks read permission.
    async fn get(
        &self,
        ctx: &SecurityContext,
        key: &SecretRef,
    ) -> Result<Option<GetSecretResponse>, CredStoreError>;

    /// Stores or updates a secret (upsert) with default [`WriteOptions`]:
    /// the type is preserved on overwrite (`generic` on create) and the expiry
    /// is preserved ([`ExpiryWrite::Preserve`](crate::ExpiryWrite::Preserve)),
    /// so a value rotation never strips an existing expiry. Use
    /// [`Self::put_opts`] to set or clear it explicitly.
    ///
    /// # Concurrency
    ///
    /// Without a precondition this is **last-writer-wins** (mirroring HTTP
    /// `PUT` without `If-Match`): concurrent writers to one reference race,
    /// and the later write replaces the earlier — the intended semantics for
    /// create-or-replace flows (provisioning controllers, rotation) that own
    /// their references, where the create path has no version to send and
    /// "latest value wins" is exactly right. The race is bounded: the value
    /// fingerprint fence (ADR-0003) binds the surviving metadata to the
    /// surviving backend value, so interleaved writers can never produce a
    /// cross-writer value/sharing mismatch. Read-modify-write callers that
    /// must not overwrite a concurrent update should pass a
    /// [`WritePrecondition`](crate::WritePrecondition) via [`Self::put_opts`]
    /// (the in-process `If-Match`) and handle
    /// [`CredStoreError::Conflict`].
    async fn put(
        &self,
        ctx: &SecurityContext,
        key: &SecretRef,
        value: SecretValue,
        sharing: SharingMode,
    ) -> Result<(), CredStoreError> {
        self.put_opts(ctx, key, value, sharing, WriteOptions::default())
            .await
    }

    /// Stores or updates a secret (upsert) with explicit [`WriteOptions`]
    /// (secret type, expiry, optimistic-concurrency precondition). The type is
    /// immutable: an `opts.secret_type` differing from an existing secret's
    /// type is rejected. A failed `opts.precondition`
    /// ([`WritePrecondition`](crate::WritePrecondition)) yields
    /// [`CredStoreError::Conflict`].
    ///
    /// The default implementation reports the operation as unsupported so
    /// value-store test doubles that only override [`Self::put`] stay valid;
    /// real gear clients override it.
    async fn put_opts(
        &self,
        ctx: &SecurityContext,
        key: &SecretRef,
        value: SecretValue,
        sharing: SharingMode,
        opts: WriteOptions,
    ) -> Result<(), CredStoreError> {
        let _ = (ctx, key, value, sharing, opts);
        Err(CredStoreError::internal(
            "put_opts is not supported by this CredStoreClientV1 implementation",
        ))
    }

    /// Creates a secret, failing with [`CredStoreError::Conflict`] if one of the
    /// same sharing class already exists (create-only — the 409 path behind the
    /// REST `POST`). Use [`Self::put`] for upsert semantics.
    async fn create(
        &self,
        ctx: &SecurityContext,
        key: &SecretRef,
        value: SecretValue,
        sharing: SharingMode,
    ) -> Result<(), CredStoreError> {
        self.create_opts(ctx, key, value, sharing, WriteOptions::default())
            .await
    }

    /// Create-only variant of [`Self::put_opts`]. See it for the default
    /// implementation contract.
    async fn create_opts(
        &self,
        ctx: &SecurityContext,
        key: &SecretRef,
        value: SecretValue,
        sharing: SharingMode,
        opts: WriteOptions,
    ) -> Result<(), CredStoreError> {
        let _ = (ctx, key, value, sharing, opts);
        Err(CredStoreError::internal(
            "create_opts is not supported by this CredStoreClientV1 implementation",
        ))
    }

    /// Deletes a secret unconditionally. Convenience for
    /// [`Self::delete_opts`] with no precondition.
    async fn delete(&self, ctx: &SecurityContext, key: &SecretRef) -> Result<(), CredStoreError> {
        self.delete_opts(ctx, key, None).await
    }

    /// Deletes a secret, optionally guarded by an optimistic-concurrency
    /// [`WritePrecondition`](crate::WritePrecondition) (the in-process
    /// equivalent of a REST `If-Match` delete). A failed precondition yields
    /// [`CredStoreError::Conflict`].
    ///
    /// The default implementation reports the operation as unsupported so
    /// value-store test doubles that only override [`Self::delete`] stay valid;
    /// real gear clients override it.
    async fn delete_opts(
        &self,
        ctx: &SecurityContext,
        key: &SecretRef,
        precondition: Option<crate::models::WritePrecondition>,
    ) -> Result<(), CredStoreError> {
        let _ = (ctx, key, precondition);
        Err(CredStoreError::internal(
            "delete_opts is not supported by this CredStoreClientV1 implementation",
        ))
    }
}
