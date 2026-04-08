use async_trait::async_trait;
use modkit_security::SecurityContext;

use crate::error::CredStoreError;
use crate::models::{GetSecretResponse, SecretRef};

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
}
