// Created: 2026-06-11 by Constructor Tech
use std::sync::{Arc, Mutex};

use async_trait::async_trait;

use super::ScopedCacheBackend;
use crate::cache::backend::ClusterCacheBackend;
use crate::cache::types::{
    CacheConsistency, CacheEntry, CacheEvent, CacheFeatures, PutRequest, Ttl,
};
use crate::cache::watch::{CacheWatch, CacheWatchEvent};
use crate::error::ClusterError;
use crate::scope;

/// A stub cache that records the keys it is asked about, seeds a fixed
/// keyspace for `scan_prefix`, and emits one `Changed` event (carrying the
/// backend-facing key) on `watch`/`watch_prefix`.
struct RecordingCache {
    seen: Mutex<Vec<String>>,
    keys: Vec<String>,
}

impl RecordingCache {
    fn new() -> Self {
        Self {
            seen: Mutex::new(Vec::new()),
            keys: vec![
                "event-broker/a".to_owned(),
                "event-broker/b".to_owned(),
                "other/c".to_owned(),
            ],
        }
    }
}

#[async_trait]
impl ClusterCacheBackend for RecordingCache {
    fn consistency(&self) -> CacheConsistency {
        CacheConsistency::Linearizable
    }

    fn features(&self) -> CacheFeatures {
        CacheFeatures::new(true)
    }

    async fn get(&self, key: &str) -> Result<Option<CacheEntry>, ClusterError> {
        self.seen.lock().expect("lock").push(key.to_owned());
        Ok(None)
    }

    async fn put(&self, req: PutRequest<'_>) -> Result<(), ClusterError> {
        self.seen.lock().expect("lock").push(req.key.to_owned());
        Ok(())
    }

    async fn delete(&self, key: &str) -> Result<bool, ClusterError> {
        self.seen.lock().expect("lock").push(key.to_owned());
        Ok(true)
    }

    async fn contains(&self, key: &str) -> Result<bool, ClusterError> {
        self.seen.lock().expect("lock").push(key.to_owned());
        Ok(false)
    }

    async fn put_if_absent(&self, req: PutRequest<'_>) -> Result<Option<CacheEntry>, ClusterError> {
        self.seen.lock().expect("lock").push(req.key.to_owned());
        Ok(Some(CacheEntry {
            value: Vec::new(),
            version: 1,
        }))
    }

    async fn compare_and_swap(
        &self,
        key: &str,
        _expected_version: u64,
        _new_value: &[u8],
        _ttl: Ttl,
    ) -> Result<CacheEntry, ClusterError> {
        self.seen.lock().expect("lock").push(key.to_owned());
        Ok(CacheEntry {
            value: Vec::new(),
            version: 2,
        })
    }

    async fn watch(&self, key: &str) -> Result<CacheWatch, ClusterError> {
        self.seen.lock().expect("lock").push(key.to_owned());
        let (tx, watch) = CacheWatch::channel(8);
        // Emit one event carrying the backend-facing (prefixed) key, then end.
        tx.send(CacheWatchEvent::Event(CacheEvent::Changed {
            key: key.to_owned(),
        }))
        .await
        .ok();
        Ok(watch)
    }

    async fn watch_prefix(&self, prefix: &str) -> Result<CacheWatch, ClusterError> {
        self.seen.lock().expect("lock").push(prefix.to_owned());
        let (tx, watch) = CacheWatch::channel(8);
        let event_key = format!("{prefix}item");
        tx.send(CacheWatchEvent::Event(CacheEvent::Changed {
            key: event_key,
        }))
        .await
        .ok();
        Ok(watch)
    }

    async fn scan_prefix(&self, prefix: &str) -> Result<Vec<String>, ClusterError> {
        self.seen.lock().expect("lock").push(prefix.to_owned());
        Ok(self
            .keys
            .iter()
            .filter(|k| k.starts_with(prefix))
            .cloned()
            .collect())
    }
}

fn scoped(inner: Arc<RecordingCache>, prefix: &str) -> ScopedCacheBackend {
    ScopedCacheBackend::new(
        inner,
        scope::validated_prefix(prefix).expect("valid prefix"),
    )
}

#[tokio::test]
async fn write_path_prepends_the_prefix() {
    let cache = Arc::new(RecordingCache::new());
    let wrapper = scoped(Arc::clone(&cache), "event-broker");
    assert!(
        wrapper
            .put(PutRequest {
                key: "shard-assignments",
                value: b"v",
                ttl: Ttl::Indefinite,
            })
            .await
            .is_ok()
    );
    assert!(wrapper.get("shard-assignments").await.is_ok());
    assert_eq!(
        cache.seen.lock().expect("lock").as_slice(),
        [
            "event-broker/shard-assignments",
            "event-broker/shard-assignments"
        ]
    );
}

#[tokio::test]
async fn watch_strips_the_prefix_from_event_keys() {
    let cache = Arc::new(RecordingCache::new());
    let wrapper = scoped(Arc::clone(&cache), "event-broker");
    let mut watch = wrapper.watch("shard-assignments").await.expect("watch");
    // The backend saw the prefixed key...
    assert_eq!(
        cache.seen.lock().expect("lock").as_slice(),
        ["event-broker/shard-assignments"]
    );
    // ...but the consumer sees the name relative to its scope.
    match watch.recv().await {
        Some(CacheWatchEvent::Event(CacheEvent::Changed { key })) => {
            assert_eq!(key, "shard-assignments");
        }
        other => panic!("expected a stripped Changed event, got {other:?}"),
    }
}

#[tokio::test]
async fn watch_prefix_strips_the_prefix_from_event_keys() {
    let cache = Arc::new(RecordingCache::new());
    let wrapper = scoped(Arc::clone(&cache), "event-broker");
    // Watch the whole scope (relative prefix "").
    let mut watch = wrapper.watch_prefix("").await.expect("watch_prefix");
    assert_eq!(
        cache.seen.lock().expect("lock").as_slice(),
        ["event-broker/"]
    );
    match watch.recv().await {
        Some(CacheWatchEvent::Event(CacheEvent::Changed { key })) => {
            assert_eq!(key, "item");
        }
        other => panic!("expected a stripped Changed event, got {other:?}"),
    }
}

#[tokio::test]
async fn scan_prefix_strips_the_prefix_from_returned_keys() {
    let cache = Arc::new(RecordingCache::new());
    let wrapper = scoped(Arc::clone(&cache), "event-broker");
    let mut keys = wrapper.scan_prefix("").await.expect("scan");
    keys.sort();
    assert_eq!(keys, ["a", "b"]);
}

#[tokio::test]
async fn scoping_composes_when_nested() {
    let cache = Arc::new(RecordingCache::new());
    let inner = scoped(Arc::clone(&cache), "event-broker");
    let outer = ScopedCacheBackend::new(
        Arc::new(inner),
        scope::validated_prefix("shard-0").expect("valid"),
    );
    assert!(
        outer
            .put(PutRequest {
                key: "k",
                value: b"v",
                ttl: Ttl::Indefinite,
            })
            .await
            .is_ok()
    );
    assert_eq!(
        cache.seen.lock().expect("lock").as_slice(),
        ["event-broker/shard-0/k"]
    );
}
