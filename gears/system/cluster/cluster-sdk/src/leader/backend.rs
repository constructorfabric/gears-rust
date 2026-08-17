// Created: 2026-06-03 by Constructor Tech
//! The pluggable leader-election backend trait every provider implements.

use std::time::Duration;

use async_trait::async_trait;

use crate::assert_dyn_compatible;
use crate::error::ClusterError;
use crate::leader::types::{ElectionConfig, LeaderElectionFeatures};
use crate::leader::watch::LeaderWatch;
use crate::lease::LeaseToken;
use crate::lock::STORE_OWNED_LEASES;

/// The plugin contract a leader-election backend implements.
///
/// The facade holds an `Arc<dyn LeaderElectionBackend>`, so the trait must be
/// dyn-compatible (asserted at the bottom of this module). Every fallible
/// method returns [`ClusterError`].
///
/// # Automatic renewal contract
///
/// `elect` / `elect_with_config` join a named election and return a
/// [`LeaderWatch`]. The backend owns a background renewal task that, per
/// `cpt-cf-clst-algo-leader-election-renewal`:
///
/// - renews the claim on the derived [`ElectionConfig::renewal_interval`];
/// - retries transient backend errors (`ConnectionLost`, `Timeout`,
///   `ResourceExhausted`) **internally**, never surfacing them as transitions;
/// - emits [`LeaderWatchEvent::Status(Lost)`](crate::leader::LeaderWatchEvent::Status)
///   only after renewals fail past `max_missed_renewals`, then auto-reenrolls
///   and resolves to `Leader` or `Follower`;
/// - keeps the cached snapshot coherent with the emitted events by driving both
///   through [`LeaderWatchSender::send_status`](crate::leader::LeaderWatchSender::send_status).
///
/// # Shutdown contract
///
/// On graceful shutdown the backend delivers `Status(Lost)` then a terminal
/// `Closed(ClusterError::Shutdown)` to every active watch, and completes
/// in-flight [`resign`](crate::leader::LeaderWatch::resign) requests on a
/// best-effort basis.
///
/// # Advisory semantics
///
/// Election is advisory coordination — *which* node should run a workload, not
/// mutual exclusion. Backends declaring `linearizable == false` may transiently
/// elect two leaders under partition; consumers needing correctness-critical
/// exclusion combine this with a distributed lock or cache compare-and-swap.
#[async_trait]
pub trait LeaderElectionBackend: Send + Sync {
    /// The backend's native capability flags.
    #[must_use]
    fn features(&self) -> LeaderElectionFeatures;

    /// The concrete provider type name, used for diagnostics — for example the
    /// `provider` field of
    /// [`ClusterError::CapabilityNotMet`](crate::error::ClusterError::CapabilityNotMet).
    ///
    /// The default returns the implementing type's name via
    /// [`std::any::type_name`]. Resolving the name *through the trait object*
    /// this way is deliberate: `std::any::type_name_of_val` applied to a
    /// `&dyn LeaderElectionBackend` only ever yields the trait-object name,
    /// never the concrete backend, because it is monomorphized on the static
    /// type. A provided method is monomorphized per implementer, so this body
    /// reports the real backend through the vtable.
    #[must_use]
    fn provider_name(&self) -> &'static str {
        std::any::type_name::<Self>()
    }

    /// Joins the named election with the default [`ElectionConfig`], returning a
    /// [`LeaderWatch`]. The claim is renewed automatically; the watch
    /// auto-reenrolls on `Status(Lost)`.
    ///
    /// # Errors
    /// Returns [`ClusterError`] if the election cannot be joined.
    async fn elect(&self, name: &str) -> Result<LeaderWatch, ClusterError>;

    /// Joins the named election with custom timing. Identical to [`elect`] but
    /// with the supplied [`ElectionConfig`].
    ///
    /// [`elect`]: LeaderElectionBackend::elect
    ///
    /// # Errors
    /// Returns [`ClusterError`] if the election cannot be joined.
    async fn elect_with_config(
        &self,
        name: &str,
        config: ElectionConfig,
    ) -> Result<LeaderWatch, ClusterError>;

    /// Enrols `owner` in the named election, returning `Some(token)` when this
    /// candidate took the claim and `None` when another candidate holds a live one.
    ///
    /// The lease-token half of the trait, for the same reason as
    /// [`DistributedLockBackend::acquire`](crate::lock::DistributedLockBackend::acquire):
    /// [`elect`](Self::elect) returns a [`LeaderWatch`], which carries no token, so
    /// a gear serving `Join` over the wire needs the token itself (§6.6, §12.6).
    /// A leader claim *is* a lease — the same record, the same fence, the same
    /// steal-on-lapse — which is what lets a leader survive the replica it was
    /// elected through (§5.8.1).
    ///
    /// `None` rather than [`ClusterError::LockContended`] because losing an
    /// election is an ordinary outcome, not an error: the caller becomes a
    /// follower and retries on its own cadence.
    ///
    /// # Errors
    /// - [`ClusterError::Unsupported`] from the default body: a backend that has
    ///   not implemented store-owned leases. Defaulted rather than required so
    ///   adding it does not break every plugin (invariant I11).
    /// - Any other [`ClusterError`] the backend raises.
    async fn join(
        &self,
        name: &str,
        owner: &str,
        config: ElectionConfig,
    ) -> Result<Option<LeaseToken>, ClusterError> {
        let _unused = (name, owner, config);
        Err(ClusterError::Unsupported {
            feature: STORE_OWNED_LEASES,
        })
    }

    /// Extends the claim `token` is authority over to `ttl` from now — **the
    /// operation that holds leadership** (§7.3).
    ///
    /// Renewal is client-driven so that renewal stays the consumer-liveness proxy:
    /// a wedged holder stops renewing and loses its claim (invariant I8). A failed
    /// renewal is therefore a *status change* — the caller emits
    /// [`LeaderStatus::Lost`](crate::leader::LeaderStatus::Lost) and keeps its
    /// subscription open — never a terminal close (§6.6).
    ///
    /// # Errors
    /// - [`ClusterError::LockExpired`] if the predicate matches no record — the
    ///   claim lapsed, was stolen, or was never this owner's. The lock variant is
    ///   reused deliberately: `ClusterError` is frozen (invariant I3) and the
    ///   meaning is identical.
    /// - [`ClusterError::Unsupported`] from the default body, as
    ///   [`join`](Self::join).
    /// - Any other [`ClusterError`] the backend raises.
    async fn renew(&self, token: &LeaseToken, ttl: Duration) -> Result<(), ClusterError> {
        let _unused = (token, ttl);
        Err(ClusterError::Unsupported {
            feature: STORE_OWNED_LEASES,
        })
    }

    /// Gives up the claim `token` is authority over — a conditional delete any
    /// replica can serve.
    ///
    /// **Absence is `Ok`**, as with
    /// [`DistributedLockBackend::release`](crate::lock::DistributedLockBackend::release):
    /// a claim that already lapsed, or was fenced out by a successor, resigns
    /// successfully and leaves the successor's claim untouched.
    ///
    /// # Errors
    /// - [`ClusterError::Unsupported`] from the default body, as
    ///   [`join`](Self::join).
    /// - Any other [`ClusterError`] the backend raises.
    async fn resign(&self, token: &LeaseToken) -> Result<(), ClusterError> {
        let _unused = token;
        Err(ClusterError::Unsupported {
            feature: STORE_OWNED_LEASES,
        })
    }

    /// A cheap, non-mutating liveness check on the backend's own resources.
    ///
    /// The leader-election counterpart of
    /// [`ClusterCacheBackend::probe`](crate::cache::ClusterCacheBackend::probe) —
    /// same contract, same `Ok(())` default, same forwarding obligation on a
    /// delegating backend, and the same reason for existing on this trait: a
    /// natively-bound election backend holds its own resources and is not
    /// reachable through the profile's cache. The SDK default over that cache
    /// leaves this at the default, because the cache is probed directly.
    ///
    /// # Errors
    /// Returns [`ClusterError`] if the backend cannot currently serve.
    async fn probe(&self) -> Result<(), ClusterError> {
        Ok(())
    }
}

assert_dyn_compatible!(LeaderElectionBackend);

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use async_trait::async_trait;

    use super::{LeaderElectionBackend, STORE_OWNED_LEASES};
    use crate::error::ClusterError;
    use crate::leader::types::{ElectionConfig, LeaderElectionFeatures};
    use crate::leader::watch::LeaderWatch;
    use crate::leader::{LeaderStatus, ResignReceiver};
    use crate::lease::LeaseToken;

    /// A backend implementing only the two required methods.
    struct StubBackend;

    #[async_trait]
    impl LeaderElectionBackend for StubBackend {
        fn features(&self) -> LeaderElectionFeatures {
            LeaderElectionFeatures::new(true)
        }

        async fn elect(&self, _name: &str) -> Result<LeaderWatch, ClusterError> {
            let (_sender, _resign, watch): (_, ResignReceiver, _) =
                LeaderWatch::channel(1, LeaderStatus::Follower);
            Ok(watch)
        }

        async fn elect_with_config(
            &self,
            name: &str,
            _config: ElectionConfig,
        ) -> Result<LeaderWatch, ClusterError> {
            self.elect(name).await
        }
    }

    /// The lease methods are **defaulted**, so a backend that implements neither
    /// still compiles — invariant I11.
    #[tokio::test]
    async fn the_lease_methods_are_defaulted_and_report_unsupported() {
        let backend = StubBackend;
        let token = LeaseToken::new("primary", "cand-a", 1);
        assert!(matches!(
            backend
                .join("primary", "cand-a", ElectionConfig::default())
                .await,
            Err(ClusterError::Unsupported {
                feature: STORE_OWNED_LEASES
            })
        ));
        assert!(matches!(
            backend.renew(&token, Duration::from_secs(30)).await,
            Err(ClusterError::Unsupported { .. })
        ));
        assert!(matches!(
            backend.resign(&token).await,
            Err(ClusterError::Unsupported { .. })
        ));
    }

    /// And they stay reachable through the trait object the facade holds.
    #[tokio::test]
    async fn the_lease_methods_are_reachable_through_a_trait_object() {
        let backend: Arc<dyn LeaderElectionBackend> = Arc::new(StubBackend);
        assert!(matches!(
            backend
                .resign(&LeaseToken::new("primary", "cand-a", 1))
                .await,
            Err(ClusterError::Unsupported { .. })
        ));
    }

    /// `probe` is defaulted to `Ok(())` and dyn-reachable, on the same terms as the
    /// lease methods (I11) — see `DistributedLockBackend`'s counterpart for why the
    /// default is `Ok` rather than `Unsupported`.
    #[tokio::test]
    async fn probe_is_defaulted_to_ok_and_dyn_reachable() {
        assert!(StubBackend.probe().await.is_ok());
        let dynamic: Arc<dyn LeaderElectionBackend> = Arc::new(StubBackend);
        assert!(dynamic.probe().await.is_ok());
    }
}
