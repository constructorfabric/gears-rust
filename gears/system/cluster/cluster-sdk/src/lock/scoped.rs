// Created: 2026-06-10 by Constructor Tech
//! The per-primitive scoping wrapper for the distributed lock (DESIGN §3.8).

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;

use crate::error::ClusterError;
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
}
