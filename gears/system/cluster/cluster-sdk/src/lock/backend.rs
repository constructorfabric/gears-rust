//! The pluggable distributed-lock backend trait every provider implements.

use std::time::Duration;

use async_trait::async_trait;

use crate::assert_dyn_compatible;
use crate::error::ClusterError;
use crate::lease::LeaseToken;
use crate::lock::guard::LockGuard;
use crate::lock::types::LockFeatures;

/// The plugin contract a distributed-lock backend implements.
///
/// The facade holds an `Arc<dyn DistributedLockBackend>`, so the trait must be
/// dyn-compatible (asserted at the bottom of this module). Every fallible
/// method returns [`ClusterError`].
///
/// # TTL safety-net contract
///
/// Every acquisition carries a consumer-supplied `ttl`. The backend attaches it
/// to the lock entry at acquisition (`cpt-cf-clst-algo-distributed-lock-ttl-safety`,
/// `inst-ts-attach`) and **automatically releases** the entry once the TTL
/// elapses if the holder crashes or never releases (`inst-ts-auto`). This
/// bounds the leak window handed to the next acquirer (`inst-ts-return`) — the
/// safety net that replaces fencing tokens (ADR-002). [`LockGuard::renew`]
/// pushes the deadline out for a longer critical section.
///
/// # Two halves, one lease
///
/// [`try_lock`](DistributedLockBackend::try_lock) / [`lock`](DistributedLockBackend::lock)
/// hand back a [`LockGuard`]; [`acquire`](DistributedLockBackend::acquire) /
/// [`acquire_waiting`](DistributedLockBackend::acquire_waiting) hand back the
/// [`LeaseToken`] the guard cannot carry, and
/// [`renew`](DistributedLockBackend::renew) / [`release`](DistributedLockBackend::release)
/// operate on that token. The second half exists because a remote caller must be
/// able to renew from somewhere other than the acquiring task — and, once the lease
/// is a record in the store, from a *replica that never saw the acquire* (§5.8.1,
/// invariant I7). A backend serves both from one lease rather than two mechanisms;
/// the four lease methods are **required**, so the compiler forces every backend to
/// provide the over-the-wire path. They were once defaulted to
/// [`ClusterError::Unsupported`], which let a native backend ship serving `try_lock`
/// in-process while silently lacking the token path every remote lock RPC takes —
/// a whole provider unusable in Profile 3 with only a per-call opaque error to show
/// for it. Requiring them makes that a build error instead.
///
/// # Release-if-still-holder contract
///
/// Release is conditional (`cpt-cf-clst-algo-distributed-lock-release-if-holder`).
/// On a [`LockRequest::Release`](crate::lock::LockRequest::Release) the backend
/// compares the requester's holder identity against the current entry
/// (`inst-rh-compare`); if the requester is no longer the holder — the TTL
/// lapsed and another participant re-acquired — it returns **without** deleting
/// the foreign holder's entry (`inst-rh-foreign`/`inst-rh-skip`), and otherwise
/// deletes the entry conditionally (`inst-rh-release`). A foreign holder
/// therefore cannot release another's lock.
///
/// # Guard command channel
///
/// `try_lock` / `lock` return a [`LockGuard`] created via
/// [`LockGuard::channel`]. The backend owns the paired
/// [`LockCommandReceiver`](crate::lock::LockCommandReceiver), selects on its
/// `recv`, and completes each [`LockRequest`](crate::lock::LockRequest) through
/// its responder with the real outcome. Dropping the guard without releasing
/// yields `None` from `recv`; the backend does nothing and the entry lapses via
/// TTL.
///
/// # Advisory vs. linearizable semantics
///
/// A backend declaring `linearizable == false` provides only advisory
/// coordination and may transiently grant the same lock to two holders under
/// partition. Consumers needing correctness-grade exclusion require
/// [`LockCapability::Linearizable`](crate::lock::LockCapability::Linearizable)
/// at resolution.
#[async_trait]
pub trait DistributedLockBackend: Send + Sync {
    /// The backend's native capability flags.
    #[must_use]
    fn features(&self) -> LockFeatures;

    /// The concrete provider type name, used for diagnostics — for example the
    /// `provider` field of
    /// [`ClusterError::CapabilityNotMet`](crate::error::ClusterError::CapabilityNotMet).
    ///
    /// The default returns the implementing type's name via
    /// [`std::any::type_name`]. Resolving the name *through the trait object*
    /// this way is deliberate: `std::any::type_name_of_val` applied to a
    /// `&dyn DistributedLockBackend` only ever yields the trait-object name,
    /// never the concrete backend, because it is monomorphized on the static
    /// type. A provided method is monomorphized per implementer, so this body
    /// reports the real backend through the vtable.
    #[must_use]
    fn provider_name(&self) -> &'static str {
        std::any::type_name::<Self>()
    }

    /// Attempts a non-blocking acquisition of `name` with the given `ttl`,
    /// returning a [`LockGuard`] on success.
    ///
    /// # Errors
    /// - [`ClusterError::LockContended`] if the lock is already held
    ///   (`inst-tc-held`/`inst-tc-contended`).
    /// - Any other [`ClusterError`] the backend raises.
    async fn try_lock(&self, name: &str, ttl: Duration) -> Result<LockGuard, ClusterError>;

    /// Attempts a blocking acquisition of `name` with the given `ttl`, waiting
    /// up to `timeout`, returning a [`LockGuard`] on success.
    ///
    /// # Errors
    /// - [`ClusterError::LockTimeout`] (reporting `waited`) if the lock is not
    ///   acquired within `timeout` (`inst-wt-timeout`/`inst-wt-timeout-return`).
    /// - Any other [`ClusterError`] the backend raises.
    async fn lock(
        &self,
        name: &str,
        ttl: Duration,
        timeout: Duration,
    ) -> Result<LockGuard, ClusterError>;

    /// Acquires `name` for `owner` without blocking, returning the
    /// [`LeaseToken`] that is the whole authority over the lease.
    ///
    /// The lease-token half of the trait, and the one a **remote** caller uses:
    /// [`try_lock`](Self::try_lock) hands back a [`LockGuard`] whose private
    /// fields cannot carry a token, so a gear serving `TryLock` over the wire —
    /// or any caller that must `renew` from somewhere other than the acquiring
    /// task — needs the token itself (§6.5, §12.6). In-process the guard is still
    /// the ergonomic path, and the cache-backed default builds it *over* this
    /// method so both paths share one lease.
    ///
    /// Acquisition is insert-or-steal-if-lapsed: a record whose `deadline` has
    /// passed is taken over by CAS with `fence + 1`, so the previous holder's
    /// token can never match again (§5.8.1).
    ///
    /// # Errors
    /// - [`ClusterError::LockContended`] if a live lease is held — by anyone,
    ///   including `owner` itself.
    /// - Any other [`ClusterError`] the backend raises.
    async fn acquire(
        &self,
        name: &str,
        owner: &str,
        ttl: Duration,
    ) -> Result<LeaseToken, ClusterError>;

    /// Acquires `name` for `owner`, waiting up to `timeout` — the lease-token
    /// counterpart of [`lock`](Self::lock).
    ///
    /// # Errors
    /// - [`ClusterError::LockTimeout`] (reporting `waited`) if the lease is not
    ///   acquired within `timeout`.
    /// - Any other [`ClusterError`] the backend raises.
    async fn acquire_waiting(
        &self,
        name: &str,
        owner: &str,
        ttl: Duration,
        timeout: Duration,
    ) -> Result<LeaseToken, ClusterError>;

    /// Extends the lease `token` is authority over to `ttl` from now.
    ///
    /// One conditional write predicated on `(name, owner, fence, deadline > now)`.
    /// Nothing matched means expired, stolen, or never-yours — indistinguishable
    /// and all [`ClusterError::LockExpired`] (§6.9) — and because the predicate is
    /// entirely over stored state, **any replica gives the same answer** (I7).
    ///
    /// The lease is **reset** to `ttl` from now, not extended by it, matching
    /// [`LockGuard::renew`]'s existing contract.
    ///
    /// A caller's authority is the token. Cross-checking that the *transport*
    /// caller is `token.owner` is the serving gear's authorization decision
    /// (§4.6), not the backend's predicate.
    ///
    /// # Errors
    /// - [`ClusterError::LockExpired`] if the predicate matches no record.
    /// - Any other [`ClusterError`] the backend raises.
    async fn renew(&self, token: &LeaseToken, ttl: Duration) -> Result<(), ClusterError>;

    /// Releases the lease `token` is authority over.
    ///
    /// The same predicate, deleting instead of updating. **Absence is `Ok`**
    /// (idempotent by absence, §6.10): a retried release, or one bearing a
    /// fenced-out token, deletes nothing and succeeds — never `LockExpired`, never
    /// a not-found. A record the token does not match is another holder's and is
    /// left untouched.
    ///
    /// # Errors
    /// - Any other [`ClusterError`] the backend raises. Note that *nothing to
    ///   release* is not one of them.
    async fn release(&self, token: &LeaseToken) -> Result<(), ClusterError>;

    /// A cheap, non-mutating liveness check on the backend's own resources.
    ///
    /// The lock counterpart of
    /// [`ClusterCacheBackend::probe`](crate::cache::ClusterCacheBackend::probe) —
    /// same contract, same `Ok(())` default, same forwarding obligation on a
    /// delegating backend. Read the cache trait's method for the full rationale.
    ///
    /// It exists on this trait because a natively-bound lock is **not** reachable
    /// through the profile's cache. The Postgres lock opens its own pool and is
    /// "always standalone, never shared with a co-located cache pool", so a
    /// profile pairing a healthy cache with an unreachable lock database would
    /// otherwise report `Serving` (DESIGN.md's `Ready` row is
    /// over "every backend instance's probe").
    ///
    /// A lock backend the wiring auto-filled over the profile's cache — the SDK
    /// default — deliberately leaves this at the default: its store *is* that
    /// cache, which the composite healthcheck probes directly, so overriding it
    /// would only probe the same instance twice.
    ///
    /// # Errors
    /// Returns [`ClusterError`] if the backend cannot currently serve.
    async fn probe(&self) -> Result<(), ClusterError> {
        Ok(())
    }
}

/// The canonical feature name for the store-owned-leases model, shared by the
/// lock and [`LeaderElectionBackend`] primitives.
///
/// No default body returns it any more: the four lock lease methods and the three
/// leader lease methods are all **required**, so a backend can no longer compile
/// while silently lacking the model. Retained as the one spelling of the feature
/// name for any diagnostic that still needs to name it.
///
/// [`LeaderElectionBackend`]: crate::leader::LeaderElectionBackend
pub const STORE_OWNED_LEASES: &str = "store-owned-leases";

assert_dyn_compatible!(DistributedLockBackend);

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use async_trait::async_trait;

    use super::DistributedLockBackend;
    use crate::error::ClusterError;
    use crate::lease::LeaseToken;
    use crate::lock::guard::LockGuard;
    use crate::lock::types::LockFeatures;

    /// A stub backend: the named lock is "held" iff `held` is set.
    struct StubBackend {
        held: bool,
    }

    #[async_trait]
    impl DistributedLockBackend for StubBackend {
        fn features(&self) -> LockFeatures {
            LockFeatures::new(true)
        }

        async fn try_lock(&self, name: &str, _ttl: Duration) -> Result<LockGuard, ClusterError> {
            if self.held {
                return Err(ClusterError::LockContended {
                    name: name.to_owned(),
                });
            }
            let (_rx, guard) = LockGuard::channel(name.to_owned(), 1);
            Ok(guard)
        }

        async fn lock(
            &self,
            name: &str,
            _ttl: Duration,
            timeout: Duration,
        ) -> Result<LockGuard, ClusterError> {
            if self.held {
                return Err(ClusterError::LockTimeout {
                    name: name.to_owned(),
                    waited: timeout,
                });
            }
            let (_rx, guard) = LockGuard::channel(name.to_owned(), 1);
            Ok(guard)
        }

        // The token half, now that it is required rather than defaulted: the stub
        // mints a lease when the name is free and mirrors the guard path's
        // contention answer otherwise, so the same `held` flag drives both halves.
        async fn acquire(
            &self,
            name: &str,
            owner: &str,
            _ttl: Duration,
        ) -> Result<LeaseToken, ClusterError> {
            if self.held {
                return Err(ClusterError::LockContended {
                    name: name.to_owned(),
                });
            }
            Ok(LeaseToken::new(name, owner, 1))
        }

        async fn acquire_waiting(
            &self,
            name: &str,
            owner: &str,
            _ttl: Duration,
            timeout: Duration,
        ) -> Result<LeaseToken, ClusterError> {
            if self.held {
                return Err(ClusterError::LockTimeout {
                    name: name.to_owned(),
                    waited: timeout,
                });
            }
            Ok(LeaseToken::new(name, owner, 1))
        }

        async fn renew(&self, _token: &LeaseToken, _ttl: Duration) -> Result<(), ClusterError> {
            Ok(())
        }

        async fn release(&self, _token: &LeaseToken) -> Result<(), ClusterError> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn try_lock_contends_when_held() {
        let backend = StubBackend { held: true };
        assert!(matches!(
            backend.try_lock("ledger", Duration::from_secs(30)).await,
            Err(ClusterError::LockContended { name }) if name == "ledger"
        ));
    }

    #[tokio::test]
    async fn try_lock_acquires_when_free() {
        let backend = StubBackend { held: false };
        let Ok(guard) = backend.try_lock("ledger", Duration::from_secs(30)).await else {
            panic!("a free lock must be acquired");
        };
        assert_eq!(guard.name(), "ledger");
    }

    #[tokio::test]
    async fn lock_times_out_when_held() {
        let backend = StubBackend { held: true };
        assert!(matches!(
            backend
                .lock("ledger", Duration::from_secs(30), Duration::from_millis(100))
                .await,
            Err(ClusterError::LockTimeout { name, waited })
                if name == "ledger" && waited == Duration::from_millis(100)
        ));
    }

    #[test]
    fn provider_name_reports_concrete_backend() {
        let backend = StubBackend { held: false };
        assert!(backend.provider_name().contains("StubBackend"));
    }

    /// The four lease methods are **required**, not defaulted, so a backend that
    /// serves the guard path must serve the token path too — the store-owned-leases
    /// half the gear serves every remote lock RPC through. The `held` flag drives
    /// both token-path acquisitions through their real branch (acquired vs
    /// contended/timed-out), so the assertions exercise stub behaviour rather than
    /// echoing the stub's own input; `renew`/`release` are checked for reachability.
    #[tokio::test]
    async fn the_lease_methods_are_implemented_not_defaulted() {
        let ttl = Duration::from_secs(30);
        let free = StubBackend { held: false };
        let token = free
            .acquire("ledger", "owner-a", ttl)
            .await
            .expect("a free name is acquired on the token path");
        free.renew(&token, ttl)
            .await
            .expect("renew is reachable on the held token");
        free.release(&token)
            .await
            .expect("release is reachable on the held token");

        // A held name drives both blocking and non-blocking token-path acquires
        // through their contended branch, exactly as the `held` flag does on the
        // guard path — a real outcome, not the stub echoing its input.
        let held = StubBackend { held: true };
        assert!(matches!(
            held.acquire("ledger", "owner-a", ttl).await,
            Err(ClusterError::LockContended { name }) if name == "ledger"
        ));
        assert!(matches!(
            held.acquire_waiting("ledger", "owner-a", ttl, ttl).await,
            Err(ClusterError::LockTimeout { name, .. }) if name == "ledger"
        ));
    }

    /// And they are reachable through the trait object every facade holds — the
    /// required methods must not have made the trait dyn-incompatible.
    #[tokio::test]
    async fn the_lease_methods_are_reachable_through_a_trait_object() {
        let backend: Arc<dyn DistributedLockBackend> = Arc::new(StubBackend { held: false });
        assert!(
            backend
                .acquire("ledger", "owner-a", Duration::from_secs(30))
                .await
                .is_ok()
        );
    }

    /// `probe` is defaulted on the same terms (I11), and its default is `Ok(())`
    /// rather than `Unsupported`: a backend that cannot be probed must not read as
    /// a backend that is failing, or every plugin without an implementation would
    /// take its profile out of rotation.
    #[tokio::test]
    async fn probe_is_defaulted_to_ok_and_dyn_reachable() {
        let backend = StubBackend { held: false };
        assert!(backend.probe().await.is_ok());

        let dynamic: Arc<dyn DistributedLockBackend> = Arc::new(StubBackend { held: false });
        assert!(dynamic.probe().await.is_ok());
    }
}
