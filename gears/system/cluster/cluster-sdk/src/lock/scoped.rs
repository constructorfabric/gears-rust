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
    /// ([`LeaseToken::name`] is the consumer name, not the backend's cache key),
    /// and records this layer's prefix in [`LeaseToken::scope`] so a later
    /// `renew`/`release` can prove the token came from *this* view. `owner`,
    /// `fence` and `deadline` are preserved. A name-bearing error is stripped of
    /// the prefix too (via [`scope::strip_error_name`]) so the error path returns
    /// the bare consumer name like the success path.
    async fn acquire(
        &self,
        name: &str,
        owner: &str,
        ttl: Duration,
    ) -> Result<LeaseToken, ClusterError> {
        let mut token = self
            .inner
            .acquire(&scope::apply(&self.prefix, name), owner, ttl)
            .await
            .map_err(|e| scope::strip_error_name(&self.prefix, e))?;
        token.name = scope::strip(&self.prefix, &token.name).to_owned();
        token.scope.push(self.prefix.clone());
        Ok(token)
    }

    /// As [`acquire`](Self::acquire): strips the prefix off the returned token,
    /// records this layer's scope, and strips name-bearing errors.
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
            .await
            .map_err(|e| scope::strip_error_name(&self.prefix, e))?;
        token.name = scope::strip(&self.prefix, &token.name).to_owned();
        token.scope.push(self.prefix.clone());
        Ok(token)
    }

    /// Verifies the token was minted by *this* view, then re-applies the prefix to a
    /// clone before delegating, so the scoped key the inner backend recorded on
    /// `acquire` is reconstructed. This view's prefix must be the *top* layer of
    /// [`LeaseToken::scope`] (it was pushed last on the way out); a token from a
    /// different scoped view — or a raw-backend token whose scope is empty — fails
    /// with [`ClusterError::InvalidName`]/[`scope::SCOPE_MISMATCH`] rather than being
    /// silently re-prefixed into a phantom key that reports `LockExpired`
    /// ([`scope::reapply_for_token`] holds the shared check). A name-bearing error is
    /// stripped of the prefix too (via [`scope::strip_error_name`]), so the error path
    /// returns the bare consumer name exactly as `acquire` does — and, through a
    /// chain, each layer peels its own prefix. The caller's token is left unchanged.
    async fn renew(&self, token: &LeaseToken, ttl: Duration) -> Result<(), ClusterError> {
        let scoped = scope::reapply_for_token(&self.prefix, token)?;
        self.inner
            .renew(&scoped, ttl)
            .await
            .map_err(|e| scope::strip_error_name(&self.prefix, e))
    }

    /// Verifies-then-strips this view's scope and re-applies the prefix on a clone,
    /// for the reason [`renew`](Self::renew) gives; name-bearing errors are stripped
    /// of the prefix the same way.
    async fn release(&self, token: &LeaseToken) -> Result<(), ClusterError> {
        let scoped = scope::reapply_for_token(&self.prefix, token)?;
        self.inner
            .release(&scoped)
            .await
            .map_err(|e| scope::strip_error_name(&self.prefix, e))
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

    /// The blocking token path must strip the returned name (and record its scope)
    /// exactly as `acquire` does, so its strip cannot silently drift from `acquire`'s
    /// and hand back a still-prefixed token whose next `renew` double-prefixes.
    #[tokio::test]
    async fn acquire_waiting_prepends_the_prefix_and_strips_the_returned_name() {
        let backend = Arc::new(RecordingBackend {
            seen: Mutex::new(Vec::new()),
        });
        let wrapper = scoped(Arc::clone(&backend), "event-broker");
        let token = wrapper
            .acquire_waiting(
                "ledger",
                "owner-a",
                Duration::from_secs(30),
                Duration::from_secs(5),
            )
            .await
            .expect("acquire_waiting");
        // The inner backend keyed the lease under the scoped name...
        assert_eq!(
            backend.seen.lock().expect("lock").as_slice(),
            ["event-broker/ledger"]
        );
        // ...but the caller gets the unprefixed name back, with owner/fence intact...
        assert_eq!(token.name, "ledger");
        assert_eq!(token.owner, "owner-a");
        assert_eq!(token.fence, 1);
        // ...and the token records this view's scope layer, like `acquire` does.
        assert_eq!(token.scope, ["event-broker/"]);
    }

    /// A token minted through a *chain* of scoped wrappers round-trips through the
    /// same chain: acquire records the full effective key, the caller sees the bare
    /// name, and renew/release reconstruct that key layer by layer.
    #[tokio::test]
    async fn a_chained_scope_round_trips_acquire_renew_release() {
        let backend = Arc::new(RecordingBackend {
            seen: Mutex::new(Vec::new()),
        });
        let inner = scoped(Arc::clone(&backend), "a");
        let outer = ScopedDistributedLockBackend::new(
            Arc::new(inner),
            scope::validated_prefix("b").expect("valid prefix"),
        );

        let token = outer
            .acquire("ledger", "owner-a", Duration::from_secs(30))
            .await
            .expect("acquire");
        // The caller sees the bare name; the token carries the full effective scope,
        // and `scope + name` reconstitutes the innermost backend's key.
        assert_eq!(token.name, "ledger");
        assert_eq!(token.scope, ["a/", "b/"]);

        outer
            .renew(&token, Duration::from_secs(30))
            .await
            .expect("renew");
        outer.release(&token).await.expect("release");

        // Every delegation reached the innermost backend under the full chained key.
        assert_eq!(
            backend.seen.lock().expect("lock").as_slice(),
            ["a/b/ledger", "a/b/ledger", "a/b/ledger"]
        );
    }

    /// A raw-backend token (bare name, empty scope) presented to a scoped view fails
    /// loudly with `SCOPE_MISMATCH` instead of being re-prefixed into a phantom key
    /// that reports `LockExpired`; the wrapper rejects before delegating.
    #[tokio::test]
    async fn a_raw_backend_token_is_rejected_by_a_scoped_renew() {
        let backend = Arc::new(RecordingBackend {
            seen: Mutex::new(Vec::new()),
        });
        let wrapper = scoped(Arc::clone(&backend), "event-broker");
        let raw = LeaseToken::new("ledger", "owner-a", 1);
        let err = wrapper
            .renew(&raw, Duration::from_secs(30))
            .await
            .expect_err("a raw token is not from this view");
        assert!(
            matches!(err, ClusterError::InvalidName { reason, .. } if reason == scope::SCOPE_MISMATCH)
        );
        assert!(
            backend.seen.lock().expect("lock").is_empty(),
            "the wrapper must reject before delegating"
        );
    }

    /// A token from a *sibling* scope fails against a differently-scoped view.
    #[tokio::test]
    async fn a_sibling_scope_token_is_rejected_by_a_scoped_release() {
        let backend = Arc::new(RecordingBackend {
            seen: Mutex::new(Vec::new()),
        });
        let team_a = scoped(Arc::clone(&backend), "team-a");
        let team_b = scoped(Arc::clone(&backend), "team-b");
        let token = team_a
            .acquire("ledger", "owner-a", Duration::from_secs(30))
            .await
            .expect("acquire");
        assert_eq!(token.scope, ["team-a/"]);
        let err = team_b
            .release(&token)
            .await
            .expect_err("team-a's token is not team-b's");
        assert!(
            matches!(err, ClusterError::InvalidName { reason, .. } if reason == scope::SCOPE_MISMATCH)
        );
    }

    /// A full-chain token presented to a *sub*-wrapper of that chain fails: the
    /// sub-wrapper's prefix is not the suffix of the accumulated scope.
    #[tokio::test]
    async fn a_full_chain_token_is_rejected_by_a_sub_wrapper_renew() {
        let backend = Arc::new(RecordingBackend {
            seen: Mutex::new(Vec::new()),
        });
        let inner: Arc<dyn DistributedLockBackend> = Arc::new(scoped(Arc::clone(&backend), "a"));
        let outer = ScopedDistributedLockBackend::new(
            Arc::clone(&inner),
            scope::validated_prefix("b").expect("valid prefix"),
        );
        let token = outer
            .acquire("ledger", "owner-a", Duration::from_secs(30))
            .await
            .expect("acquire");
        assert_eq!(token.scope, ["a/", "b/"]);
        // The top scope layer is "b/", not the inner view's "a/", so the exact
        // top-of-stack check rejects it → mismatch, not a phantom key.
        let err = inner
            .renew(&token, Duration::from_secs(30))
            .await
            .expect_err("the full-chain token is not the inner view's");
        assert!(
            matches!(err, ClusterError::InvalidName { reason, .. } if reason == scope::SCOPE_MISMATCH)
        );
    }

    /// The boundary case a flat-string suffix check gets wrong: a token minted under
    /// a *multi-segment* prefix (`eu/team`) whose trailing segment run matches a
    /// *sibling* view's whole prefix (`team`). A `strip_suffix("team/")` on the
    /// concatenated `"eu/team/"` would succeed and re-derive the phantom key
    /// `team/<name>` reporting `LockExpired`; the layered top-of-stack check rejects
    /// it loudly instead. This is the regression guard for that fix.
    #[tokio::test]
    async fn a_multi_segment_token_is_rejected_by_a_sibling_view_sharing_its_tail_segment() {
        let backend = Arc::new(RecordingBackend {
            seen: Mutex::new(Vec::new()),
        });
        // One `.scoped("eu/team")` view (a single multi-segment layer)...
        let eu_team = scoped(Arc::clone(&backend), "eu/team");
        // ...and a sibling `.scoped("team")` view whose prefix is the tail of the above.
        let team = scoped(Arc::clone(&backend), "team");
        let token = eu_team
            .acquire("ledger", "owner-a", Duration::from_secs(30))
            .await
            .expect("acquire");
        assert_eq!(token.scope, ["eu/team/"]);
        let err = team
            .renew(&token, Duration::from_secs(30))
            .await
            .expect_err("eu/team's token is not team's, despite the shared tail segment");
        assert!(
            matches!(err, ClusterError::InvalidName { reason, .. } if reason == scope::SCOPE_MISMATCH),
            "the shared-tail-segment token must be rejected loudly, not accepted into a phantom key"
        );
        assert!(
            backend.seen.lock().expect("lock").len() == 1,
            "only the acquire reached the backend; the mismatched renew rejected before delegating"
        );
    }

    /// An inner backend that always contends, echoing the scoped key it was asked
    /// for, so the wrapper's *error* path can be proven to strip the prefix like the
    /// success path.
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
            _ttl: Duration,
            _timeout: Duration,
        ) -> Result<LockGuard, ClusterError> {
            Err(ClusterError::LockContended {
                name: name.to_owned(),
            })
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
            _owner: &str,
            _ttl: Duration,
            _timeout: Duration,
        ) -> Result<LeaseToken, ClusterError> {
            Err(ClusterError::LockContended {
                name: name.to_owned(),
            })
        }

        async fn renew(&self, _token: &LeaseToken, _ttl: Duration) -> Result<(), ClusterError> {
            Ok(())
        }

        async fn release(&self, _token: &LeaseToken) -> Result<(), ClusterError> {
            Ok(())
        }
    }

    /// On the read path, a name-bearing error from the inner backend must return the
    /// *bare* consumer name, not the internal `event-broker/ledger`, so the error
    /// path is as scope-transparent as the success path.
    #[tokio::test]
    async fn a_contended_inner_backend_reports_the_bare_name() {
        let wrapper = ScopedDistributedLockBackend::new(
            Arc::new(ContendingLockBackend),
            scope::validated_prefix("event-broker").expect("valid prefix"),
        );
        let acquire_err = wrapper
            .acquire("ledger", "owner-a", Duration::from_secs(30))
            .await
            .expect_err("contended");
        assert!(
            matches!(acquire_err, ClusterError::LockContended { name } if name == "ledger"),
            "acquire error must name the bare `ledger`"
        );
        let waiting_err = wrapper
            .acquire_waiting(
                "ledger",
                "owner-a",
                Duration::from_secs(30),
                Duration::from_secs(5),
            )
            .await
            .expect_err("contended");
        assert!(
            matches!(waiting_err, ClusterError::LockContended { name } if name == "ledger"),
            "acquire_waiting error must name the bare `ledger`"
        );
    }

    /// An inner backend that hands out a token on `acquire` but fails `renew`/`release`
    /// with a name-bearing error carrying the scoped key it was asked for, so the
    /// wrapper's *write*-path error stripping (the mirror of the read-path strip) can
    /// be proven.
    struct RenewReleaseFailsBackend;

    #[async_trait]
    impl DistributedLockBackend for RenewReleaseFailsBackend {
        fn features(&self) -> LockFeatures {
            LockFeatures::new(true)
        }

        async fn try_lock(&self, name: &str, _ttl: Duration) -> Result<LockGuard, ClusterError> {
            let (_rx, guard) = LockGuard::channel(name.to_owned(), 1);
            Ok(guard)
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
            owner: &str,
            _ttl: Duration,
        ) -> Result<LeaseToken, ClusterError> {
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

        async fn renew(&self, token: &LeaseToken, _ttl: Duration) -> Result<(), ClusterError> {
            Err(ClusterError::LockExpired {
                name: token.name.clone(),
            })
        }

        async fn release(&self, token: &LeaseToken) -> Result<(), ClusterError> {
            Err(ClusterError::LockExpired {
                name: token.name.clone(),
            })
        }
    }

    /// The write-path counterpart of `a_contended_inner_backend_reports_the_bare_name`:
    /// a name-bearing error from `renew`/`release` must return the *bare* consumer
    /// name, not the internal `event-broker/ledger`, so the error path is as
    /// scope-transparent on the write path as it is on acquire.
    #[tokio::test]
    async fn renew_and_release_strip_the_prefix_from_a_name_bearing_error() {
        let wrapper = ScopedDistributedLockBackend::new(
            Arc::new(RenewReleaseFailsBackend),
            scope::validated_prefix("event-broker").expect("valid prefix"),
        );
        let token = wrapper
            .acquire("ledger", "owner-a", Duration::from_secs(30))
            .await
            .expect("acquire");
        let renew_err = wrapper
            .renew(&token, Duration::from_secs(30))
            .await
            .expect_err("renew fails");
        assert!(
            matches!(renew_err, ClusterError::LockExpired { name } if name == "ledger"),
            "renew error must name the bare `ledger`, not the scoped key"
        );
        let release_err = wrapper.release(&token).await.expect_err("release fails");
        assert!(
            matches!(release_err, ClusterError::LockExpired { name } if name == "ledger"),
            "release error must name the bare `ledger`, not the scoped key"
        );
    }
}
