// Created: 2026-06-18 by Constructor Tech
use std::sync::{Arc, Mutex};

use async_trait::async_trait;

use super::InstrumentedCache;
use crate::cache::types::{PutRequest, Ttl};
use crate::cache::{CacheConsistency, CacheEntry, CacheFeatures, CacheWatch, ClusterCacheBackend};
use crate::error::{ClusterError, ProviderErrorKind};
use crate::observability::ClusterMetrics;

/// Records every metric call so a test can assert the bounded label values. The
/// `ClusterMetrics` trait has no key/name parameter, so it is *structurally*
/// impossible for a high-cardinality value to reach a metric label — this spy
/// captures only what the contract permits (ADR-004 cardinality rule).
#[derive(Default)]
struct RecordingMetrics {
    cache_ops: Mutex<Vec<(String, String)>>,
    cache_durations: Mutex<Vec<String>>,
    provider_errors: Mutex<Vec<String>>,
}

impl ClusterMetrics for RecordingMetrics {
    fn cache_op(&self, op: &str, result: &str) {
        self.cache_ops
            .lock()
            .unwrap()
            .push((op.to_owned(), result.to_owned()));
    }
    fn cache_op_duration(&self, op: &str, _seconds: f64) {
        self.cache_durations.lock().unwrap().push(op.to_owned());
    }
    fn lock_op(&self, _op: &str, _result: &str) {}
    fn lock_op_duration(&self, _op: &str, _seconds: f64) {}
    fn leader_transition(&self, _transition: &str) {}
    fn discovery_op(&self, _op: &str, _result: &str) {}
    fn watch_reset(&self, _primitive: &str) {}
    fn provider_error(&self, kind: &str) {
        self.provider_errors.lock().unwrap().push(kind.to_owned());
    }
}

/// A backend with fixed per-method outcomes covering the three result classes
/// the decorator must distinguish: success, a normal non-error outcome (CAS
/// conflict), and a genuine provider error.
struct FakeBackend;

#[async_trait]
impl ClusterCacheBackend for FakeBackend {
    fn consistency(&self) -> CacheConsistency {
        CacheConsistency::Linearizable
    }
    fn features(&self) -> CacheFeatures {
        CacheFeatures::new(true)
    }
    async fn get(&self, _key: &str) -> Result<Option<CacheEntry>, ClusterError> {
        Ok(None)
    }
    async fn put(&self, _req: PutRequest<'_>) -> Result<(), ClusterError> {
        Ok(())
    }
    async fn delete(&self, _key: &str) -> Result<bool, ClusterError> {
        // A genuine backend error → drives the provider-error path.
        Err(ClusterError::Provider {
            kind: ProviderErrorKind::Other,
            message: "boom".to_owned(),
        })
    }
    async fn contains(&self, _key: &str) -> Result<bool, ClusterError> {
        Ok(false)
    }
    async fn put_if_absent(
        &self,
        _req: PutRequest<'_>,
    ) -> Result<Option<CacheEntry>, ClusterError> {
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
        // A normal outcome, NOT a provider error.
        Err(ClusterError::CasConflict {
            key: key.to_owned(),
            current: None,
        })
    }
    async fn watch(&self, _key: &str) -> Result<CacheWatch, ClusterError> {
        let (_sender, watch) = CacheWatch::channel(1);
        Ok(watch)
    }
    async fn watch_prefix(&self, _prefix: &str) -> Result<CacheWatch, ClusterError> {
        let (_sender, watch) = CacheWatch::channel(1);
        Ok(watch)
    }
}

fn instrumented() -> (InstrumentedCache, Arc<RecordingMetrics>) {
    let metrics = Arc::new(RecordingMetrics::default());
    let cache = InstrumentedCache::new(Arc::new(FakeBackend), "fake", Arc::clone(&metrics) as _);
    (cache, metrics)
}

#[tokio::test]
async fn success_records_ok_result_and_duration() {
    let (cache, metrics) = instrumented();
    let _outcome = cache.get("session/abc").await;

    assert_eq!(
        metrics.cache_ops.lock().unwrap().as_slice(),
        &[("get".to_owned(), "ok".to_owned())]
    );
    assert_eq!(metrics.cache_durations.lock().unwrap().as_slice(), &["get"]);
    // A success is not a provider error.
    assert!(metrics.provider_errors.lock().unwrap().is_empty());
}

#[tokio::test]
async fn cas_conflict_is_a_bounded_result_not_a_provider_error() {
    let (cache, metrics) = instrumented();
    let _outcome = cache.compare_and_swap("k", 1, b"v", Ttl::Indefinite).await;

    assert_eq!(
        metrics.cache_ops.lock().unwrap().as_slice(),
        &[("compare_and_swap".to_owned(), "conflict".to_owned())]
    );
    assert!(
        metrics.provider_errors.lock().unwrap().is_empty(),
        "a CAS conflict is a normal outcome, never a provider error"
    );
}

#[tokio::test]
async fn provider_error_records_both_the_result_and_the_error_kind() {
    let (cache, metrics) = instrumented();
    let _outcome = cache.delete("k").await;

    assert_eq!(
        metrics.cache_ops.lock().unwrap().as_slice(),
        &[("delete".to_owned(), "error".to_owned())]
    );
    assert_eq!(
        metrics.provider_errors.lock().unwrap().as_slice(),
        &["other"]
    );
}
