// Created: 2026-06-11 by Constructor Tech
use std::sync::Arc;

use std::time::Duration;

use cluster_sdk::{
    CacheCapability, CacheWatchEvent, ClusterCacheV1, ClusterError, ClusterProfile,
    DistributedLockV1, LeaderElectionV1, LeaderStatus, LeaderWatchEvent, ServiceDiscoveryV1,
    ServiceWatchEvent,
};
use standalone_cluster_plugin::StandaloneClusterPlugin;
use toolkit::client_hub::ClientHub;

use super::{ClusterWiring, ProfileBackends};

#[derive(Clone, Copy)]
struct EventBroker;
impl ClusterProfile for EventBroker {
    const NAME: &'static str = "event-broker";
}

#[tokio::test]
async fn omit_default_registers_all_four_then_stop_unbinds() {
    let hub = Arc::new(ClientHub::new());
    let plugin = StandaloneClusterPlugin::builder()
        .build_and_start()
        .expect("plugin starts");
    let cache = plugin.cache();

    // Bind only the cache; the wiring auto-fills the other three with SDK defaults.
    let handle = ClusterWiring::builder(Arc::clone(&hub))
        .profile(EventBroker, ProfileBackends::new(cache))
        .on_stop(move || async move { plugin.stop().await })
        .build_and_start()
        .expect("wiring starts");

    assert!(
        ClusterCacheV1::resolver(&hub)
            .profile(EventBroker)
            .require(CacheCapability::Linearizable)
            .resolve()
            .is_ok(),
        "the bound linearizable cache resolves"
    );
    assert!(
        LeaderElectionV1::resolver(&hub)
            .profile(EventBroker)
            .resolve()
            .is_ok(),
        "omit-default leader election resolves"
    );
    assert!(
        DistributedLockV1::resolver(&hub)
            .profile(EventBroker)
            .resolve()
            .is_ok(),
        "omit-default lock resolves"
    );
    assert!(
        ServiceDiscoveryV1::resolver(&hub)
            .profile(EventBroker)
            .resolve()
            .is_ok(),
        "omit-default service discovery resolves"
    );

    handle.stop().await;

    // Deregistration leaves the profile unbound.
    assert!(matches!(
        ClusterCacheV1::resolver(&hub)
            .profile(EventBroker)
            .resolve(),
        Err(ClusterError::ProfileNotBound { .. })
    ));
    assert!(matches!(
        LeaderElectionV1::resolver(&hub)
            .profile(EventBroker)
            .resolve(),
        Err(ClusterError::ProfileNotBound { .. })
    ));
}

#[tokio::test]
async fn stop_revokes_an_active_leader_before_shutdown_completes() {
    let hub = Arc::new(ClientHub::new());
    let plugin = StandaloneClusterPlugin::builder()
        .build_and_start()
        .expect("plugin starts");
    let cache = plugin.cache();

    let handle = ClusterWiring::builder(Arc::clone(&hub))
        .profile(EventBroker, ProfileBackends::new(cache))
        .on_stop(move || async move { plugin.stop().await })
        .build_and_start()
        .expect("wiring starts");

    // A consumer wins the omit-default (CAS-based) election.
    let leader = LeaderElectionV1::resolver(&hub)
        .profile(EventBroker)
        .resolve()
        .expect("leader election resolves");
    let mut watch = leader.elect("primary").await.expect("election joins");
    assert!(matches!(
        watch.changed().await,
        LeaderWatchEvent::Status(LeaderStatus::Leader)
    ));

    // Graceful shutdown must revoke leadership before it completes.
    handle.stop().await;

    // The former leader observes loss, then a terminal shutdown close, and its
    // synchronous snapshot no longer claims leadership.
    assert!(matches!(
        watch.changed().await,
        LeaderWatchEvent::Status(LeaderStatus::Lost)
    ));
    assert!(matches!(
        watch.changed().await,
        LeaderWatchEvent::Closed(ClusterError::Shutdown)
    ));
    assert!(!watch.is_leader());
}

#[tokio::test]
async fn stop_revokes_active_lock_sd_and_cache_watches_before_shutdown_completes() {
    let hub = Arc::new(ClientHub::new());
    let plugin = StandaloneClusterPlugin::builder()
        .build_and_start()
        .expect("plugin starts");
    let cache_backend = plugin.cache();

    let handle = ClusterWiring::builder(Arc::clone(&hub))
        .profile(EventBroker, ProfileBackends::new(plugin.cache()))
        .on_stop(move || async move { plugin.stop().await })
        .build_and_start()
        .expect("wiring starts");

    // A blocking lock waiter that must keep waiting (the lock is held).
    let lock = DistributedLockV1::resolver(&hub)
        .profile(EventBroker)
        .resolve()
        .expect("lock resolves");
    let _held = lock
        .try_lock("ledger", Duration::from_secs(100))
        .await
        .expect("first holder acquires");
    let lock_waiter = lock.clone();
    let waiter = tokio::spawn(async move {
        lock_waiter
            .lock("ledger", Duration::from_secs(100), Duration::from_secs(100))
            .await
    });

    // An active service-discovery watch.
    let discovery = ServiceDiscoveryV1::resolver(&hub)
        .profile(EventBroker)
        .resolve()
        .expect("service discovery resolves");
    let mut sd_watch = discovery
        .watch("delivery")
        .await
        .expect("sd watch establishes");

    // An active cache watch.
    let mut cache_watch = cache_backend
        .watch("k")
        .await
        .expect("cache watch establishes");

    // Let the lock waiter and translator tasks reach their wait points.
    for _ in 0..16 {
        tokio::task::yield_now().await;
    }

    // Graceful shutdown must revoke all in-flight coordination before completing.
    handle.stop().await;

    // The in-flight lock waiter resolves to Shutdown (not LockTimeout).
    let joined = waiter.await.expect("waiter task joins");
    assert!(
        matches!(joined, Err(ClusterError::Shutdown)),
        "an in-flight lock waiter must observe Shutdown on stop; got {joined:?}"
    );
    // The service-discovery watch observes a terminal Closed(Shutdown).
    assert!(matches!(
        sd_watch.recv().await,
        Some(ServiceWatchEvent::Closed(ClusterError::Shutdown))
    ));
    // The cache watch observes a terminal Closed(Shutdown) via the plugin stop hook.
    assert!(matches!(
        cache_watch.recv().await,
        Some(CacheWatchEvent::Closed(ClusterError::Shutdown))
    ));
}

#[tokio::test]
async fn explicit_backends_override_defaults() {
    let hub = Arc::new(ClientHub::new());
    let plugin = StandaloneClusterPlugin::builder()
        .build_and_start()
        .expect("plugin starts");

    // Bind every primitive explicitly (here, the plugin's own backends).
    let backends = ProfileBackends::new(plugin.cache())
        .with_leader_election(plugin.leader_election())
        .with_lock(plugin.lock())
        .with_service_discovery(plugin.service_discovery());

    let handle = ClusterWiring::builder(Arc::clone(&hub))
        .profile(EventBroker, backends)
        .on_stop(move || async move { plugin.stop().await })
        .build_and_start()
        .expect("wiring starts");

    assert!(
        DistributedLockV1::resolver(&hub)
            .profile(EventBroker)
            .resolve()
            .is_ok()
    );

    handle.stop().await;
}
