//! `ClusterCapabilities` integration: `DirectoryService` advertise-address
//! resolution. The routing markers the broker keeps in `ClusterCacheV1` are
//! `Storage`'s (`infra::storage::facade`); nothing here watches the cache.

mod advertise_address;

pub(crate) use advertise_address::{AdvertiseAddressResolver, ConfigAdvertiseAddress};
