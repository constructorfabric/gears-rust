// Created: 2026-06-04 by Constructor Tech
//! The pluggable distributed-lock backend trait every provider implements.

use std::time::Duration;

use async_trait::async_trait;

use crate::assert_dyn_compatible;
use crate::error::ClusterError;
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
}

assert_dyn_compatible!(DistributedLockBackend);

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use async_trait::async_trait;

    use super::DistributedLockBackend;
    use crate::error::ClusterError;
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
}
