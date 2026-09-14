//! Write wrapper over [`credstore_sdk::CredStoreClientV1`].
//!
//! The plugin's system [`SecurityContext`] is baked in at construction so call
//! sites cannot accidentally pass an AM-forwarded ctx to mutation paths
//! (DESIGN "Component Model", "Security and Data Protection"). Vendor 404 on `delete` is mapped to `Ok(())` —
//! "already absent" is success-equivalent (DESIGN "Component Model").

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

    /// Create or replace one secret.
    ///
    /// The credstore contract splits these: `create` is the only
    /// preconditionless write, and `put` never creates — a missing target
    /// fails its precondition with [`CredStoreError::Conflict`]. So
    /// create-or-replace is `create` first, falling back to a
    /// [`WritePrecondition::Exists`] overwrite when the reference already
    /// exists. `Exists` is the documented opt-out for exactly this shape:
    /// a provisioning writer that owns its references and whose new value
    /// is not derived from the stored one, so there is no version to carry.
    /// Concurrent writers to one reference therefore race last-writer-wins,
    /// which is the pre-existing behaviour of this path.
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
        // `SecretValue` is a non-Clone newtype over the raw bytes, so keep a
        // copy to rebuild it for the overwrite leg instead of cloning.
        let bytes = value.as_bytes().to_vec();
        match self
            .inner
            .create(&self.system_ctx, key, value, sharing)
            .await
        {
            Ok(()) => Ok(()),
            // Already present: fall through to the explicit overwrite.
            Err(CredStoreError::Conflict) => {
                self.inner
                    .put(
                        &self.system_ctx,
                        key,
                        SecretValue::new(bytes),
                        sharing,
                        WritePrecondition::Exists,
                    )
                    .await
            }
            Err(e) => Err(e),
        }
    }

    /// Delete one plugin-owned secret. Vendor 404
    /// ([`CredStoreError::NotFound`]) is mapped to `Ok(())` — "already
    /// absent" is success-equivalent on the deprovision path (DESIGN "Component Model").
    ///
    /// # Errors
    ///
    /// Any non-`NotFound` [`CredStoreError`] surfaced by the underlying
    /// client.
    pub async fn delete(&self, key: &SecretRef) -> Result<(), CredStoreError> {
        // `Exists` is the explicit delete-whatever-is-there form; the
        // plugin owns these references and holds no version validator.
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
