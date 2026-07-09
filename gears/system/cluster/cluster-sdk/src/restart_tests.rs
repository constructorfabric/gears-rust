// Created: 2026-06-11 by Constructor Tech
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use async_trait::async_trait;

use super::RetryPolicy;
use crate::cache::types::{PutRequest, Ttl};
use crate::cache::{
    CacheConsistency, CacheEntry, CacheEvent, CacheFeatures, CacheWatch, CacheWatchEvent,
    CacheWatchSender, ClusterCacheBackend, ClusterCacheV1,
};
use crate::discovery::{ServiceWatch, ServiceWatchEvent};
use crate::error::{ClusterError, ProviderErrorKind};
use crate::leader::{LeaderStatus, LeaderWatch, LeaderWatchEvent};
use crate::observability::ClusterMetrics;

/// Records `watch_reset` calls so a test can assert the combinator emits the
/// watch-reset signal (and only it) on a reconnect.
#[derive(Default)]
struct ResetRecorder {
    resets: std::sync::Mutex<Vec<String>>,
}

impl ClusterMetrics for ResetRecorder {
    fn cache_op(&self, _op: &str, _result: &str) {}
    fn cache_op_duration(&self, _op: &str, _seconds: f64) {}
    fn lock_op(&self, _op: &str, _result: &str) {}
    fn lock_op_duration(&self, _op: &str, _seconds: f64) {}
    fn leader_transition(&self, _transition: &str) {}
    fn discovery_op(&self, _op: &str, _result: &str) {}
    fn watch_reset(&self, primitive: &str) {
        self.resets.lock().expect("lock").push(primitive.to_owned());
    }
    fn provider_error(&self, _kind: &str) {}
}

fn retryable_close() -> ClusterError {
    ClusterError::Provider {
        kind: ProviderErrorKind::ConnectionLost,
        message: "connection dropped".to_owned(),
    }
}

/// A cache backend whose first `flaky` `watch` calls return a subscription
/// pre-loaded with a retryable `Closed`, and whose next `watch` returns a
/// live subscription with its sender parked for the test to drive. Proves the
/// facade re-installs the resubscribe seam on each reconnected watch, so
/// `auto_restart` reconnects *repeatedly*, not just once.
struct FlakyCache {
    calls: AtomicU32,
    flaky: u32,
    live_sender: std::sync::Mutex<Option<CacheWatchSender>>,
}

#[async_trait]
impl ClusterCacheBackend for FlakyCache {
    fn consistency(&self) -> CacheConsistency {
        CacheConsistency::Linearizable
    }
    fn features(&self) -> CacheFeatures {
        CacheFeatures::new(true)
    }
    async fn watch(&self, _key: &str) -> Result<CacheWatch, ClusterError> {
        let n = self.calls.fetch_add(1, Ordering::SeqCst);
        let (tx, watch) = CacheWatch::channel(8);
        if n < self.flaky {
            // Pre-load a retryable terminal close; the sender then drops.
            tx.send(CacheWatchEvent::Closed(retryable_close()))
                .await
                .ok();
        } else {
            // A live subscription; park the sender for the test to drive.
            *self.live_sender.lock().expect("lock") = Some(tx);
        }
        Ok(watch)
    }
    async fn get(&self, _key: &str) -> Result<Option<CacheEntry>, ClusterError> {
        unimplemented!("not exercised by the auto-restart test")
    }
    async fn put(&self, _req: PutRequest<'_>) -> Result<(), ClusterError> {
        unimplemented!("not exercised by the auto-restart test")
    }
    async fn delete(&self, _key: &str) -> Result<bool, ClusterError> {
        unimplemented!("not exercised by the auto-restart test")
    }
    async fn contains(&self, _key: &str) -> Result<bool, ClusterError> {
        unimplemented!("not exercised by the auto-restart test")
    }
    async fn put_if_absent(
        &self,
        _req: PutRequest<'_>,
    ) -> Result<Option<CacheEntry>, ClusterError> {
        unimplemented!("not exercised by the auto-restart test")
    }
    async fn compare_and_swap(
        &self,
        _key: &str,
        _expected_version: u64,
        _new_value: &[u8],
        _ttl: Ttl,
    ) -> Result<CacheEntry, ClusterError> {
        unimplemented!("not exercised by the auto-restart test")
    }
    async fn watch_prefix(&self, _prefix: &str) -> Result<CacheWatch, ClusterError> {
        unimplemented!("not exercised by the auto-restart test")
    }
}

/// `RestartingWatch<W>` must be `Send` so consumers can `tokio::spawn` it.
const _: fn() = || {
    fn assert_send<T: Send>() {}
    assert_send::<super::RestartingWatch<CacheWatch>>();
    assert_send::<super::RestartingWatch<LeaderWatch>>();
    assert_send::<super::RestartingWatch<ServiceWatch>>();
};

#[tokio::test(start_paused = true)]
async fn leader_shutdown_close_propagates_through_combinator_then_terminates() {
    // Shutdown is non-retryable: it must propagate unchanged and terminate,
    // even though `LeaderWatch::changed` would keep re-yielding it on a bare
    // watch. (Seam-less; reconnection is irrelevant for a non-retryable close.)
    let (tx, _resign, watch) = LeaderWatch::channel(8, LeaderStatus::Follower);
    let mut restarting = watch.auto_restart(RetryPolicy::default());
    tx.send(LeaderWatchEvent::Closed(ClusterError::Shutdown))
        .await
        .expect("send");
    assert!(matches!(
        restarting.recv().await,
        Some(LeaderWatchEvent::Closed(ClusterError::Shutdown))
    ));
    assert!(restarting.recv().await.is_none());
}

#[tokio::test(start_paused = true)]
async fn facade_reseams_so_reconnect_repeats() {
    // Initial subscription (call 0) and the first reconnect target (call 1)
    // are both flaky; call 2 is live.
    let backend = Arc::new(FlakyCache {
        calls: AtomicU32::new(0),
        flaky: 2,
        live_sender: std::sync::Mutex::new(None),
    });
    let cache = ClusterCacheV1::from_backend(backend.clone());
    let watch = cache.watch("k").await.expect("watch");
    let mut restarting = watch.auto_restart(RetryPolicy::default());

    // Call 0's close -> reconnect to call 1 -> synthesized Reset.
    assert!(matches!(
        restarting.recv().await,
        Some(CacheWatchEvent::Reset)
    ));
    // Call 1's close -> reconnect to call 2 -> Reset. The single-reconnect
    // bug would instead surface this close and terminate, because the watch
    // returned by the bare backend carried no seam.
    assert!(matches!(
        restarting.recv().await,
        Some(CacheWatchEvent::Reset)
    ));

    // The live subscription (call 2) is seamed and delivering.
    let live = backend
        .live_sender
        .lock()
        .expect("lock")
        .take()
        .expect("live sender parked on the third watch call");
    live.send(CacheWatchEvent::Event(CacheEvent::Changed {
        key: "k".to_owned(),
    }))
    .await
    .expect("send event");
    assert!(matches!(
        restarting.recv().await,
        Some(CacheWatchEvent::Event(CacheEvent::Changed { .. }))
    ));
    assert_eq!(
        backend.calls.load(Ordering::SeqCst),
        3,
        "initial + two reconnects"
    );
}

#[test]
fn default_policy_matches_design() {
    let p = RetryPolicy::default();
    assert_eq!(p.initial_backoff, Duration::from_secs(1));
    assert_eq!(p.max_backoff, Duration::from_secs(30));
    assert!((p.jitter_factor - 1.0).abs() < f32::EPSILON);
    assert_eq!(p.max_retries, None);
    // Behavioral: verify schedule is non-degenerate with zero jitter, then
    // verify full-jitter variant stays within bounds.
    let zero_jitter = RetryPolicy {
        jitter_factor: 0.0,
        ..p
    };
    assert_eq!(zero_jitter.backoff_for(0), Duration::from_secs(1));
    assert_eq!(zero_jitter.backoff_for(10), Duration::from_secs(30));
    assert!(p.backoff_for(0) <= Duration::from_secs(1));
    assert!(p.backoff_for(10) <= Duration::from_secs(30));
}

#[test]
fn backoff_grows_then_caps_without_jitter() {
    let p = RetryPolicy {
        initial_backoff: Duration::from_secs(1),
        max_backoff: Duration::from_secs(8),
        jitter_factor: 0.0,
        max_retries: None,
    };
    assert_eq!(p.backoff_for(0), Duration::from_secs(1));
    assert_eq!(p.backoff_for(1), Duration::from_secs(2));
    assert_eq!(p.backoff_for(2), Duration::from_secs(4));
    // 2^3 = 8s hits the cap, and every later attempt stays capped.
    assert_eq!(p.backoff_for(3), Duration::from_secs(8));
    assert_eq!(p.backoff_for(64), Duration::from_secs(8));
}

#[test]
fn full_jitter_stays_within_bounds() {
    let p = RetryPolicy {
        initial_backoff: Duration::from_secs(4),
        max_backoff: Duration::from_secs(30),
        jitter_factor: 1.0,
        max_retries: None,
    };
    for _ in 0..256 {
        let d = p.backoff_for(0);
        assert!(
            d <= Duration::from_secs(4),
            "jittered delay must not exceed the base"
        );
    }
}

/// Installs a resubscribe seam on a cache watch that, on each call, hands out
/// the next pre-seeded `CacheWatch` so tests can drive reconnection.
fn cache_watch_with_resubscribes(followups: Vec<CacheWatch>) -> (CacheWatchSender, CacheWatch) {
    let (tx, mut watch) = CacheWatch::channel(8);
    let queue = Arc::new(std::sync::Mutex::new(
        followups
            .into_iter()
            .collect::<std::collections::VecDeque<_>>(),
    ));
    watch.set_resubscribe(move || {
        let queue = Arc::clone(&queue);
        Box::pin(async move {
            queue
                .lock()
                .expect("queue lock")
                .pop_front()
                .ok_or(ClusterError::Shutdown)
        })
    });
    (tx, watch)
}

#[tokio::test(start_paused = true)]
async fn retryable_close_reconnects_and_synthesizes_reset() {
    // The follow-up subscription delivers a real event after reconnect.
    let (fresh_tx, fresh_watch) = CacheWatch::channel(8);
    let (tx, watch) = cache_watch_with_resubscribes(vec![fresh_watch]);

    let mut restarting = watch.auto_restart(RetryPolicy::default());

    // First subscription closes with a retryable error.
    tx.send(CacheWatchEvent::Closed(retryable_close()))
        .await
        .expect("send close");
    drop(tx);

    // The combinator absorbs the close and surfaces a synthesized Reset.
    assert!(matches!(
        restarting.recv().await,
        Some(CacheWatchEvent::Reset)
    ));

    // ...then events flow from the reconnected subscription.
    fresh_tx
        .send(CacheWatchEvent::Event(CacheEvent::Changed {
            key: "k".to_owned(),
        }))
        .await
        .expect("send event");
    assert!(matches!(
        restarting.recv().await,
        Some(CacheWatchEvent::Event(CacheEvent::Changed { .. }))
    ));
}

#[tokio::test(start_paused = true)]
async fn reconnect_emits_watch_reset_when_instrumented() {
    let metrics = Arc::new(ResetRecorder::default());
    let (fresh_tx, fresh_watch) = CacheWatch::channel(8);
    let (tx, mut watch) = cache_watch_with_resubscribes(vec![fresh_watch]);
    // Stamp the watch as the instrumented backend would; the combinator captures
    // this at `auto_restart` time and emits on each successful reconnect.
    watch.set_observability("test", Arc::clone(&metrics) as _);
    let mut restarting = watch.auto_restart(RetryPolicy::default());

    tx.send(CacheWatchEvent::Closed(retryable_close()))
        .await
        .expect("send close");
    drop(tx);

    // The retryable close drives one reconnect -> one synthesized Reset.
    assert!(matches!(
        restarting.recv().await,
        Some(CacheWatchEvent::Reset)
    ));
    let _keep = fresh_tx; // keep the reconnected subscription alive
    assert_eq!(
        metrics.resets.lock().expect("lock").as_slice(),
        &["cache"],
        "exactly one cache watch-reset is recorded per reconnect"
    );
}

#[tokio::test(start_paused = true)]
async fn non_retryable_close_emits_no_watch_reset() {
    let metrics = Arc::new(ResetRecorder::default());
    let (fresh_tx, fresh_watch) = CacheWatch::channel(8);
    let _keep = fresh_tx; // reconnect target that must never be used
    let (tx, mut watch) = cache_watch_with_resubscribes(vec![fresh_watch]);
    watch.set_observability("test", Arc::clone(&metrics) as _);
    let mut restarting = watch.auto_restart(RetryPolicy::default());

    // A non-retryable close propagates unchanged and never reconnects.
    let auth = ClusterError::Provider {
        kind: ProviderErrorKind::AuthFailure,
        message: "bad credentials".to_owned(),
    };
    tx.send(CacheWatchEvent::Closed(auth)).await.expect("send");
    assert!(matches!(
        restarting.recv().await,
        Some(CacheWatchEvent::Closed(_))
    ));
    assert!(
        metrics.resets.lock().expect("lock").is_empty(),
        "a non-retryable close must not record a watch reset"
    );
}

#[tokio::test(start_paused = true)]
async fn non_retryable_close_propagates_then_terminates() {
    let (fresh_tx, fresh_watch) = CacheWatch::channel(8);
    let _keep = fresh_tx; // would-be reconnect target; must never be used
    let (tx, watch) = cache_watch_with_resubscribes(vec![fresh_watch]);
    let mut restarting = watch.auto_restart(RetryPolicy::default());

    let auth = ClusterError::Provider {
        kind: ProviderErrorKind::AuthFailure,
        message: "bad credentials".to_owned(),
    };
    tx.send(CacheWatchEvent::Closed(auth))
        .await
        .expect("send close");

    assert!(matches!(
        restarting.recv().await,
        Some(CacheWatchEvent::Closed(ClusterError::Provider {
            kind: ProviderErrorKind::AuthFailure,
            ..
        }))
    ));
    // Terminal thereafter.
    assert!(restarting.recv().await.is_none());
}

#[tokio::test(start_paused = true)]
async fn shutdown_close_propagates_unchanged() {
    let (tx, watch) = cache_watch_with_resubscribes(vec![]);
    let mut restarting = watch.auto_restart(RetryPolicy::default());
    tx.send(CacheWatchEvent::Closed(ClusterError::Shutdown))
        .await
        .expect("send");
    assert!(matches!(
        restarting.recv().await,
        Some(CacheWatchEvent::Closed(ClusterError::Shutdown))
    ));
    assert!(restarting.recv().await.is_none());
}

#[tokio::test(start_paused = true)]
async fn exhausted_retry_cap_propagates_most_recent_close() {
    // Resubscribe always fails (empty follow-up queue), so the cap is hit.
    let (tx, watch) = cache_watch_with_resubscribes(vec![]);
    let mut restarting = watch.auto_restart(RetryPolicy {
        max_retries: Some(3),
        ..RetryPolicy::default()
    });
    tx.send(CacheWatchEvent::Closed(retryable_close()))
        .await
        .expect("send");
    drop(tx);

    assert!(matches!(
        restarting.recv().await,
        Some(CacheWatchEvent::Closed(ClusterError::Provider {
            kind: ProviderErrorKind::ConnectionLost,
            ..
        }))
    ));
    assert!(restarting.recv().await.is_none());
}

#[tokio::test(start_paused = true)]
async fn none_on_resubscribable_watch_reconnects_and_synthesizes_reset() {
    // The underlying stream ends with `None` — the sender is dropped without a
    // terminal `Closed`, the canonical transient remote-backend sender-drop. A
    // watch carrying a resubscribe seam must treat it as a retryable close:
    // reconnect with backoff and synthesize a Reset, not die permanently.
    let (fresh_tx, fresh_watch) = CacheWatch::channel(8);
    let (tx, watch) = cache_watch_with_resubscribes(vec![fresh_watch]);
    let mut restarting = watch.auto_restart(RetryPolicy::default());

    // Drop the sender with no Closed -> the inner stream yields None.
    drop(tx);

    assert!(matches!(
        restarting.recv().await,
        Some(CacheWatchEvent::Reset)
    ));
    // Events flow from the reconnected subscription.
    fresh_tx
        .send(CacheWatchEvent::Event(CacheEvent::Changed {
            key: "k".to_owned(),
        }))
        .await
        .expect("send event");
    assert!(matches!(
        restarting.recv().await,
        Some(CacheWatchEvent::Event(CacheEvent::Changed { .. }))
    ));
}

#[tokio::test(start_paused = true)]
async fn none_on_seamless_watch_terminates_without_reconnecting() {
    // A bare channel() watch has no resubscribe seam: a stream that ends in None
    // genuinely ended, so the combinator terminates rather than reconnecting.
    let (tx, watch) = CacheWatch::channel(8);
    let mut restarting = watch.auto_restart(RetryPolicy::default());
    drop(tx);
    assert!(restarting.recv().await.is_none());
    // Terminal thereafter.
    assert!(restarting.recv().await.is_none());
}

#[tokio::test(start_paused = true)]
async fn seamless_watch_propagates_retryable_close_unchanged() {
    // A bare channel() watch has no resubscribe seam — even a retryable close
    // is propagated unchanged (Option A behaviour).
    let (tx, watch) = CacheWatch::channel(8);
    let mut restarting = watch.auto_restart(RetryPolicy::default());
    tx.send(CacheWatchEvent::Closed(retryable_close()))
        .await
        .expect("send");
    assert!(matches!(
        restarting.recv().await,
        Some(CacheWatchEvent::Closed(ClusterError::Provider {
            kind: ProviderErrorKind::ConnectionLost,
            ..
        }))
    ));
    assert!(restarting.recv().await.is_none());
}

#[tokio::test(start_paused = true)]
async fn failed_resubscribe_retries_until_success() {
    // First resubscribe attempt fails (sentinel error), second succeeds.
    let (fresh_tx, fresh_watch) = CacheWatch::channel(8);
    let (tx, mut watch) = CacheWatch::channel(8);
    let attempts = Arc::new(AtomicU32::new(0));
    let slot = Arc::new(std::sync::Mutex::new(Some(fresh_watch)));
    let attempts_c = Arc::clone(&attempts);
    watch.set_resubscribe(move || {
        let n = attempts_c.fetch_add(1, Ordering::SeqCst);
        let slot = Arc::clone(&slot);
        Box::pin(async move {
            if n == 0 {
                Err(ClusterError::Provider {
                    kind: ProviderErrorKind::Timeout,
                    message: "still down".to_owned(),
                })
            } else {
                slot.lock()
                    .expect("slot")
                    .take()
                    .ok_or(ClusterError::Shutdown)
            }
        })
    });
    let mut restarting = watch.auto_restart(RetryPolicy::default());
    tx.send(CacheWatchEvent::Closed(retryable_close()))
        .await
        .expect("send");
    drop(tx);

    assert!(matches!(
        restarting.recv().await,
        Some(CacheWatchEvent::Reset)
    ));
    assert_eq!(
        attempts.load(Ordering::SeqCst),
        2,
        "one failed then one successful attempt"
    );

    fresh_tx
        .send(CacheWatchEvent::Event(CacheEvent::Deleted {
            key: "k".to_owned(),
        }))
        .await
        .expect("send event");
    assert!(matches!(
        restarting.recv().await,
        Some(CacheWatchEvent::Event(CacheEvent::Deleted { .. }))
    ));
}

#[tokio::test(start_paused = true)]
async fn service_watch_auto_restart_reconnects() {
    let (fresh_tx, fresh_watch) = ServiceWatch::channel(8);
    let (tx, mut watch) = ServiceWatch::channel(8);
    let slot = Arc::new(std::sync::Mutex::new(Some(fresh_watch)));
    watch.set_resubscribe(move || {
        let slot = Arc::clone(&slot);
        Box::pin(async move {
            slot.lock()
                .expect("slot")
                .take()
                .ok_or(ClusterError::Shutdown)
        })
    });
    let mut restarting = watch.auto_restart(RetryPolicy::default());
    tx.send(ServiceWatchEvent::Closed(retryable_close()))
        .await
        .expect("send");
    drop(tx);

    assert!(matches!(
        restarting.recv().await,
        Some(ServiceWatchEvent::Reset)
    ));
    fresh_tx.send(ServiceWatchEvent::Reset).await.expect("send");
    assert!(matches!(
        restarting.recv().await,
        Some(ServiceWatchEvent::Reset)
    ));
}

#[tokio::test(start_paused = true)]
async fn leader_watch_auto_restart_reconnects_and_forwards_gate_reads() {
    let (fresh_tx, _fresh_resign, fresh_watch) = LeaderWatch::channel(8, LeaderStatus::Follower);
    let (tx, _resign, mut watch) = LeaderWatch::channel(8, LeaderStatus::Follower);
    let slot = Arc::new(std::sync::Mutex::new(Some(fresh_watch)));
    watch.set_resubscribe(move || {
        let slot = Arc::clone(&slot);
        Box::pin(async move {
            slot.lock()
                .expect("slot")
                .take()
                .ok_or(ClusterError::Shutdown)
        })
    });
    let mut restarting = watch.auto_restart(RetryPolicy::default());

    tx.send(LeaderWatchEvent::Closed(retryable_close()))
        .await
        .expect("send");
    assert!(matches!(
        restarting.recv().await,
        Some(LeaderWatchEvent::Reset)
    ));

    // Gate reads now reflect the reconnected subscription.
    fresh_tx
        .send_status(LeaderStatus::Leader)
        .await
        .expect("send status");
    assert!(matches!(
        restarting.recv().await,
        Some(LeaderWatchEvent::Status(LeaderStatus::Leader))
    ));
    assert!(restarting.is_leader());
    assert_eq!(restarting.status(), LeaderStatus::Leader);
}
