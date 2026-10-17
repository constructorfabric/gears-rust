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
/// to every lock `name` on the write path. There is no read-path strip on either
/// half: a [`LockGuard`] is opaque to the consumer (DESIGN §3.8 table), and a
/// [`LeaseToken`] carries the *scoped* name the inner backend recorded, so it is
/// presented back verbatim. Scoping composes by stacking wrappers.
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

    /// Acquires under the scoped name. The returned token is the inner backend's,
    /// unchanged: its [`LeaseToken::name`] is the scoped name the lease is stored
    /// under, which is what lets [`renew`](Self::renew) and
    /// [`release`](Self::release) forward it without any prefix arithmetic.
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

    /// As [`acquire`](Self::acquire): scoped on the way in, returned unchanged.
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

    /// Forwarded verbatim, prefix and all: the token names the *scoped* lease, and
    /// it is presented back unchanged. Re-applying the prefix here would double it,
    /// and stripping it on the way out would leave the inner backend unable to find
    /// its own record — the same read-path rule the [`LockGuard`] follows
    /// (DESIGN §3.8). A token therefore addresses the lease it was minted for
    /// whichever view it is presented to, and survives its wire mirror intact
    /// (invariant I7).
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
    use crate::lease::LeaseToken;
    use crate::lock::backend::DistributedLockBackend;
    use crate::lock::guard::LockGuard;
    use crate::lock::types::LockFeatures;
    use crate::scope;

    /// Records every name it was asked to acquire and every token it was handed
    /// back, so a test can assert what reached the innermost backend.
    struct RecordingBackend {
        seen: Mutex<Vec<String>>,
        tokens: Mutex<Vec<LeaseToken>>,
    }

    impl RecordingBackend {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                seen: Mutex::new(Vec::new()),
                tokens: Mutex::new(Vec::new()),
            })
        }
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

        // The token half records the name it was handed, exactly as the guard
        // half does, so scoping can be asserted on the store-owned-leases path too.
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

        // renew/release record the whole token they were handed, so a test can
        // assert the wrapper forwarded it byte-for-byte.
        async fn renew(&self, token: &LeaseToken, _ttl: Duration) -> Result<(), ClusterError> {
            self.tokens.lock().expect("lock").push(token.clone());
            Ok(())
        }

        async fn release(&self, token: &LeaseToken) -> Result<(), ClusterError> {
            self.tokens.lock().expect("lock").push(token.clone());
            Ok(())
        }

        async fn probe(&self) -> Result<(), ClusterError> {
            // Recorded under a name no lock could take, so a forwarded probe is
            // distinguishable from the trait's `Ok(())` default.
            self.seen.lock().expect("lock").push("<probe>".to_owned());
            Ok(())
        }
    }

    fn scoped(
        inner: Arc<dyn DistributedLockBackend>,
        prefix: &str,
    ) -> ScopedDistributedLockBackend {
        ScopedDistributedLockBackend::new(
            inner,
            scope::validated_prefix(prefix).expect("valid prefix"),
        )
    }

    #[tokio::test]
    async fn try_lock_prepends_the_prefix() {
        let backend = RecordingBackend::new();
        let wrapper = scoped(backend.clone(), "event-broker");
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
        let backend = RecordingBackend::new();
        let outer = scoped(Arc::new(scoped(backend.clone(), "event-broker")), "shard-0");
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
        let backend = RecordingBackend::new();
        let outer = scoped(Arc::new(scoped(backend.clone(), "event-broker")), "shard-0");

        assert!(outer.probe().await.is_ok());
        assert_eq!(backend.seen.lock().expect("lock").as_slice(), ["<probe>"]);
    }

    /// Both token-path acquisitions scope the name on the way in and hand back the
    /// inner backend's token unchanged — its `name` is the scoped key the lease is
    /// stored under, exactly as the opaque guard's is.
    #[tokio::test]
    async fn acquire_and_acquire_waiting_prepend_the_prefix_and_return_the_scoped_token() {
        let backend = RecordingBackend::new();
        let wrapper = scoped(backend.clone(), "event-broker");
        let ttl = Duration::from_secs(30);

        let token = wrapper
            .acquire("ledger", "owner-a", ttl)
            .await
            .expect("acquire");
        assert_eq!(token, LeaseToken::new("event-broker/ledger", "owner-a", 1));

        let waited = wrapper
            .acquire_waiting("journal", "owner-a", ttl, Duration::from_secs(5))
            .await
            .expect("acquire_waiting");
        assert_eq!(
            waited,
            LeaseToken::new("event-broker/journal", "owner-a", 1)
        );

        assert_eq!(
            backend.seen.lock().expect("lock").as_slice(),
            ["event-broker/ledger", "event-broker/journal"]
        );
    }

    /// A token minted through a *chain* of scoped wrappers carries the full chained
    /// key, and `renew`/`release` forward it to the innermost backend unchanged —
    /// no layer re-applies (which would double the prefix) or strips anything.
    #[tokio::test]
    async fn a_chained_token_is_forwarded_verbatim_on_renew_and_release() {
        let backend = RecordingBackend::new();
        let outer = scoped(Arc::new(scoped(backend.clone(), "a")), "b");

        let token = outer
            .acquire("ledger", "owner-a", Duration::from_secs(30))
            .await
            .expect("acquire");
        assert_eq!(token.name, "a/b/ledger");

        outer
            .renew(&token, Duration::from_secs(30))
            .await
            .expect("renew");
        outer.release(&token).await.expect("release");

        assert_eq!(
            backend.seen.lock().expect("lock").as_slice(),
            ["a/b/ledger"]
        );
        assert_eq!(
            backend.tokens.lock().expect("lock").as_slice(),
            [token.clone(), token],
            "renew and release must reach the backend with the token byte-for-byte"
        );
    }

    /// The case a strip-and-reapply design gets wrong: a token presented to a
    /// *different* view. Because the token carries its own scoped key, it still
    /// addresses the lease it was minted for — there is no phantom key to derive.
    /// Authority stays with the backend's owner + fence check, not the view.
    #[tokio::test]
    async fn a_token_presented_to_another_view_still_addresses_its_own_lease() {
        let backend = RecordingBackend::new();
        let eu_team = scoped(backend.clone(), "eu/team");
        let team = scoped(backend.clone(), "team");

        let token = eu_team
            .acquire("ledger", "owner-a", Duration::from_secs(30))
            .await
            .expect("acquire");
        team.renew(&token, Duration::from_secs(30))
            .await
            .expect("renew through a sibling view");

        let forwarded = backend.tokens.lock().expect("lock");
        assert_eq!(
            forwarded[0].name, "eu/team/ledger",
            "the sibling view must not re-derive `team/ledger`"
        );
    }

    /// An inner backend that always contends, echoing the scoped key it was asked
    /// for.
    struct ContendingLockBackend;

    #[async_trait]
    impl DistributedLockBackend for ContendingLockBackend {
        fn features(&self) -> LockFeatures {
            LockFeatures::new(true)
        }

        async fn try_lock(&self, name: &str, _ttl: Duration) -> Result<LockGuard, ClusterError> {
            Err(ClusterError::LockContended {
                name: name.to_owned(),
            })
        }

        async fn lock(
            &self,
            name: &str,
            ttl: Duration,
            _timeout: Duration,
        ) -> Result<LockGuard, ClusterError> {
            self.try_lock(name, ttl).await
        }

        async fn acquire(
            &self,
            name: &str,
            _owner: &str,
            _ttl: Duration,
        ) -> Result<LeaseToken, ClusterError> {
            Err(ClusterError::LockContended {
                name: name.to_owned(),
            })
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

        async fn renew(&self, _token: &LeaseToken, _ttl: Duration) -> Result<(), ClusterError> {
            Ok(())
        }

        async fn release(&self, _token: &LeaseToken) -> Result<(), ClusterError> {
            Ok(())
        }
    }

    /// A name-bearing error is forwarded unchanged on both halves, so it names the
    /// same scoped key as the token and the guard: the guard path and the token
    /// path report one name for one lease.
    #[tokio::test]
    async fn a_name_bearing_error_names_the_scoped_key_on_both_halves() {
        let wrapper = scoped(Arc::new(ContendingLockBackend), "event-broker");
        let ttl = Duration::from_secs(30);

        let guard_err = wrapper.try_lock("ledger", ttl).await;
        assert!(
            matches!(guard_err, Err(ClusterError::LockContended { name }) if name == "event-broker/ledger")
        );
        let token_err = wrapper.acquire("ledger", "owner-a", ttl).await;
        assert!(
            matches!(token_err, Err(ClusterError::LockContended { name }) if name == "event-broker/ledger")
        );
    }
}
