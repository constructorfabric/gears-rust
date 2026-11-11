//! The key's mutation lock over `toolkit-db`'s file-marker lock on `SQLite`.
//!
//! The markers live under the user's cache directory, shared by every test
//! process, so each test locks keys of its own. The `PostgreSQL` binding of the
//! same lock is exercised by `tests/pg_activation_state_test.rs`.

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use uuid::Uuid;

use super::{DeliveryScanLock, Held, LockSchedule, MutationLock, Releaser, clear_startup_markers};
use crate::domain::activation::testing::{LogBuf, RecordingLockMetrics};
use crate::domain::activation::{HeldKey, KeyLocks, ScanLock};
use crate::domain::error::DomainError;
use crate::test_support::sqlite_provider;

fn unique_key() -> String {
    format!("activation-test-{}", Uuid::new_v4())
}

/// Short polls, so a test that waits for a held lock waits for little.
const QUICK: LockSchedule = LockSchedule {
    base_ms: 1,
    factor: 4,
    cap: Duration::from_millis(20),
    jitter: false,
};

async fn lock(budget: Duration) -> (MutationLock, Arc<RecordingLockMetrics>) {
    let metrics = Arc::new(RecordingLockMetrics::default());
    let lock =
        MutationLock::new(sqlite_provider().await, budget, metrics.clone()).with_schedule(QUICK);
    (lock, metrics)
}

#[tokio::test]
async fn the_default_schedule_starts_at_tens_of_milliseconds_doubles_and_caps_at_one_second() {
    // The configuration `toolkit-db` receives, not the gear's own struct.
    let metrics = Arc::new(RecordingLockMetrics::default());
    let lock = MutationLock::new(sqlite_provider().await, Duration::from_secs(10), metrics);
    let config = lock.config(None);
    assert!(config.jitter, "waiters are jittered");
    assert_eq!(config.backoff_base_ms, 2);
    assert_eq!(config.backoff_factor, 25);
    assert_eq!(config.max_backoff, Duration::from_secs(1));
    // `delay(n) = factor * base^n`, in the retry vocabulary `toolkit-db` takes.
    let delays: Vec<u64> = (1..=7)
        .map(|n| {
            (config.backoff_factor * config.backoff_base_ms.pow(n))
                .min(u64::try_from(config.max_backoff.as_millis()).expect("fits"))
        })
        .collect();
    assert_eq!(delays, vec![50, 100, 200, 400, 800, 1000, 1000]);
}

#[tokio::test]
async fn the_wait_is_the_budget_capped_by_the_request_deadline_and_the_schedule_is_injected() {
    let (lock, _) = lock(Duration::from_secs(10)).await;
    assert_eq!(lock.wait_for(None), Duration::from_secs(10));
    assert_eq!(
        lock.wait_for(Some(Duration::from_millis(250))),
        Duration::from_millis(250),
        "a batch's remaining deadline caps the budget"
    );
    assert_eq!(
        lock.wait_for(Some(Duration::from_secs(3600))),
        Duration::from_secs(10),
        "and never lengthens it"
    );
    assert_eq!(lock.wait_for(Some(Duration::ZERO)), Duration::ZERO);

    let config = lock.config(Some(Duration::from_millis(250)));
    assert_eq!(config.max_wait, Some(Duration::from_millis(250)));
    assert_eq!(
        (config.backoff_base_ms, config.backoff_factor, config.jitter),
        (QUICK.base_ms, QUICK.factor, QUICK.jitter),
        "the injected schedule is the one handed on"
    );
    assert_eq!(config.max_backoff, QUICK.cap);
    assert_eq!(config.max_retries, None, "bounded by the budget alone");
}

#[tokio::test]
async fn an_acquisition_is_counted_and_a_release_frees_the_key() {
    let (lock, metrics) = lock(Duration::from_millis(200)).await;
    let key = unique_key();

    let held = lock.acquire(&key, None).await.expect("a free key");
    assert_eq!(metrics.acquired.lock().expect("metrics").len(), 1);
    held.release().await;

    let again = lock
        .acquire(&key, None)
        .await
        .expect("free again after the release");
    again.release().await;
    assert_eq!(metrics.acquired.lock().expect("metrics").len(), 2);
    assert!(metrics.exhausted.lock().expect("metrics").is_empty());
    assert_eq!(metrics.errors.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn a_wait_that_runs_out_is_unavailable_counted_exhausted_and_leaves_the_owner_its_lock() {
    let (lock, metrics) = lock(Duration::from_millis(60)).await;
    let key = unique_key();
    let owner = lock.acquire(&key, None).await.expect("the owner");

    let started = Instant::now();
    let outcome = lock.acquire(&key, None).await;

    assert!(
        matches!(outcome, Err(DomainError::Unavailable { .. })),
        "{:?}",
        outcome.as_ref().err()
    );
    assert!(
        started.elapsed() >= Duration::from_millis(50),
        "it waited its budget"
    );
    assert_eq!(metrics.exhausted.lock().expect("metrics").len(), 1);
    assert!(
        metrics.exhausted.lock().expect("metrics")[0] >= Duration::from_millis(50),
        "the wait is observed"
    );
    // Still held by its owner: another attempt is refused too.
    assert!(lock.acquire(&key, Some(Duration::ZERO)).await.is_err());
    owner.release().await;
    lock.acquire(&key, None)
        .await
        .expect("the owner's release frees it")
        .release()
        .await;
}

#[tokio::test]
async fn a_waiter_gets_the_lock_once_its_owner_lets_go_within_the_budget() {
    let (lock, metrics) = lock(Duration::from_secs(5)).await;
    let key = unique_key();
    let owner = lock.acquire(&key, None).await.expect("the owner");
    let release = async {
        tokio::time::sleep(Duration::from_millis(80)).await;
        owner.release().await;
    };
    let (waited, ()) = tokio::join!(lock.acquire(&key, None), release);
    waited.expect("acquired after the release").release().await;
    let waits = metrics.acquired.lock().expect("metrics").clone();
    assert_eq!(waits.len(), 2);
    assert!(
        waits[1] >= Duration::from_millis(40),
        "the wait is observed: {waits:?}"
    );
}

#[tokio::test]
async fn two_keys_never_share_a_lock() {
    let (lock, _) = lock(Duration::from_millis(50)).await;
    let (one, two) = (unique_key(), unique_key());
    let first = lock.acquire(&one, None).await.expect("first");
    let second = lock
        .acquire(&two, None)
        .await
        .expect("a held key blocks no other");
    first.release().await;
    second.release().await;
}

struct FailingRelease;

#[async_trait]
impl Releaser for FailingRelease {
    async fn release(self: Box<Self>) -> Result<(), String> {
        Err("the unlock found the lock not held".to_owned())
    }
}

#[tokio::test]
async fn a_failed_unlock_warns_once_and_counts_an_error_and_is_not_raised() {
    let logs = LogBuf::default();
    let _guard = tracing::subscriber::set_default(
        tracing_subscriber::fmt()
            .with_writer(logs.clone())
            .with_ansi(false)
            .finish(),
    );
    let metrics = Arc::new(RecordingLockMetrics::default());
    let key = unique_key();
    let held: Box<dyn HeldKey> = Box::new(Held {
        key: key.clone(),
        guard: Box::new(FailingRelease),
        metrics: metrics.clone(),
    });
    held.release().await;
    assert_eq!(metrics.errors.load(Ordering::SeqCst), 1);
    let text = logs.text();
    let warns: Vec<&str> = text.lines().filter(|l| l.contains("WARN")).collect();
    assert_eq!(warns.len(), 1, "{text}");
    assert!(warns[0].contains(&key), "the warning names the key: {text}");
}

#[tokio::test]
async fn start_up_removes_the_stale_markers_of_the_scan_lock_and_every_declared_key_on_sqlite() {
    let db = sqlite_provider().await;
    let metrics = Arc::new(RecordingLockMetrics::default());
    let lock =
        MutationLock::new(Arc::clone(&db), Duration::from_millis(30), metrics).with_schedule(QUICK);
    let key = unique_key();
    // A crash: the holder is gone and the marker stays behind.
    for stale in [key.clone(), super::SCAN_LOCK_KEY.to_owned()] {
        let held = lock.acquire(&stale, None).await.expect("held, then lost");
        std::mem::forget(held);
    }
    assert!(
        lock.acquire(&key, None).await.is_err(),
        "the stale marker blocks"
    );

    let removed = clear_startup_markers(&db, [key.clone()])
        .await
        .expect("cleared");

    // The declared key's marker is this test's own; the scan lock's is one file
    // shared by every process, so a concurrent gear init may have removed it first.
    assert!(
        (1..=2).contains(&removed),
        "the declared key's marker and, unless another init took it, the scan lock's: {removed}"
    );
    for freed in [key.as_str(), super::SCAN_LOCK_KEY] {
        lock.acquire(freed, None)
            .await
            .unwrap_or_else(|e| panic!("`{freed}` is free after the start-up removal: {e:?}"))
            .release()
            .await;
    }
}

#[test]
fn the_scan_lock_is_one_attempt_with_no_retry_and_no_wait() {
    let config = DeliveryScanLock::config();
    assert_eq!(config.max_retries, Some(0));
    assert_eq!(config.max_wait, None);
}

#[tokio::test]
async fn a_held_scan_lock_answers_none_at_once_and_a_release_frees_it() {
    let db = sqlite_provider().await;
    let key = unique_key();
    let one = DeliveryScanLock::named(Arc::clone(&db), key.clone());
    let other = DeliveryScanLock::named(db, key);

    let held = one.try_acquire().await.expect("attempted").expect("free");
    let started = Instant::now();
    assert!(
        other.try_acquire().await.expect("attempted").is_none(),
        "another replica's single attempt fails while the lock is held"
    );
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "the attempt did not wait"
    );
    held.release().await;
    let again = other.try_acquire().await.expect("attempted");
    assert!(again.is_some(), "a released scan lock is free again");
    again.expect("held").release().await;
}
