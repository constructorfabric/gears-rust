//! Emission tests for the SDK default backends: a recording [`ClusterMetrics`]
//! sink injected via `with_observability` asserts each primitive records the
//! contracted bounded-label metric. The `provider` label is fixed by the sink,
//! so high-cardinality values cannot reach a metric label by construction.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use super::test_cache::MemoryCache;
use super::{
    CacheBasedServiceDiscoveryBackend, CasBasedDistributedLockBackend,
    CasBasedLeaderElectionBackend,
};
use cluster_sdk::discovery::{DiscoveryFilter, ServiceDiscoveryBackend, ServiceRegistration};
use cluster_sdk::leader::{LeaderElectionBackend, LeaderStatus, LeaderWatchEvent};
use cluster_sdk::lock::DistributedLockBackend;
use cluster_sdk::observability::ClusterMetrics;

#[derive(Default)]
struct Rec {
    lock_ops: Mutex<Vec<(String, String)>>,
    leader_transitions: Mutex<Vec<String>>,
    discovery_ops: Mutex<Vec<(String, String)>>,
}

impl ClusterMetrics for Rec {
    fn cache_op(&self, _op: &str, _result: &str) {}
    fn cache_op_duration(&self, _op: &str, _seconds: f64) {}
    fn lock_op(&self, op: &str, result: &str) {
        self.lock_ops
            .lock()
            .unwrap()
            .push((op.to_owned(), result.to_owned()));
    }
    fn lock_op_duration(&self, _op: &str, _seconds: f64) {}
    fn leader_transition(&self, transition: &str) {
        self.leader_transitions
            .lock()
            .unwrap()
            .push(transition.to_owned());
    }
    fn discovery_op(&self, op: &str, result: &str) {
        self.discovery_ops
            .lock()
            .unwrap()
            .push((op.to_owned(), result.to_owned()));
    }
    fn watch_reset(&self, _primitive: &str) {}
    fn provider_error(&self, _kind: &str) {}
}

async fn settle() {
    for _ in 0..32 {
        tokio::task::yield_now().await;
    }
}

#[tokio::test]
async fn lock_records_acquire_and_contention() {
    let rec = Arc::new(Rec::default());
    let backend = CasBasedDistributedLockBackend::new(MemoryCache::linearizable())
        .expect("linearizable cache")
        .with_observability("test", Arc::clone(&rec) as _);

    let _guard = backend
        .try_lock("ledger", Duration::from_secs(30))
        .await
        .expect("free lock acquires");
    // A second acquisition of the held lock is contended.
    let _contended = backend.try_lock("ledger", Duration::from_secs(30)).await;

    let ops = rec.lock_ops.lock().unwrap();
    assert!(
        ops.contains(&("try_lock".to_owned(), "ok".to_owned())),
        "expected a successful try_lock, got {ops:?}"
    );
    assert!(
        ops.contains(&("try_lock".to_owned(), "contended".to_owned())),
        "expected a contended try_lock, got {ops:?}"
    );
}

#[tokio::test]
async fn leader_records_acquired_transition() {
    let rec = Arc::new(Rec::default());
    let backend = CasBasedLeaderElectionBackend::new(MemoryCache::linearizable())
        .expect("linearizable cache")
        .with_observability("test", Arc::clone(&rec) as _);

    let mut watch = backend.elect("primary").await.expect("election joins");
    assert!(matches!(
        watch.changed().await,
        LeaderWatchEvent::Status(LeaderStatus::Leader)
    ));
    settle().await;

    assert!(
        rec.leader_transitions
            .lock()
            .unwrap()
            .contains(&"acquired".to_owned()),
        "sole candidate must record an `acquired` transition"
    );
}

#[tokio::test]
async fn discovery_records_register_and_discover() {
    let rec = Arc::new(Rec::default());
    let backend = CacheBasedServiceDiscoveryBackend::new(MemoryCache::linearizable())
        .with_observability("test", Arc::clone(&rec) as _);

    let _handle = backend
        .register(ServiceRegistration {
            name: "delivery".to_owned(),
            instance_id: Some("i-1".to_owned()),
            address: "127.0.0.1:9000".to_owned(),
            metadata: std::collections::HashMap::new(),
        })
        .await
        .expect("registration succeeds");
    let _discovered = backend
        .discover("delivery", DiscoveryFilter::default())
        .await;

    let ops = rec.discovery_ops.lock().unwrap();
    assert!(
        ops.contains(&("register".to_owned(), "ok".to_owned())),
        "expected a successful register, got {ops:?}"
    );
    assert!(
        ops.contains(&("discover".to_owned(), "ok".to_owned())),
        "expected a successful discover, got {ops:?}"
    );
}
