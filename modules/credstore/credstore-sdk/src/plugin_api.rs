use async_trait::async_trait;
use modkit_security::SecurityContext;

use crate::error::CredStoreError;
use crate::models::{SecretMetadata, SecretRef};

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
}
