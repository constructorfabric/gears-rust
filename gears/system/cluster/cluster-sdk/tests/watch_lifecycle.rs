// Created: 2026-06-10 by Constructor Tech
// @cpt-dod:cpt-cf-clst-dod-smoke-tests-watch:p1
//! Contract smoke tests: watch lifecycle across all three watch types
//! (`cpt-cf-clst-dod-smoke-tests-watch`).
//!
//! Each of the cache, leader, and service watches must surface the full
//! watch-union (`Event`/`Lagged`/`Reset`/`Closed`), preserve per-key ordering,
//! and deliver each event at most once. The union signals are injected through
//! the public `*Watch::channel` seam (the same seam every backend drives), and
//! end-to-end per-key ordering is verified through the cache facade over the
//! in-process fixture.

mod common;

use std::sync::Arc;

use cluster_sdk::cache::{
    CacheEvent, CacheWatch, CacheWatchEvent, ClusterCacheBackend, ClusterCacheV1, PutRequest, Ttl,
};
use cluster_sdk::discovery::{
    InstanceState, ServiceInstance, ServiceWatch, ServiceWatchEvent, TopologyChange,
};
use cluster_sdk::error::{ClusterError, ProviderErrorKind};
use cluster_sdk::leader::{LeaderStatus, LeaderWatch, LeaderWatchEvent};
use cluster_sdk::profile::ClusterProfile;
use cluster_sdk::registration::register_cache_backend;
use common::{MemCacheBackend, SmokeProfile};
use std::collections::HashMap;
use std::time::SystemTime;
use toolkit::client_hub::ClientHub;

#[tokio::test]
async fn cache_watch_surfaces_event_lagged_reset_closed_in_order_at_most_once() {
    // @cpt-begin:cpt-cf-clst-flow-cache-primitive-watch-recover:p1:inst-w-subscribe
    let (tx, mut watch) = CacheWatch::channel(8);
    // @cpt-end:cpt-cf-clst-flow-cache-primitive-watch-recover:p1:inst-w-subscribe
    assert!(
        tx.send(CacheWatchEvent::Event(CacheEvent::Changed {
            key: "k".to_owned()
        }))
        .await
        .is_ok()
    );
    assert!(
        tx.send(CacheWatchEvent::Lagged { dropped: 3 })
            .await
            .is_ok()
    );
    assert!(tx.send(CacheWatchEvent::Reset).await.is_ok());
    assert!(
        tx.send(CacheWatchEvent::Closed(ClusterError::Shutdown))
            .await
            .is_ok()
    );

    // @cpt-begin:cpt-cf-clst-flow-cache-primitive-watch-recover:p1:inst-w-next
    // @cpt-begin:cpt-cf-clst-flow-cache-primitive-watch-recover:p1:inst-w-change
    assert!(matches!(
        watch.recv().await,
        Some(CacheWatchEvent::Event(CacheEvent::Changed { key })) if key == "k"
    ));
    // @cpt-end:cpt-cf-clst-flow-cache-primitive-watch-recover:p1:inst-w-change
    // @cpt-end:cpt-cf-clst-flow-cache-primitive-watch-recover:p1:inst-w-next
    // @cpt-begin:cpt-cf-clst-flow-cache-primitive-watch-recover:p1:inst-w-lag
    assert!(matches!(
        watch.recv().await,
        Some(CacheWatchEvent::Lagged { dropped: 3 })
    ));
    assert!(matches!(watch.recv().await, Some(CacheWatchEvent::Reset)));
    // @cpt-end:cpt-cf-clst-flow-cache-primitive-watch-recover:p1:inst-w-lag
    // @cpt-begin:cpt-cf-clst-flow-cache-primitive-watch-recover:p1:inst-w-closed
    assert!(matches!(
        watch.recv().await,
        Some(CacheWatchEvent::Closed(ClusterError::Shutdown))
    ));
    // @cpt-end:cpt-cf-clst-flow-cache-primitive-watch-recover:p1:inst-w-closed
    // Every event was consumed exactly once; the stream then ends.
    // @cpt-begin:cpt-cf-clst-flow-cache-primitive-watch-recover:p1:inst-w-stop
    drop(tx);
    assert!(watch.recv().await.is_none());
    // @cpt-end:cpt-cf-clst-flow-cache-primitive-watch-recover:p1:inst-w-stop
}

#[tokio::test]
async fn leader_watch_surfaces_status_lagged_reset_closed() {
    let (tx, _resign, mut watch) = LeaderWatch::channel(8, LeaderStatus::Follower);
    assert!(tx.send_status(LeaderStatus::Leader).await.is_ok());
    assert!(
        tx.send(LeaderWatchEvent::Lagged { dropped: 1 })
            .await
            .is_ok()
    );
    assert!(tx.send(LeaderWatchEvent::Reset).await.is_ok());
    assert!(
        tx.send(LeaderWatchEvent::Closed(ClusterError::Provider {
            kind: ProviderErrorKind::AuthFailure,
            message: "bad credentials".to_owned(),
        }))
        .await
        .is_ok()
    );

    assert!(matches!(
        watch.changed().await,
        LeaderWatchEvent::Status(LeaderStatus::Leader)
    ));
    assert!(matches!(
        watch.changed().await,
        LeaderWatchEvent::Lagged { dropped: 1 }
    ));
    assert!(matches!(watch.changed().await, LeaderWatchEvent::Reset));
    assert!(matches!(
        watch.changed().await,
        LeaderWatchEvent::Closed(ClusterError::Provider {
            kind: ProviderErrorKind::AuthFailure,
            ..
        })
    ));
    // Each event was delivered exactly once; once the sender drops, the stream
    // stays terminal (a synthesized `Closed(Shutdown)`), never replaying events.
    drop(tx);
    assert!(matches!(
        watch.changed().await,
        LeaderWatchEvent::Closed(ClusterError::Shutdown)
    ));
}

#[tokio::test]
async fn service_watch_surfaces_change_lagged_reset_closed_in_order() {
    // @cpt-begin:cpt-cf-clst-flow-service-discovery-watch:p1:inst-tw-subscribe
    let (tx, mut watch) = ServiceWatch::channel(8);
    // @cpt-end:cpt-cf-clst-flow-service-discovery-watch:p1:inst-tw-subscribe
    assert!(
        tx.send(ServiceWatchEvent::Change(TopologyChange::Joined(instance(
            "i-1"
        ))))
        .await
        .is_ok()
    );
    assert!(
        tx.send(ServiceWatchEvent::Lagged { dropped: 9 })
            .await
            .is_ok()
    );
    assert!(tx.send(ServiceWatchEvent::Reset).await.is_ok());
    assert!(
        tx.send(ServiceWatchEvent::Closed(ClusterError::Shutdown))
            .await
            .is_ok()
    );

    // @cpt-begin:cpt-cf-clst-flow-service-discovery-watch:p1:inst-tw-filter
    assert!(matches!(
        watch.recv().await,
        Some(ServiceWatchEvent::Change(TopologyChange::Joined(i))) if i.instance_id == "i-1"
    ));
    // @cpt-end:cpt-cf-clst-flow-service-discovery-watch:p1:inst-tw-filter
    // @cpt-begin:cpt-cf-clst-flow-service-discovery-watch:p1:inst-tw-lag
    assert!(matches!(
        watch.recv().await,
        Some(ServiceWatchEvent::Lagged { dropped: 9 })
    ));
    assert!(matches!(watch.recv().await, Some(ServiceWatchEvent::Reset)));
    // @cpt-end:cpt-cf-clst-flow-service-discovery-watch:p1:inst-tw-lag
    // @cpt-begin:cpt-cf-clst-flow-service-discovery-watch:p1:inst-tw-closed
    assert!(matches!(
        watch.recv().await,
        Some(ServiceWatchEvent::Closed(ClusterError::Shutdown))
    ));
    // @cpt-end:cpt-cf-clst-flow-service-discovery-watch:p1:inst-tw-closed
    // Each event was delivered exactly once; the stream then ends.
    // @cpt-begin:cpt-cf-clst-flow-service-discovery-watch:p1:inst-tw-stop
    drop(tx);
    assert!(watch.recv().await.is_none());
    // @cpt-end:cpt-cf-clst-flow-service-discovery-watch:p1:inst-tw-stop
}

#[tokio::test]
async fn cache_watch_preserves_per_key_ordering_end_to_end() {
    let hub = ClientHub::new();
    let cache: Arc<dyn ClusterCacheBackend> = MemCacheBackend::linearizable();
    assert!(register_cache_backend(&hub, SmokeProfile::NAME, cache).is_ok());
    let Ok(cache) = ClusterCacheV1::resolver(&hub)
        .profile(SmokeProfile)
        .resolve()
    else {
        panic!("cache must resolve");
    };

    let Ok(mut watch) = cache.watch("k").await else {
        panic!("watch must establish");
    };
    // Drive a deterministic create → update → delete sequence on one key.
    assert!(
        cache
            .put(PutRequest {
                key: "k",
                value: b"v1",
                ttl: Ttl::Indefinite,
            })
            .await
            .is_ok()
    );
    assert!(
        cache
            .put(PutRequest {
                key: "k",
                value: b"v2",
                ttl: Ttl::Indefinite,
            })
            .await
            .is_ok()
    );
    // The key existed, so delete must both succeed and report a removal.
    assert!(
        cache.delete("k").await.expect("delete must succeed"),
        "deleting an existing key must report it was removed"
    );

    assert!(matches!(
        watch.recv().await,
        Some(CacheWatchEvent::Event(CacheEvent::Changed { key })) if key == "k"
    ));
    assert!(matches!(
        watch.recv().await,
        Some(CacheWatchEvent::Event(CacheEvent::Changed { key })) if key == "k"
    ));
    assert!(matches!(
        watch.recv().await,
        Some(CacheWatchEvent::Event(CacheEvent::Deleted { key })) if key == "k"
    ));
}

/// A minimal discoverable instance for the topology-change assertions.
fn instance(id: &str) -> ServiceInstance {
    ServiceInstance {
        instance_id: id.to_owned(),
        address: "10.0.0.1:9000".to_owned(),
        metadata: HashMap::new(),
        state: InstanceState::Enabled,
        registered_at: SystemTime::UNIX_EPOCH,
    }
}
