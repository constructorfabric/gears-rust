//! `ClusterCapabilities` integration: event notification via
//! `ClusterCacheV1::put` + `watch` (`DESIGN.md:604-605,756-758`), and
//! `ServiceDiscoveryV1` advertise-address resolution
//! (`eb-dispatcher-routing`).

mod advertise_address;
pub mod notifications;

pub(crate) use advertise_address::{AdvertiseAddressResolver, ConfigAdvertiseAddress};
