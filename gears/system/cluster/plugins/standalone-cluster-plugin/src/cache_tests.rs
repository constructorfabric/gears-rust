// Created: 2026-06-11 by Constructor Tech
use std::time::Duration;

use cluster_sdk::{
    CacheConsistency, CacheEvent, CacheWatchEvent, ClusterCacheBackend, ClusterError,
};
use tokio_util::sync::CancellationToken;

use super::StandaloneCache;

#[tokio::test]
async fn put_get_and_versioning() {
    let cache = StandaloneCache::new();
    assert_eq!(cache.consistency(), CacheConsistency::Linearizable);
    assert!(cache.features().prefix_watch);

    let Ok(Some(created)) = cache.put_if_absent("k", b"a", None).await else {
        panic!("first claim must create");
    };
    assert_eq!(created.version, 1);
    assert!(cache.put("k", b"b", None).await.is_ok());
    let Ok(Some(entry)) = cache.get("k").await else {
        panic!("entry must be present");
    };
    assert_eq!(entry.version, 2);
    assert_eq!(entry.value, b"b");
}

#[tokio::test(start_paused = true)]
async fn sweeper_expires_and_emits_then_stops_on_cancel() {
    let cache = StandaloneCache::new();
    let shutdown = CancellationToken::new();
    let sweeper = cache.spawn_sweeper(Duration::from_millis(25), shutdown.clone());

    let Ok(mut watch) = cache.watch("k").await else {
        panic!("watch must establish");
    };
    assert!(
        cache
            .put("k", b"a", Some(Duration::from_secs(10)))
            .await
            .is_ok()
    );
    assert!(matches!(
        watch.recv().await,
        Some(CacheWatchEvent::Event(CacheEvent::Changed { .. }))
    ));

    tokio::time::advance(Duration::from_secs(11)).await;
    tokio::task::yield_now().await;
    assert!(matches!(
        watch.recv().await,
        Some(CacheWatchEvent::Event(CacheEvent::Expired { .. }))
    ));

    // Cancelling stops the sweeper task.
    shutdown.cancel();
    assert!(sweeper.await.is_ok(), "sweeper must exit on cancellation");
}

#[tokio::test]
async fn shutdown_closes_active_watches_and_rejects_later_ops() {
    let cache = StandaloneCache::new();
    let Ok(mut exact) = cache.watch("k").await else {
        panic!("exact watch must establish");
    };
    let Ok(mut prefix) = cache.watch_prefix("svc/").await else {
        panic!("prefix watch must establish");
    };

    cache.shutdown();

    // Every active watch observes a terminal Closed(Shutdown).
    assert!(matches!(
        exact.recv().await,
        Some(CacheWatchEvent::Closed(ClusterError::Shutdown))
    ));
    assert!(matches!(
        prefix.recv().await,
        Some(CacheWatchEvent::Closed(ClusterError::Shutdown))
    ));

    // Post-shutdown mutating ops are rejected so a racing op cannot
    // resurrect a watcher.
    assert!(matches!(
        cache.put("k", b"v", None).await,
        Err(ClusterError::Shutdown)
    ));
    assert!(matches!(
        cache.delete("k").await,
        Err(ClusterError::Shutdown)
    ));
    assert!(matches!(
        cache.put_if_absent("k", b"v", None).await,
        Err(ClusterError::Shutdown)
    ));
    assert!(matches!(
        cache.compare_and_swap("k", 1, b"v", None).await,
        Err(ClusterError::Shutdown)
    ));
    // New watches are rejected too.
    assert!(matches!(
        cache.watch("k").await,
        Err(ClusterError::Shutdown)
    ));
    assert!(matches!(
        cache.watch_prefix("svc/").await,
        Err(ClusterError::Shutdown)
    ));
    // Reads stay live (harmless and cannot resurrect a watcher).
    assert!(cache.get("k").await.is_ok());
    assert!(cache.contains("k").await.is_ok());
}

#[tokio::test]
async fn slow_watcher_never_blocks_writers_and_is_told_it_lagged() {
    let cache = StandaloneCache::new();
    // A watcher we deliberately never drain until the end.
    let Ok(mut watch) = cache.watch("k").await else {
        panic!("watch must establish");
    };

    // Fill the buffer exactly, then overflow it. Under a blocking broadcast
    // the overflowing writes would deadlock here (no consumer is draining);
    // with the non-blocking broadcast every write must complete promptly.
    let overflow: usize = 50;
    for _ in 0..(super::WATCH_CAPACITY + overflow) {
        assert!(
            cache.put("k", b"v", None).await.is_ok(),
            "a slow watcher must not stall the write path"
        );
    }

    // Drain the buffered events to free space; the dropped count is held
    // until the next broadcast can deliver a Lagged notice.
    for _ in 0..super::WATCH_CAPACITY {
        assert!(matches!(
            watch.recv().await,
            Some(CacheWatchEvent::Event(CacheEvent::Changed { .. }))
        ));
    }

    // The next write flushes the coalesced lag notice ahead of its event.
    assert!(cache.put("k", b"v", None).await.is_ok());
    assert!(
        matches!(watch.recv().await, Some(CacheWatchEvent::Lagged { dropped }) if dropped == overflow as u64),
        "consumer must be told exactly how many events it missed"
    );
    assert!(matches!(
        watch.recv().await,
        Some(CacheWatchEvent::Event(CacheEvent::Changed { .. }))
    ));
}
