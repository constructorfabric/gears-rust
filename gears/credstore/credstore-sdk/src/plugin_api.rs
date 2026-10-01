//! Backend storage-plugin contract (ADR-0006: immutable value versions).
//!
//! A plugin is a versioned key-value store keyed by `(tenant_id, record_id)`
//! ([`StoreKey`]); the gear chooses the key and the plugin only maps it to a
//! physical location under its installation prefix. Every `put` creates a new
//! immutable version and returns the provider's identifier of it
//! ([`ValueVersion`]); the gear stores that in the metadata row and passes it
//! back verbatim. The plugin learns nothing about references, owners, types or
//! sharing, and the request context is used for correlation only, never for
//! authorization.
//!
//! Three operations are required of every backend (`put`, `get`,
//! `delete_key`); `destroy` is optional and declared through
//! [`CredStorePluginClientV2::supports_destroy`]. The gear never calls
//! `destroy` on a plugin that does not declare it.
//!
//! Guarantees required of a backend: **durability** (`put` returns only after
//! the bytes are durable), **exact bytes** (`get` returns exactly the bytes of
//! the `put` that returned the version, or `None`), **idempotent
//! `delete_key`/`destroy`**. **Ordered versions per key** (a `put` that starts
//! after another `put` on the same key has returned gets a greater version)
//! are required only together with `destroy`. Not required: CAS, listing,
//! cross-key transactions.

use async_trait::async_trait;
use toolkit_security::SecurityContext;

use crate::error::CredStoreError;
use crate::models::{SecretValue, StoreKey, ValueVersion};

/// Which versions of a key [`CredStorePluginClientV2::destroy`] removes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DestroySelector {
    /// Every version older than the given one (requires ordered versions).
    Below(ValueVersion),
    /// Exactly the given version.
    Exactly(ValueVersion),
}

/// Versioned value store. See the module docs.
#[async_trait]
pub trait CredStorePluginClientV2: Send + Sync {
    /// Durably stores a new immutable version under `key` and returns the
    /// version the provider assigned. Concurrent `put`s to one key are
    /// allowed: each creates its own version.
    async fn put(
        &self,
        ctx: &SecurityContext,
        key: &StoreKey,
        value: SecretValue,
    ) -> Result<ValueVersion, CredStoreError>;

    /// Returns exactly the bytes written by the `put` that returned
    /// `version`, or `None` when that version is gone. Never different bytes.
    async fn get(
        &self,
        ctx: &SecurityContext,
        key: &StoreKey,
        version: &ValueVersion,
    ) -> Result<Option<SecretValue>, CredStoreError>;

    /// Deletes the key with all its versions. Idempotent: a key the plugin
    /// does not hold is success. Issued only by the outbox handler after a
    /// record delete.
    async fn delete_key(&self, ctx: &SecurityContext, key: &StoreKey)
    -> Result<(), CredStoreError>;

    /// Whether this plugin implements [`Self::destroy`] (and provides ordered
    /// versions). Defaults to `false`.
    fn supports_destroy(&self) -> bool {
        false
    }

    /// Permanently deletes the selected versions of `key`. Idempotent.
    /// Optional: the default reports the operation as unsupported, and the
    /// gear never calls it unless [`Self::supports_destroy`] is `true`.
    async fn destroy(
        &self,
        _ctx: &SecurityContext,
        _key: &StoreKey,
        _selector: DestroySelector,
    ) -> Result<(), CredStoreError> {
        Err(CredStoreError::internal(
            "destroy is not supported by this plugin",
        ))
    }
}
