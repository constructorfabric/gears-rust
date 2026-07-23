use async_trait::async_trait;
use toolkit_security::SecurityContext;

use crate::error::CredStoreError;
use crate::models::{SecretMetadata, SecretRef, SecretValue, SharingMode};

/// Backend storage adapter trait implemented by credential store plugins.
///
/// Plugins operate at the single-tenant level with explicit parameters
/// decomposed by the gateway. Authorization is the gateway's responsibility.
#[async_trait]
pub trait CredStorePluginClientV1: Send + Sync {
    /// Retrieves a secret with full metadata from the backend.
    async fn get(
        &self,
        ctx: &SecurityContext,
        key: &SecretRef,
    ) -> Result<Option<SecretMetadata>, CredStoreError>;

    /// Stores a secret in the backend, creating or overwriting it.
    ///
    /// The owning tenant and (for [`SharingMode::Private`]) the owner are
    /// derived from `ctx`. Read-only backends return
    /// [`CredStoreError::Internal`].
    ///
    /// The default implementation reports the operation as unsupported, so
    /// existing read-only plugin backends keep compiling without changes.
    ///
    /// # Errors
    ///
    /// Returns `Err` for backend failures or unsupported (read-only) backends.
    async fn put(
        &self,
        _ctx: &SecurityContext,
        _key: &SecretRef,
        _value: SecretValue,
        _sharing: SharingMode,
    ) -> Result<(), CredStoreError> {
        Err(CredStoreError::internal(
            "put is not supported by this backend",
        ))
    }

    /// Deletes a secret from the backend. Idempotent.
    ///
    /// The default implementation reports the operation as unsupported, so
    /// existing read-only plugin backends keep compiling without changes.
    ///
    /// # Errors
    ///
    /// Returns `Err` for backend failures or unsupported (read-only) backends.
    async fn delete(&self, _ctx: &SecurityContext, _key: &SecretRef) -> Result<(), CredStoreError> {
        Err(CredStoreError::internal(
            "delete is not supported by this backend",
        ))
    }
}
