// Created: 2026-06-03 by Constructor Tech
//! The pluggable leader-election backend trait every provider implements.

use async_trait::async_trait;

use crate::assert_dyn_compatible;
use crate::error::ClusterError;
use crate::leader::types::{ElectionConfig, LeaderElectionFeatures};
use crate::leader::watch::LeaderWatch;

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
}

assert_dyn_compatible!(LeaderElectionBackend);
