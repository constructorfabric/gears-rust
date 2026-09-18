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
/// to every lock `name` on the write path, and — on the store-owned-leases path —
/// strips it back off the [`LeaseToken::name`] it returns. A [`LockGuard`] is
/// opaque and needs no read-path strip (DESIGN §3.8 table), but a `LeaseToken`
/// exposes `name` as the *unprefixed* consumer name, so the token path strips on
/// the way out and re-applies on the way back in, the way the cache primitive
/// translates its keys. Scoping composes by stacking wrappers.
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

    /// Acquires under the scoped key, then strips the prefix from the returned
    /// token so the caller sees the *unprefixed* name it asked for
    /// ([`LeaseToken::name`] is the consumer name, not the backend's cache key).
    /// `owner`, `fence` and `deadline` are preserved.
    async fn acquire(
        &self,
        name: &str,
        owner: &str,
        ttl: Duration,
    ) -> Result<LeaseToken, ClusterError> {
        let mut token = self
            .inner
            .acquire(&scope::apply(&self.prefix, name), owner, ttl)
            .await?;
        token.name = scope::strip(&self.prefix, &token.name).to_owned();
        Ok(token)
    }

    /// As [`acquire`](Self::acquire), stripping the prefix off the returned token.
    async fn acquire_waiting(
        &self,
        name: &str,
        owner: &str,
        ttl: Duration,
        timeout: Duration,
    ) -> Result<LeaseToken, ClusterError> {
        let mut token = self
            .inner
            .acquire_waiting(&scope::apply(&self.prefix, name), owner, ttl, timeout)
            .await?;
        token.name = scope::strip(&self.prefix, &token.name).to_owned();
        Ok(token)
    }

    /// Re-applies the prefix to a clone of the caller's token before delegating, so
    /// the scoped key the inner backend recorded on `acquire` is reconstructed. The
    /// caller's token is left unchanged; only `name` is rewritten on the clone.
    async fn renew(&self, token: &LeaseToken, ttl: Duration) -> Result<(), ClusterError> {
        let mut scoped = token.clone();
        scoped.name = scope::apply(&self.prefix, &token.name);
        self.inner.renew(&scoped, ttl).await
    }

    /// Re-applies the prefix on a clone, for the reason [`renew`](Self::renew) gives.
    async fn release(&self, token: &LeaseToken) -> Result<(), ClusterError> {
        let mut scoped = token.clone();
        scoped.name = scope::apply(&self.prefix, &token.name);
        self.inner.release(&scoped).await
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
    use crate::lease::LeaseToken;
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

        // The token half records the scoped name it was handed, exactly as the
        // guard half does, so `acquire_prepends_the_prefix` can assert scoping
        // applies to the store-owned-leases path too.
        async fn acquire(
            &self,
            name: &str,
            owner: &str,
            _ttl: Duration,
        ) -> Result<LeaseToken, ClusterError> {
            self.seen.lock().expect("lock").push(name.to_owned());
            Ok(LeaseToken::new(name, owner, 1))
        }

        async fn acquire_waiting(
            &self,
            name: &str,
            owner: &str,
            ttl: Duration,
            _timeout: Duration,
        ) -> Result<LeaseToken, ClusterError> {
            self.acquire(name, owner, ttl).await
        }

        // renew/release record the token name they were handed, so a test can
        // assert the wrapper re-applied the prefix before delegating.
        async fn renew(&self, token: &LeaseToken, _ttl: Duration) -> Result<(), ClusterError> {
            self.seen.lock().expect("lock").push(token.name.clone());
            Ok(())
        }

        async fn release(&self, token: &LeaseToken) -> Result<(), ClusterError> {
            self.seen.lock().expect("lock").push(token.name.clone());
            Ok(())
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

    /// The token path scopes the same way the guard path does on the *write* side —
    /// the name the backend records is the `prefix/name` the wrapper composed — but,
    /// unlike the opaque guard, the returned [`LeaseToken::name`] is stripped back to
    /// the bare consumer name, because that field is contractually unprefixed.
    #[tokio::test]
    async fn acquire_prepends_the_prefix_and_strips_the_returned_name() {
        let backend = Arc::new(RecordingBackend {
            seen: Mutex::new(Vec::new()),
        });
        let wrapper = scoped(Arc::clone(&backend), "event-broker");
        let token = wrapper
            .acquire("ledger", "owner-a", Duration::from_secs(30))
            .await
            .expect("acquire");
        // The inner backend keyed the lease under the scoped name...
        assert_eq!(
            backend.seen.lock().expect("lock").as_slice(),
            ["event-broker/ledger"]
        );
        // ...but the caller gets the unprefixed name back, with owner/fence intact.
        assert_eq!(token.name, "ledger");
        assert_eq!(token.owner, "owner-a");
        assert_eq!(token.fence, 1);
    }

    /// A token minted by the wrapper (bare name) must round-trip back to the inner
    /// backend under the scoped key on `renew`/`release`, or the caller's token
    /// would fail to resolve to the record `acquire` created.
    #[tokio::test]
    async fn renew_and_release_reapply_the_prefix() {
        let backend = Arc::new(RecordingBackend {
            seen: Mutex::new(Vec::new()),
        });
        let wrapper = scoped(Arc::clone(&backend), "event-broker");
        let token = wrapper
            .acquire("ledger", "owner-a", Duration::from_secs(30))
            .await
            .expect("acquire");
        assert_eq!(token.name, "ledger");

        wrapper
            .renew(&token, Duration::from_secs(30))
            .await
            .expect("renew");
        wrapper.release(&token).await.expect("release");

        // The caller's token is untouched by the round-trip...
        assert_eq!(token.name, "ledger");
        // ...and both delegations reached the inner backend under the scoped key.
        assert_eq!(
            backend.seen.lock().expect("lock").as_slice(),
            [
                "event-broker/ledger",
                "event-broker/ledger",
                "event-broker/ledger"
            ]
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
