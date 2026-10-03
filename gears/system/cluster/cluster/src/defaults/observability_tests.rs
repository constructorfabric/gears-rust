//! Emission tests for the SDK default backends: a recording [`ClusterMetrics`]
//! sink injected via `with_observability` asserts each primitive records the
//! contracted bounded-label metric. The `provider` label is fixed by the sink,
//! so high-cardinality values cannot reach a metric label by construction.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use super::test_cache::MemoryCache;
use super::{CasBasedDistributedLockBackend, CasBasedLeaderElectionBackend};
use cluster_sdk::leader::{LeaderElectionBackend, LeaderStatus, LeaderWatchEvent};
use cluster_sdk::lock::DistributedLockBackend;
use cluster_sdk::observability::ClusterMetrics;

#[derive(Default)]
struct Rec {
    lock_ops: Mutex<Vec<(String, String)>>,
    leader_transitions: Mutex<Vec<String>>,
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

/// The token path records under its **own** `op` labels, distinct from the guard
/// path's: `acquire`/`acquire_waiting`/`token_renew`/`token_release` rather than
/// `try_lock`/`lock`/`renew`/`release`. Every Profile-3 lock RPC takes the token
/// path, so an operator watching remote lock RPCs fail while the in-process guard
/// path is healthy needs the two apart. Every native backend (Redis, Postgres,
/// k8s) draws the same split, so moving a profile between backends keeps the same
/// label set.
#[tokio::test]
async fn the_token_path_records_its_own_op_labels() {
    let rec = Arc::new(Rec::default());
    let backend = CasBasedDistributedLockBackend::new(MemoryCache::linearizable())
        .expect("linearizable cache")
        .with_observability("test", Arc::clone(&rec) as _);
    let ttl = Duration::from_secs(30);

    let token = backend
        .acquire("ledger", "owner-a", ttl)
        .await
        .expect("free lock acquires");
    backend.renew(&token, ttl).await.expect("renew");
    backend.release(&token).await.expect("release");
    let waited = backend
        .acquire_waiting("ledger", "owner-a", ttl, Duration::from_secs(1))
        .await
        .expect("free lock acquires");
    backend.release(&waited).await.expect("release");

    let ops = rec.lock_ops.lock().unwrap().clone();
    assert_eq!(
        ops,
        [
            ("acquire", "ok"),
            ("token_renew", "ok"),
            ("token_release", "ok"),
            ("acquire_waiting", "ok"),
            ("token_release", "ok"),
        ]
        .map(|(op, result)| (op.to_owned(), result.to_owned())),
        "the token path must not report under the guard path's labels"
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
