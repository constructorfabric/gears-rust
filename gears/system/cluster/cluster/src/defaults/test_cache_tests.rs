use std::time::Duration;

use super::MemoryCache;
use cluster_sdk::cache::types::{PutRequest, Ttl};
use cluster_sdk::cache::{CacheEvent, CacheWatchEvent, ClusterCacheBackend};
use cluster_sdk::error::ClusterError;

#[tokio::test]
async fn put_if_absent_is_exclusive_then_versions_increment() {
    let cache = MemoryCache::linearizable();
    let Ok(Some(first)) = cache
        .put_if_absent(PutRequest {
            key: "k",
            value: b"a",
            ttl: Ttl::Indefinite,
        })
        .await
    else {
        panic!("first claim must create");
    };
    assert_eq!(first.version, 1);
    // A second claim while present is refused.
    let Ok(None) = cache
        .put_if_absent(PutRequest {
            key: "k",
            value: b"b",
            ttl: Ttl::Indefinite,
        })
        .await
    else {
        panic!("second claim must be refused");
    };
    // Overwrite increments the version.
    assert!(
        cache
            .put(PutRequest {
                key: "k",
                value: b"c",
                ttl: Ttl::Indefinite,
            })
            .await
            .is_ok()
    );
    let Ok(Some(entry)) = cache.get("k").await else {
        panic!("entry must be present");
    };
    assert_eq!(entry.version, 2);
    assert_eq!(entry.value, b"c");
}

#[tokio::test]
async fn compare_and_swap_enforces_version() {
    let cache = MemoryCache::linearizable();
    let Ok(Some(entry)) = cache
        .put_if_absent(PutRequest {
            key: "k",
            value: b"a",
            ttl: Ttl::Indefinite,
        })
        .await
    else {
        panic!("create must succeed");
    };
    // Wrong version conflicts and reports the current entry.
    assert!(matches!(
        cache
            .compare_and_swap("k", entry.version + 9, b"z", Ttl::Indefinite)
            .await,
        Err(ClusterError::CasConflict {
            current: Some(_),
            ..
        })
    ));
    // Correct version swaps and bumps.
    let Ok(swapped) = cache
        .compare_and_swap("k", entry.version, b"b", Ttl::Indefinite)
        .await
    else {
        panic!("matching CAS must succeed");
    };
    assert_eq!(swapped.version, entry.version + 1);
}

#[tokio::test(start_paused = true)]
async fn ttl_expiry_reads_as_absent_and_emits_expired() {
    let cache = MemoryCache::linearizable();
    let Ok(mut watch) = cache.watch("k").await else {
        panic!("watch must establish");
    };
    assert!(
        cache
            .put(PutRequest {
                key: "k",
                value: b"a",
                ttl: Ttl::Of(Duration::from_secs(10)),
            })
            .await
            .is_ok()
    );
    // The create event arrives.
    assert!(matches!(
        watch.recv().await,
        Some(CacheWatchEvent::Event(CacheEvent::Changed { .. }))
    ));
    // Advance past the TTL: the sweeper expires it and reads go absent.
    tokio::time::advance(Duration::from_secs(11)).await;
    // Let the sweeper run.
    tokio::task::yield_now().await;
    let Ok(absent) = cache.get("k").await else {
        panic!("get must succeed");
    };
    assert!(absent.is_none(), "expired entry reads as absent");
    assert!(matches!(
        watch.recv().await,
        Some(CacheWatchEvent::Event(CacheEvent::Expired { .. }))
    ));
}

#[tokio::test]
async fn prefix_watch_observes_matching_keys_only() {
    let cache = MemoryCache::linearizable();
    let Ok(mut watch) = cache.watch_prefix("svc/").await else {
        panic!("prefix watch must establish");
    };
    assert!(
        cache
            .put(PutRequest {
                key: "svc/a",
                value: b"1",
                ttl: Ttl::Indefinite,
            })
            .await
            .is_ok()
    );
    assert!(
        cache
            .put(PutRequest {
                key: "other/b",
                value: b"2",
                ttl: Ttl::Indefinite,
            })
            .await
            .is_ok()
    );
    assert!(matches!(
        watch.recv().await,
        Some(CacheWatchEvent::Event(CacheEvent::Changed { key })) if key == "svc/a"
    ));
    // `other/b` is not under the prefix, so the next event is the unrelated
    // key never arriving — assert by checking the channel has no immediately
    // ready non-matching event via a follow-up matching put.
    assert!(
        cache
            .put(PutRequest {
                key: "svc/c",
                value: b"3",
                ttl: Ttl::Indefinite,
            })
            .await
            .is_ok()
    );
    assert!(matches!(
        watch.recv().await,
        Some(CacheWatchEvent::Event(CacheEvent::Changed { key })) if key == "svc/c"
    ));
}

#[tokio::test]
async fn prefix_watch_unsupported_is_reported() {
    let cache = MemoryCache::linearizable_without_prefix_watch();
    assert!(matches!(
        cache.watch_prefix("svc/").await,
        Err(ClusterError::Unsupported {
            feature: "prefix_watch"
        })
    ));
}
