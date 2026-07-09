// Created: 2026-06-04 by Constructor Tech
//! The public `DistributedLockV1` facade — a thin, cloneable handle delegating
//! to the resolved `Arc<dyn DistributedLockBackend>`.

use std::sync::Arc;
use std::time::Duration;

use toolkit::client_hub::ClientHub;

use crate::error::ClusterError;
use crate::lock::backend::DistributedLockBackend;
use crate::lock::guard::LockGuard;
use crate::lock::resolver::LockResolverBuilder;
use crate::lock::types::LockFeatures;

/// The public distributed-lock facade. Construct via
/// [`DistributedLockV1::resolver`]; cloning is cheap (an `Arc` bump).
///
/// Provides TTL-bounded mutual exclusion with non-blocking
/// ([`try_lock`](Self::try_lock)) and blocking-with-timeout ([`lock`](Self::lock))
/// acquisition. Each acquisition carries a TTL so a crashed holder cannot block
/// others indefinitely; a returned [`LockGuard`] extends the TTL and releases
/// explicitly.
///
/// # Critical-section rule (ADR-002, DESIGN §2.2/§3.3)
///
/// Code holding a [`LockGuard`] MUST NOT make additional remote I/O calls.
/// Remote effects MUST occur before acquisition or after
/// [`release`](crate::lock::LockGuard::release), never between them. Together
/// with async timeouts this eliminates the stale-writer scenario at the
/// architectural level, which is why the lock exposes **no fencing tokens**.
/// (Enforcement is a separate lock-misuse lint feature; this facade documents
/// the rule.)
///
/// Use [`scoped`](Self::scoped) to carve a composable sub-namespace: every lock
/// `name` is auto-prefixed (DESIGN §3.8).
#[derive(Clone)]
pub struct DistributedLockV1 {
    inner: Arc<dyn DistributedLockBackend>,
}

impl DistributedLockV1 {
    /// Wraps a resolved backend. Crate-internal: consumers obtain a facade
    /// through the resolver.
    pub(crate) fn from_backend(inner: Arc<dyn DistributedLockBackend>) -> Self {
        Self { inner }
    }

    /// Static entry point: returns a fluent resolver bound to `hub`.
    pub fn resolver(hub: &ClientHub) -> LockResolverBuilder<'_> {
        LockResolverBuilder::new(hub)
    }

    /// Returns a sub-namespaced view: every lock `name` is auto-prefixed with
    /// `prefix + "/"` (DESIGN §3.8). Scoping composes.
    ///
    /// # Errors
    /// Returns [`ClusterError::InvalidName`] if `prefix` violates the scope-prefix
    /// rule (`[a-zA-Z0-9_/-]+`).
    pub fn scoped(&self, prefix: &str) -> Result<Self, ClusterError> {
        let prefix = crate::scope::validated_prefix(prefix)?;
        Ok(Self::from_backend(Arc::new(
            crate::lock::ScopedDistributedLockBackend::new(Arc::clone(&self.inner), prefix),
        )))
    }

    /// The bound backend's native capability flags.
    #[must_use]
    pub fn features(&self) -> LockFeatures {
        self.inner.features()
    }

    /// Non-blocking acquisition of `name` with the given `ttl`.
    ///
    /// # Errors
    /// - [`ClusterError::LockContended`] if the lock is already held.
    /// - Propagates any other [`ClusterError`] from the backend.
    pub async fn try_lock(&self, name: &str, ttl: Duration) -> Result<LockGuard, ClusterError> {
        crate::profile::validate_cluster_name(name)?;
        self.inner.try_lock(name, ttl).await
    }

    /// Blocking acquisition of `name` with the given `ttl`, waiting up to
    /// `timeout`.
    ///
    /// # Errors
    /// - [`ClusterError::LockTimeout`] if not acquired within `timeout`.
    /// - Propagates any other [`ClusterError`] from the backend.
    pub async fn lock(
        &self,
        name: &str,
        ttl: Duration,
        timeout: Duration,
    ) -> Result<LockGuard, ClusterError> {
        crate::profile::validate_cluster_name(name)?;
        self.inner.lock(name, ttl, timeout).await
    }
}
