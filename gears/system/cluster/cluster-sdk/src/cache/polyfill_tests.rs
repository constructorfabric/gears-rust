// Created: 2026-06-11 by Constructor Tech
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;

use super::PollingPrefixWatch;
use crate::cache::backend::ClusterCacheBackend;
use crate::cache::types::{
    CacheConsistency, CacheEntry, CacheEvent, CacheFeatures, PutRequest, Ttl,
};
use crate::cache::watch::{CacheWatch, CacheWatchEvent};
use crate::error::ClusterError;

const TICK: Duration = Duration::from_millis(100);

/// A minimal in-memory cache supporting `put`/`get`/`delete`/`scan_prefix`,
/// declaring no native prefix watch (the polyfill's target case). Counts
/// `scan_prefix` calls so a test can prove the polling task stops.
struct PollTestCache {
    map: Mutex<HashMap<String, CacheEntry>>,
    scans: AtomicUsize,
}

impl PollTestCache {
    fn new() -> Self {
        Self {
            map: Mutex::new(HashMap::new()),
            scans: AtomicUsize::new(0),
        }
    }

    fn set(&self, key: &str, version: u64) {
        self.map.lock().expect("lock").insert(
            key.to_owned(),
            CacheEntry {
                value: Vec::new(),
                version,
            },
        );
    }

    fn remove(&self, key: &str) {
        self.map.lock().expect("lock").remove(key);
    }

    fn scans(&self) -> usize {
        self.scans.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl ClusterCacheBackend for PollTestCache {
    fn consistency(&self) -> CacheConsistency {
        CacheConsistency::Linearizable
    }

    fn features(&self) -> CacheFeatures {
        CacheFeatures::new(false)
    }

    async fn get(&self, key: &str) -> Result<Option<CacheEntry>, ClusterError> {
        Ok(self.map.lock().expect("lock").get(key).cloned())
    }

    async fn put(&self, _req: PutRequest<'_>) -> Result<(), ClusterError> {
        Err(ClusterError::Unsupported { feature: "put" })
    }

    async fn delete(&self, _key: &str) -> Result<bool, ClusterError> {
        Err(ClusterError::Unsupported { feature: "delete" })
    }

    async fn contains(&self, _key: &str) -> Result<bool, ClusterError> {
        Err(ClusterError::Unsupported {
            feature: "contains",
        })
    }

    async fn put_if_absent(
        &self,
        _req: PutRequest<'_>,
    ) -> Result<Option<CacheEntry>, ClusterError> {
        Err(ClusterError::Unsupported {
            feature: "put_if_absent",
        })
    }

    async fn compare_and_swap(
        &self,
        _key: &str,
        _expected_version: u64,
        _new_value: &[u8],
        _ttl: Ttl,
    ) -> Result<CacheEntry, ClusterError> {
        Err(ClusterError::Unsupported {
            feature: "compare_and_swap",
        })
    }

    async fn watch(&self, _key: &str) -> Result<CacheWatch, ClusterError> {
        Err(ClusterError::Unsupported { feature: "watch" })
    }

    async fn watch_prefix(&self, _prefix: &str) -> Result<CacheWatch, ClusterError> {
        Err(ClusterError::Unsupported {
            feature: "prefix_watch",
        })
    }

    async fn scan_prefix(&self, prefix: &str) -> Result<Vec<String>, ClusterError> {
        self.scans.fetch_add(1, Ordering::SeqCst);
        Ok(self
            .map
            .lock()
            .expect("lock")
            .keys()
            .filter(|k| k.starts_with(prefix))
            .cloned()
            .collect())
    }
}

/// Awaits the next `Event` payload, panicking on a lifecycle/terminal signal.
async fn next_event(watch: &mut CacheWatch) -> CacheEvent {
    match watch.recv().await {
        Some(CacheWatchEvent::Event(event)) => event,
        other => panic!("expected an Event, got {other:?}"),
    }
}

#[tokio::test(start_paused = true)]
async fn emits_changed_for_initial_keys_then_deleted_on_removal() {
    let cache = Arc::new(PollTestCache::new());
    cache.set("p/a", 1);
    cache.set("p/b", 1);
    let mut watch = PollingPrefixWatch::spawn(
        Arc::clone(&cache) as Arc<dyn ClusterCacheBackend>,
        "p/",
        TICK,
    );
    // The first tick fires immediately and reports both initial keys as
    // Changed (order is unspecified — the snapshot is a HashMap).
    let mut initial = Vec::new();
    for _ in 0..2 {
        match next_event(&mut watch).await {
            CacheEvent::Changed { key } => initial.push(key),
            other => panic!("expected Changed, got {other:?}"),
        }
    }
    initial.sort();
    assert_eq!(initial, ["p/a", "p/b"]);

    // Remove a key; the next tick reports it as Deleted (full backend key),
    // and the unchanged key produces no event.
    cache.remove("p/a");
    tokio::time::advance(TICK).await;
    assert_eq!(
        next_event(&mut watch).await,
        CacheEvent::Deleted {
            key: "p/a".to_owned()
        }
    );
}

#[tokio::test(start_paused = true)]
async fn dropping_the_watch_stops_the_polling_task() {
    let cache = Arc::new(PollTestCache::new());
    cache.set("p/a", 1);
    let watch = PollingPrefixWatch::spawn(
        Arc::clone(&cache) as Arc<dyn ClusterCacheBackend>,
        "p/",
        TICK,
    );
    tokio::task::yield_now().await;
    tokio::task::yield_now().await;

    // Drop the consumer; the next tick scans once, fails to send, and returns.
    drop(watch);
    tokio::time::advance(TICK).await;
    tokio::task::yield_now().await;
    tokio::task::yield_now().await;
    let baseline = cache.scans();

    // Further intervals must not scan — the task has stopped.
    for _ in 0..5 {
        tokio::time::advance(TICK).await;
        tokio::task::yield_now().await;
    }
    assert_eq!(
        cache.scans(),
        baseline,
        "polling task must stop once the watch is dropped"
    );
}

#[tokio::test(start_paused = true)]
async fn surfaces_a_backend_error_as_closed() {
    // A cache whose `scan_prefix` errors closes the synthesized watch.
    struct FailingCache;
    #[async_trait]
    impl ClusterCacheBackend for FailingCache {
        fn consistency(&self) -> CacheConsistency {
            CacheConsistency::Linearizable
        }
        fn features(&self) -> CacheFeatures {
            CacheFeatures::new(false)
        }
        async fn get(&self, _key: &str) -> Result<Option<CacheEntry>, ClusterError> {
            Ok(None)
        }
        async fn put(&self, _req: PutRequest<'_>) -> Result<(), ClusterError> {
            Ok(())
        }
        async fn delete(&self, _key: &str) -> Result<bool, ClusterError> {
            Ok(false)
        }
        async fn contains(&self, _key: &str) -> Result<bool, ClusterError> {
            Ok(false)
        }
        async fn put_if_absent(
            &self,
            _req: PutRequest<'_>,
        ) -> Result<Option<CacheEntry>, ClusterError> {
            Ok(None)
        }
        async fn compare_and_swap(
            &self,
            _key: &str,
            _expected_version: u64,
            _new_value: &[u8],
            _ttl: Ttl,
        ) -> Result<CacheEntry, ClusterError> {
            Err(ClusterError::Unsupported {
                feature: "compare_and_swap",
            })
        }
        async fn watch(&self, _key: &str) -> Result<CacheWatch, ClusterError> {
            Err(ClusterError::Unsupported { feature: "watch" })
        }
        async fn watch_prefix(&self, _prefix: &str) -> Result<CacheWatch, ClusterError> {
            Err(ClusterError::Unsupported {
                feature: "prefix_watch",
            })
        }
        async fn scan_prefix(&self, _prefix: &str) -> Result<Vec<String>, ClusterError> {
            Err(ClusterError::Unsupported {
                feature: "scan_prefix",
            })
        }
    }

    let mut watch = PollingPrefixWatch::spawn(
        Arc::new(FailingCache) as Arc<dyn ClusterCacheBackend>,
        "p/",
        TICK,
    );
    match watch.recv().await {
        Some(CacheWatchEvent::Closed(ClusterError::Unsupported { feature })) => {
            assert_eq!(feature, "scan_prefix");
        }
        other => panic!("expected Closed(Unsupported), got {other:?}"),
    }
}

#[tokio::test]
async fn zero_interval_closes_instead_of_panicking() {
    // A zero interval would panic `tokio::time::interval`; the watch must
    // instead surface a single non-retryable terminal close.
    let cache = Arc::new(PollTestCache::new());
    let mut watch = PollingPrefixWatch::spawn(
        Arc::clone(&cache) as Arc<dyn ClusterCacheBackend>,
        "p/",
        Duration::ZERO,
    );
    assert!(matches!(
        watch.recv().await,
        Some(CacheWatchEvent::Closed(ClusterError::InvalidConfig { .. }))
    ));
    assert!(watch.recv().await.is_none(), "the watch is terminal");
    assert_eq!(cache.scans(), 0, "no polling occurs for a zero interval");
}
