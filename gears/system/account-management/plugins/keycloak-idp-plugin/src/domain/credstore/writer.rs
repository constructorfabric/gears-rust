//! Write wrapper over [`credstore_sdk::CredStoreClientV1`].
//!
//! The plugin's system [`SecurityContext`] is baked in at construction so call
//! sites cannot accidentally pass an AM-forwarded ctx to mutation paths
//! (DESIGN §4.7, §8.1). Vendor 404 on `delete` is mapped to `Ok(())` —
//! "already absent" is success-equivalent (DESIGN §4.7 last paragraph).

use std::sync::Arc;

use credstore_sdk::{
    CredStoreClientV1, CredStoreError, SecretRef, SecretValue, SharingMode, WritePrecondition,
};
use toolkit_macros::domain_model;
use toolkit_security::SecurityContext;

/// Ctx-baked-in write wrapper over [`CredStoreClientV1`].
#[domain_model]
#[derive(Clone)]
pub struct CredStoreWriter {
    inner: Arc<dyn CredStoreClientV1>,
    system_ctx: Arc<SecurityContext>,
}

impl CredStoreWriter {
    /// Wrap a [`CredStoreClientV1`] with the plugin-owned system ctx.
    #[must_use]
    pub fn new(inner: Arc<dyn CredStoreClientV1>, system_ctx: Arc<SecurityContext>) -> Self {
        Self { inner, system_ctx }
    }

    /// Create or update one secret.
    ///
    /// # Errors
    ///
    /// Any [`CredStoreError`] surfaced by the underlying client (transport,
    /// auth, validation).
    pub async fn put(
        &self,
        key: &SecretRef,
        value: SecretValue,
        sharing: SharingMode,
    ) -> Result<(), CredStoreError> {
        // `create` is the SDK's only preconditionless write; `Conflict` means
        // the reference already exists (provisioning replay), so overwrite it
        // wholesale — the plugin owns its templated references outright, which
        // is exactly the `WritePrecondition::Exists` last-writer-wins case.
        let replay_value = SecretValue::new(value.as_bytes().to_vec());
        match self
            .inner
            .create(&self.system_ctx, key, value, sharing)
            .await
        {
            Err(CredStoreError::Conflict) => {
                self.inner
                    .put(
                        &self.system_ctx,
                        key,
                        replay_value,
                        sharing,
                        WritePrecondition::Exists,
                    )
                    .await
            }
            other => other,
        }
    }

    /// Delete one plugin-owned secret. Vendor 404
    /// ([`CredStoreError::NotFound`]) is mapped to `Ok(())` — "already
    /// absent" is success-equivalent on the deprovision path (DESIGN §4.7).
    ///
    /// # Errors
    ///
    /// Any non-`NotFound` [`CredStoreError`] surfaced by the underlying
    /// client.
    pub async fn delete(&self, key: &SecretRef) -> Result<(), CredStoreError> {
        match self
            .inner
            .delete(&self.system_ctx, key, WritePrecondition::Exists)
            .await
        {
            Ok(()) | Err(CredStoreError::NotFound) => Ok(()),
            Err(e) => Err(e),
        }
    }
}

#[cfg(test)]
#[path = "writer_tests.rs"]
mod writer_tests;
