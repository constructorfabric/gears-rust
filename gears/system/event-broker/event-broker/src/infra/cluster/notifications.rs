//! Event notification realized on top of `ClusterCacheV1::put` +
//! `ClusterCacheV1::watch` (`DESIGN.md:756-758`; there is no separate
//! pub/sub primitive, see `docs/ADR/0007-service-decomposition.md` D3).
//! Shell only - wiring lands with #4345/#4346.

use crate::domain::cluster::EventBrokerCluster;

/// Notifies delivery shards that new events landed on `(topic, partition)`
/// by bumping `evbk.notif:{topic}:{partition}` (`DESIGN.md:756`). The
/// watch event carries no payload - delivery re-queries the backend.
///
/// # Errors
/// Returns [`cluster_sdk::ClusterError`] if the cache write fails, once
/// implemented.
pub async fn notify_partition_appended(
    cluster: &EventBrokerCluster,
    topic: &str,
    partition: i32,
) -> Result<(), cluster_sdk::ClusterError> {
    let _ = (cluster, topic, partition);
    todo!(
        "cache.put(\"evbk.notif:{{topic}}:{{partition}}\", marker, ttl=short) - lands with #4345/#4346"
    )
}
