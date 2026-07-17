use async_trait::async_trait;
use toolkit_security::SecurityContext;

use crate::error::CredStoreError;
use crate::models::{GetSecretResponse, SecretRef, SecretValue, SharingMode};

/// Consumer-facing API trait for credential storage operations.
///
/// Obtained from `ClientHub` as `Arc<dyn CredStoreClientV1>`. All methods
/// accept a `SecurityContext` from which the gateway derives tenant and
/// owner identity. Access denial is expressed as `Ok(None)` from `get`,
/// not as an error.
#[async_trait]
pub trait CredStoreClientV1: Send + Sync {
    /// Retrieves a secret by reference.
    ///
    /// Returns `Ok(Some(response))` if the secret exists and is accessible,
    /// `Ok(None)` if not found or inaccessible (prevents enumeration),
    /// or `Err` for infrastructure failures.
    ///
    /// The response includes the decrypted value and metadata (owning tenant,
    /// sharing mode, whether the secret was inherited via hierarchical resolution).
    async fn get(
        &self,
        ctx: &SecurityContext,
        key: &SecretRef,
    ) -> Result<Option<GetSecretResponse>, CredStoreError>;

    /// Stores a secret, creating it or overwriting an existing value.
    ///
    /// The owning tenant and (for [`SharingMode::Private`]) the owner are
    /// derived from `ctx`. Backends that only support a static, read-only
    /// catalog return [`CredStoreError::Internal`].
    ///
    /// The default implementation reports the operation as unsupported, so
    /// existing read-only implementors keep compiling without changes.
    ///
    /// # Errors
    ///
    /// Returns `Err` for infrastructure failures or unsupported backends.
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

    /// Deletes a secret. Idempotent: succeeds whether or not the secret
    /// existed (anti-enumeration, consistent with `get`).
    ///
    /// The default implementation reports the operation as unsupported, so
    /// existing read-only implementors keep compiling without changes.
    ///
    /// # Errors
    ///
    /// Returns `Err` for infrastructure failures or unsupported backends.
    async fn delete(&self, _ctx: &SecurityContext, _key: &SecretRef) -> Result<(), CredStoreError> {
        Err(CredStoreError::internal(
            "delete is not supported by this backend",
        ))
    }
}
