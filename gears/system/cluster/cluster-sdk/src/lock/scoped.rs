// Created: 2026-06-10 by Constructor Tech
//! The per-primitive scoping wrapper for the distributed lock (DESIGN §3.8).

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;

use crate::error::ClusterError;
use crate::lease::LeaseToken;
use crate::lock::backend::DistributedLockBackend;
use crate::lock::guard::LockGuard;
use crate::lock::types::LockFeatures;
use crate::scope;

/// A delegating [`DistributedLockBackend`] that prepends a validated scope prefix
/// to every lock `name` on the write path. There is no read-path strip: a
/// [`LockGuard`] is opaque to the consumer (DESIGN §3.8 table). Scoping composes
/// by stacking wrappers.
pub struct ScopedDistributedLockBackend {
    inner: Arc<dyn DistributedLockBackend>,
    prefix: String,
}

impl ScopedDistributedLockBackend {
    /// Wraps `inner` with the effective `prefix` (already validated and
    /// separator-terminated by [`scope::validated_prefix`]).
    pub fn new(inner: Arc<dyn DistributedLockBackend>, prefix: String) -> Self {
        Self { inner, prefix }
    }
}

#[async_trait]
impl DistributedLockBackend for ScopedDistributedLockBackend {
    fn features(&self) -> LockFeatures {
        self.inner.features()
    }

    fn provider_name(&self) -> &'static str {
        self.inner.provider_name()
    }

    async fn try_lock(&self, name: &str, ttl: Duration) -> Result<LockGuard, ClusterError> {
        self.inner
            .try_lock(&scope::apply(&self.prefix, name), ttl)
            .await
    }

    async fn lock(
        &self,
        name: &str,
        ttl: Duration,
        timeout: Duration,
    ) -> Result<LockGuard, ClusterError> {
        self.inner
            .lock(&scope::apply(&self.prefix, name), ttl, timeout)
            .await
    }

    async fn acquire(
        &self,
        name: &str,
        owner: &str,
        ttl: Duration,
    ) -> Result<LeaseToken, ClusterError> {
        self.inner
            .acquire(&scope::apply(&self.prefix, name), owner, ttl)
            .await
    }

    async fn acquire_waiting(
        &self,
        name: &str,
        owner: &str,
        ttl: Duration,
        timeout: Duration,
    ) -> Result<LeaseToken, ClusterError> {
        self.inner
            .acquire_waiting(&scope::apply(&self.prefix, name), owner, ttl, timeout)
            .await
    }

    /// Forwarded verbatim, prefix and all: the returned token names the *scoped*
    /// lease, and it is presented back unchanged. Re-applying the prefix here would
    /// double it, and stripping it on the way out would leave the inner backend
    /// unable to find its own record — the same read-path rule the [`LockGuard`]
    /// follows (DESIGN §3.8).
    async fn renew(&self, token: &LeaseToken, ttl: Duration) -> Result<(), ClusterError> {
        self.inner.renew(token, ttl).await
    }

    /// Forwarded verbatim, for the reason [`renew`](Self::renew) gives.
    async fn release(&self, token: &LeaseToken) -> Result<(), ClusterError> {
        self.inner.release(token).await
    }

    /// Forwarded: a probe carries no name to scope, and a scoped view must not
    /// answer the trait's `Ok(())` default over an unreachable backend.
    async fn probe(&self) -> Result<(), ClusterError> {
        self.inner.probe().await
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use async_trait::async_trait;

    use super::ScopedDistributedLockBackend;
    use crate::error::ClusterError;
    use crate::lock::backend::DistributedLockBackend;
    use crate::lock::guard::LockGuard;
    use crate::lock::types::LockFeatures;
    use crate::scope;

    /// Records the lock name the backend was asked to acquire.
    struct RecordingBackend {
        seen: Mutex<Vec<String>>,
    }

    #[async_trait]
    impl DistributedLockBackend for RecordingBackend {
        fn features(&self) -> LockFeatures {
            LockFeatures::new(true)
        }

        async fn try_lock(&self, name: &str, _ttl: Duration) -> Result<LockGuard, ClusterError> {
            self.seen.lock().expect("lock").push(name.to_owned());
            let (_rx, guard) = LockGuard::channel(name.to_owned(), 1);
            Ok(guard)
        }

        async fn lock(
            &self,
            name: &str,
            _ttl: Duration,
            _timeout: Duration,
        ) -> Result<LockGuard, ClusterError> {
            self.try_lock(name, _ttl).await
        }

        async fn probe(&self) -> Result<(), ClusterError> {
            // Recorded under a name no lock could take, so a forwarded probe is
            // distinguishable from the trait's `Ok(())` default.
            self.seen.lock().expect("lock").push("<probe>".to_owned());
            Ok(())
        }
    }

    fn scoped(inner: Arc<RecordingBackend>, prefix: &str) -> ScopedDistributedLockBackend {
        ScopedDistributedLockBackend::new(
            inner,
            scope::validated_prefix(prefix).expect("valid prefix"),
        )
    }

    #[tokio::test]
    async fn try_lock_prepends_the_prefix() {
        let backend = Arc::new(RecordingBackend {
            seen: Mutex::new(Vec::new()),
        });
        let wrapper = scoped(Arc::clone(&backend), "event-broker");
        assert!(
            wrapper
                .try_lock("ledger", Duration::from_secs(30))
                .await
                .is_ok()
        );
        assert_eq!(
            backend.seen.lock().expect("lock").as_slice(),
            ["event-broker/ledger"]
        );
    }

    #[tokio::test]
    async fn scoping_composes_when_nested() {
        let backend = Arc::new(RecordingBackend {
            seen: Mutex::new(Vec::new()),
        });
        let inner = scoped(Arc::clone(&backend), "event-broker");
        let outer = ScopedDistributedLockBackend::new(
            Arc::new(inner),
            scope::validated_prefix("shard-0").expect("valid prefix"),
        );
        assert!(
            outer
                .try_lock("ledger", Duration::from_secs(30))
                .await
                .is_ok()
        );
        assert_eq!(
            backend.seen.lock().expect("lock").as_slice(),
            ["event-broker/shard-0/ledger"]
        );
    }

    /// A probe carries no name to scope, but it must still be forwarded — through
    /// nesting too — or a scoped view answers the trait's `Ok(())` default over an
    /// unreachable backend.
    #[tokio::test]
    async fn probe_is_forwarded_through_every_scoping_layer() {
        let backend = Arc::new(RecordingBackend {
            seen: Mutex::new(Vec::new()),
        });
        let inner = scoped(Arc::clone(&backend), "event-broker");
        let outer = ScopedDistributedLockBackend::new(
            Arc::new(inner),
            scope::validated_prefix("shard-0").expect("valid prefix"),
        );

        assert!(outer.probe().await.is_ok());
        assert_eq!(backend.seen.lock().expect("lock").as_slice(), ["<probe>"]);
    }
}
