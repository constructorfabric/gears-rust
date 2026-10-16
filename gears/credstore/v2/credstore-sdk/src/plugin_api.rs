// Updated: 2026-10-06 by Constructor Tech
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
//! `delete_key`); destroy is an optional capability, exposed through
//! [`CredStorePluginClientV2::destroyer`] as a [`CredStorePluginDestroyerV2`].
//! The gear never calls `destroy` on a plugin whose `destroyer()` is `None`.
//! It issues `delete_key` and
//! `destroy` as recorded cleanup obligations: right after the commit that made
//! the content dead, or later, when a request that touches the same record
//! finds the obligation still pending. A call may therefore be repeated and
//! may arrive at any later time (hence the idempotency below).
//!
//! Every method returns [`CredStorePluginError`]. The gear lifts it into its
//! own domain error and never hands it to consumers, who see only the
//! canonical error of the consumer trait.
//!
//! Guarantees required of a backend: **durability** (`put` returns only after
//! the bytes are durable), **exact bytes** (`get` returns exactly the bytes of
//! the `put` that returned the version, or `None`), **idempotent
//! `delete_key`/`destroy`**. **Ordered versions per key** (a `put` that starts
//! after another `put` on the same key has returned gets a greater version)
//! are required only together with `destroy`. Not required: CAS, listing,
//! cross-key transactions.

use std::time::Duration;

use async_trait::async_trait;
use thiserror::Error;
use toolkit_security::SecurityContext;

use crate::models::{SecretValue, StoreKey, ValueVersion};

/// Which versions of a key [`CredStorePluginDestroyerV2::destroy`] removes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DestroySelector {
    /// Every version older than the given one (requires ordered versions).
    Below(ValueVersion),
    /// Exactly the given version.
    Exactly(ValueVersion),
}

/// Boxed cause kept for in-process diagnostics.
pub type BoxError = Box<dyn std::error::Error + Send + Sync + 'static>;

/// Failure of a storage-plugin call; every method of
/// [`CredStorePluginClientV2`] returns it.
///
/// The gear lifts it into its own domain error, which maps to the canonical
/// error of the consumer trait; it never reaches consumers. `detail` and
/// `source` stay in-process (logs and diagnostics) and MUST NOT carry secret
/// bytes or the plugin's own credentials: the plugin redacts whatever text
/// the store returned before putting it in either.
///
/// A version that is gone is not an error: [`CredStorePluginClientV2::get`]
/// answers `Ok(None)` for it.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum CredStorePluginError {
    /// The store is temporarily unreachable or refused to serve: a timeout, a
    /// connection reset, a 5xx, a 429. The gear answers 503 with the retry
    /// hint, when there is one.
    #[error("store unavailable: {detail}")]
    Unavailable {
        detail: String,
        /// How long the store asked the caller to wait, when it said so.
        retry_after: Option<Duration>,
        #[source]
        source: Option<BoxError>,
    },

    /// The store refused the plugin's own token or policy (for example a
    /// Vault 403). It is neither a miss nor a denial of the caller: the gear
    /// answers 503 without a retry hint and logs an error for the operator,
    /// never a 403 to the caller.
    #[error("store rejected the plugin's credentials: {detail}")]
    CredentialsRejected {
        detail: String,
        #[source]
        source: Option<BoxError>,
    },

    /// A permanent or unexpected failure: a version that exists but can never
    /// be read, an answer the plugin does not understand. The gear answers
    /// 500.
    #[error("store error: {detail}")]
    Internal {
        detail: String,
        #[source]
        source: Option<BoxError>,
    },
}

impl CredStorePluginError {
    /// A transient failure; `retry_after` is the store's own hint, if any.
    #[must_use]
    pub fn unavailable(detail: impl Into<String>, retry_after: Option<Duration>) -> Self {
        Self::Unavailable {
            detail: detail.into(),
            retry_after,
            source: None,
        }
    }

    /// The store refused the plugin's own credentials.
    #[must_use]
    pub fn credentials_rejected(detail: impl Into<String>) -> Self {
        Self::CredentialsRejected {
            detail: detail.into(),
            source: None,
        }
    }

    /// A permanent or unexpected failure.
    #[must_use]
    pub fn internal(detail: impl Into<String>) -> Self {
        Self::Internal {
            detail: detail.into(),
            source: None,
        }
    }

    /// Attaches the underlying cause (any variant), reachable through
    /// [`std::error::Error::source`]. Redact it first: it MUST NOT carry
    /// secret bytes or the plugin's credentials.
    #[must_use]
    pub fn with_source(mut self, cause: impl Into<BoxError>) -> Self {
        let boxed: BoxError = cause.into();
        match &mut self {
            Self::Unavailable { source, .. }
            | Self::CredentialsRejected { source, .. }
            | Self::Internal { source, .. } => *source = Some(boxed),
        }
        self
    }

    /// The store's retry hint; only [`Self::Unavailable`] carries one.
    #[must_use]
    pub fn retry_after(&self) -> Option<Duration> {
        match self {
            Self::Unavailable { retry_after, .. } => *retry_after,
            _ => None,
        }
    }
}

/// Versioned value store. See the module docs.
#[async_trait]
pub trait CredStorePluginClientV2: Send + Sync {
    /// Durably stores a new immutable version under `key` and returns the
    /// version the provider assigned. Concurrent `put`s to one key are
    /// allowed: each creates its own version.
    ///
    /// Cancelling or timing out a `put` does not undo it: the store may still
    /// apply it. The gear treats an unanswered `put` as possibly applied and
    /// cleans up through its write protocol, so a plugin need not make `put`
    /// cancel-safe.
    async fn put(
        &self,
        ctx: &SecurityContext,
        key: &StoreKey,
        value: SecretValue,
    ) -> Result<ValueVersion, CredStorePluginError>;

    /// Returns exactly the bytes written by the `put` that returned
    /// `version`, or `None` when that version is gone. Never different bytes.
    ///
    /// A version that exists but can never be read (a lost decryption key, an
    /// entry the plugin did not write) is [`CredStorePluginError::Internal`]:
    /// permanent, answered by the gear with a 500 at once, unlike
    /// [`CredStorePluginError::Unavailable`] (transient, answered with a 503
    /// and a retry hint) and unlike `Ok(None)` (the version is gone, which the
    /// gear answers by re-reading the record's pointer once).
    async fn get(
        &self,
        ctx: &SecurityContext,
        key: &StoreKey,
        version: &ValueVersion,
    ) -> Result<Option<SecretValue>, CredStorePluginError>;

    /// Deletes the key: afterwards `get` returns `None` for every version of
    /// it. Idempotent: a key the plugin does not hold is success. Issued by
    /// the gear right after the commit of a record delete, or of a write whose
    /// fresh key can never hold a live value, and again by a later request
    /// that finds that obligation still pending.
    async fn delete_key(
        &self,
        ctx: &SecurityContext,
        key: &StoreKey,
    ) -> Result<(), CredStorePluginError>;

    /// The optional destroy capability. A plugin that can destroy returns
    /// `Some(self)`; the default is `None`, and the gear never calls destroy
    /// on a plugin that returns `None`. Returning `Some` promises ordered
    /// versions per key, see [`CredStorePluginDestroyerV2`].
    fn destroyer(&self) -> Option<&dyn CredStorePluginDestroyerV2> {
        None
    }
}

/// Optional destroy capability. Implementing it promises ordered versions per
/// key: a `put` that starts after another `put` on the same key has returned
/// gets a greater version. [`DestroySelector::Below`] is safe only under that
/// order.
///
/// A plugin that can destroy returns `Some(self)` from
/// [`CredStorePluginClientV2::destroyer`]; the gear never calls `destroy`
/// otherwise. `destroy` is idempotent: versions that are already gone are
/// success.
#[async_trait]
pub trait CredStorePluginDestroyerV2: Send + Sync {
    /// Permanently deletes the selected versions of `key`. Idempotent.
    async fn destroy(
        &self,
        ctx: &SecurityContext,
        key: &StoreKey,
        selector: DestroySelector,
    ) -> Result<(), CredStorePluginError>;
}

#[cfg(test)]
#[path = "plugin_api_tests.rs"]
mod plugin_api_tests;
