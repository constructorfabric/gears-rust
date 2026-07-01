// Created: 2026-06-04 by Constructor Tech
//! The pluggable service-discovery backend trait every provider implements.

use async_trait::async_trait;

use crate::assert_dyn_compatible;
use crate::discovery::handle::ServiceHandle;
use crate::discovery::types::{
    DiscoveryFilter, ServiceDiscoveryFeatures, ServiceInstance, ServiceRegistration,
};
use crate::discovery::watch::ServiceWatch;
use crate::error::ClusterError;

/// The plugin contract a service-discovery backend implements.
///
/// The facade holds an `Arc<dyn ServiceDiscoveryBackend>`, so the trait must be
/// dyn-compatible (asserted at the bottom of this module). Every fallible
/// method returns [`ClusterError`].
///
/// # Heartbeat-renewal contract
///
/// `register` admits an instance with a TTL-bounded heartbeat. The backend owns
/// a background renewal task that, per
/// `cpt-cf-clst-algo-service-discovery-heartbeat`:
///
/// - renews the registration on an interval derived from the TTL
///   (`inst-hb-renew`), keeping the instance discoverable while it heartbeats;
/// - lets the registration **expire** within the TTL once heartbeating stops, so
///   a crashed instance disappears from discovery (`inst-hb-stop`/`inst-hb-expire`).
///
/// The renewal cadence and TTL are backend-owned timing concerns; the SDK is
/// contract-only and runs no timers (it has no `rand`/`uuid`/clock dependency).
///
/// # Instance-id assignment contract
///
/// When [`ServiceRegistration::instance_id`] is `None`, the backend **assigns**
/// an instance id (flow `inst-rg-noid`/`inst-rg-assign`) and surfaces it on the
/// returned [`ServiceHandle::instance_id`] and on every discovered
/// [`ServiceInstance`]. Id generation is a backend concern — the SDK does not
/// generate ids.
///
/// # Default serving intent
///
/// A new registration defaults to
/// [`InstanceState::Enabled`](crate::discovery::InstanceState::Enabled)
/// (`inst-rg-register`). Modules draining before exposure flip to disabled via
/// [`ServiceHandle::set_state`](crate::discovery::ServiceHandle::set_state).
///
/// # Handle command channel
///
/// `register` returns a [`ServiceHandle`] created via
/// [`ServiceHandle::channel`]. The backend owns the paired
/// [`ServiceCommandReceiver`](crate::discovery::ServiceCommandReceiver), selects
/// on its `recv`, and completes each
/// [`ServiceRequest`](crate::discovery::ServiceRequest) through its responder
/// with the real outcome. Dropping the handle without deregistering yields
/// `None` from `recv`; the backend does nothing and the instance lapses via the
/// heartbeat TTL.
///
/// # Discovery and watch semantics
///
/// `discover` returns instances matching the [`DiscoveryFilter`] — the serving
/// state AND every metadata predicate (`cpt-cf-clst-algo-service-discovery-filter`).
/// The order of the returned `Vec` is **unspecified** and may differ across
/// backends and across calls; consumers needing deterministic selection sort
/// client-side, typically by `instance_id`. A backend declaring
/// `features().metadata_pushdown == false` MAY return a superset on metadata and
/// rely on the consumer applying [`DiscoveryFilter::matches`](crate::discovery::DiscoveryFilter::matches)
/// client-side. `watch` yields an **unfiltered** topology stream; consumers
/// filter each change client-side.
#[async_trait]
pub trait ServiceDiscoveryBackend: Send + Sync {
    /// The backend's native capability flags.
    #[must_use]
    fn features(&self) -> ServiceDiscoveryFeatures;

    /// The concrete provider type name, used for diagnostics — for example the
    /// `provider` field of
    /// [`ClusterError::CapabilityNotMet`](crate::error::ClusterError::CapabilityNotMet).
    ///
    /// The default returns the implementing type's name via
    /// [`std::any::type_name`]. Resolving the name *through the trait object*
    /// this way is deliberate: `std::any::type_name_of_val` applied to a
    /// `&dyn ServiceDiscoveryBackend` only ever yields the trait-object name,
    /// never the concrete backend, because it is monomorphized on the static
    /// type. A provided method is monomorphized per implementer, so this body
    /// reports the real backend through the vtable.
    #[must_use]
    fn provider_name(&self) -> &'static str {
        std::any::type_name::<Self>()
    }

    /// Registers an instance and returns its [`ServiceHandle`]. Assigns an
    /// `instance_id` when `reg.instance_id` is `None`; the instance defaults to
    /// enabled with a TTL-bounded heartbeat.
    ///
    /// # Errors
    /// Returns [`ClusterError`] if the registration cannot be admitted.
    async fn register(&self, reg: ServiceRegistration) -> Result<ServiceHandle, ClusterError>;

    /// Returns the instances of `name` matching `filter` (serving state AND every
    /// metadata predicate). The result order is unspecified.
    ///
    /// # Errors
    /// Returns [`ClusterError`] if discovery cannot be performed. An empty match
    /// is `Ok(vec![])`, not an error.
    async fn discover(
        &self,
        name: &str,
        filter: DiscoveryFilter,
    ) -> Result<Vec<ServiceInstance>, ClusterError>;

    /// Subscribes to the unfiltered topology watch for `name`.
    ///
    /// # Errors
    /// Returns [`ClusterError`] if the subscription cannot be established.
    async fn watch(&self, name: &str) -> Result<ServiceWatch, ClusterError>;
}

assert_dyn_compatible!(ServiceDiscoveryBackend);

#[cfg(test)]
#[path = "backend_tests.rs"]
mod backend_tests;
