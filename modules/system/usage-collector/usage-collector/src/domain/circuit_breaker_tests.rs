#![allow(clippy::let_underscore_must_use)]

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use usage_collector_sdk::UsageRecordError;

use super::CircuitBreaker;
use crate::config::CircuitBreakerConfig;
use crate::domain::DomainError;

fn cb(threshold: u32, window: Duration, recovery: Duration) -> CircuitBreaker {
    CircuitBreaker::new(CircuitBreakerConfig {
        failure_threshold: threshold,
        window,
        recovery_timeout: recovery,
    })
}

fn unavailable() -> DomainError {
    DomainError::PluginUnavailable {
        gts_id: "test".to_owned(),
        reason: "simulated".to_owned(),
    }
}

fn caller_error() -> DomainError {
    DomainError::Plugin(
        UsageRecordError::invalid_argument()
            .with_format("bad input")
            .create(),
    )
}

#[tokio::test]
async fn closed_circuit_passes_calls_through() {
    let cb = cb(2, Duration::from_secs(10), Duration::from_millis(1));
    let result: Result<u32, DomainError> = cb.execute(|| async { Ok(42) }).await;
    assert_eq!(result.unwrap(), 42);
}

#[tokio::test]
async fn opens_after_threshold_failures() {
    let cb = cb(2, Duration::from_secs(10), Duration::from_millis(1));

    for _ in 0..2 {
        let _ = cb.execute(|| async { Err::<(), _>(unavailable()) }).await;
    }

    // Next call must be rejected without invoking the closure.
    let invoked = Arc::new(AtomicUsize::new(0));
    let invoked_c = Arc::clone(&invoked);
    let err = cb
        .execute(move || {
            let invoked = Arc::clone(&invoked_c);
            async move {
                invoked.fetch_add(1, Ordering::SeqCst);
                Ok::<(), DomainError>(())
            }
        })
        .await
        .unwrap_err();

    assert!(matches!(err, DomainError::CircuitOpen));
    assert_eq!(invoked.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn caller_errors_do_not_open_the_circuit() {
    let cb = cb(2, Duration::from_secs(10), Duration::from_millis(1));

    for _ in 0..5 {
        let _ = cb.execute(|| async { Err::<(), _>(caller_error()) }).await;
    }

    // Circuit must still be closed: a successful call goes through.
    let result: Result<u32, DomainError> = cb.execute(|| async { Ok(1) }).await;
    assert_eq!(result.unwrap(), 1);
}

#[tokio::test]
async fn deadline_exceeded_counts_as_failure() {
    let cb = cb(2, Duration::from_secs(10), Duration::from_millis(1));

    for _ in 0..2 {
        let _ = cb
            .execute(|| async { Err::<(), _>(DomainError::Timeout) })
            .await;
    }

    let err = cb
        .execute(|| async { Ok::<(), DomainError>(()) })
        .await
        .unwrap_err();
    assert!(matches!(err, DomainError::CircuitOpen));
}

#[tokio::test]
async fn internal_error_counts_as_failure() {
    let cb = cb(2, Duration::from_secs(10), Duration::from_millis(1));

    for _ in 0..2 {
        let _ = cb
            .execute(|| async { Err::<(), _>(DomainError::Internal("boom".to_owned())) })
            .await;
    }

    let err = cb
        .execute(|| async { Ok::<(), DomainError>(()) })
        .await
        .unwrap_err();
    assert!(matches!(err, DomainError::CircuitOpen));
}

#[tokio::test]
async fn successful_probe_closes_circuit() {
    let cb = cb(1, Duration::from_secs(10), Duration::from_millis(1));

    let _ = cb.execute(|| async { Err::<(), _>(unavailable()) }).await;

    tokio::time::sleep(Duration::from_millis(5)).await;

    cb.execute(|| async { Ok::<(), DomainError>(()) })
        .await
        .expect("probe should succeed");

    cb.execute(|| async { Ok::<(), DomainError>(()) })
        .await
        .expect("circuit should be closed after successful probe");
}

#[tokio::test]
async fn failed_probe_reopens_circuit() {
    let cb = cb(1, Duration::from_secs(10), Duration::from_millis(1));

    let _ = cb.execute(|| async { Err::<(), _>(unavailable()) }).await;

    tokio::time::sleep(Duration::from_millis(5)).await;

    let _ = cb.execute(|| async { Err::<(), _>(unavailable()) }).await;

    let invoked = Arc::new(AtomicUsize::new(0));
    let invoked_c = Arc::clone(&invoked);
    let err = cb
        .execute(move || {
            let invoked = Arc::clone(&invoked_c);
            async move {
                invoked.fetch_add(1, Ordering::SeqCst);
                Ok::<(), DomainError>(())
            }
        })
        .await
        .unwrap_err();

    assert!(matches!(err, DomainError::CircuitOpen));
    assert_eq!(invoked.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn probe_with_caller_error_reopens_circuit() {
    // HalfOpen is strict: any non-success during the probe (including caller-induced)
    // re-opens the circuit, because the probe's job is to confirm health.
    let cb = cb(1, Duration::from_secs(10), Duration::from_millis(1));

    let _ = cb.execute(|| async { Err::<(), _>(unavailable()) }).await;

    tokio::time::sleep(Duration::from_millis(5)).await;

    let _ = cb.execute(|| async { Err::<(), _>(caller_error()) }).await;

    let err = cb
        .execute(|| async { Ok::<(), DomainError>(()) })
        .await
        .unwrap_err();
    assert!(matches!(err, DomainError::CircuitOpen));
}

#[tokio::test]
async fn failed_probe_reopens_under_default_threshold() {
    // Regression: with threshold > 1, a single failed probe must still
    // re-open the circuit. The rolling-window counter is cleared when the
    // breaker enters Open, so a probe failure on its own can never satisfy
    // `failures_in_window >= threshold` — without an unconditional re-open
    // for HalfOpen the breaker would stay stuck forever.
    let cb = cb(5, Duration::from_secs(10), Duration::from_millis(1));

    for _ in 0..5 {
        let _ = cb.execute(|| async { Err::<(), _>(unavailable()) }).await;
    }

    tokio::time::sleep(Duration::from_millis(5)).await;
    let _ = cb.execute(|| async { Err::<(), _>(unavailable()) }).await;

    // The breaker must now reject calls (Open), not stall in HalfOpen.
    let invoked = Arc::new(AtomicUsize::new(0));
    let invoked_c = Arc::clone(&invoked);
    let err = cb
        .execute(move || {
            let invoked = Arc::clone(&invoked_c);
            async move {
                invoked.fetch_add(1, Ordering::SeqCst);
                Ok::<(), DomainError>(())
            }
        })
        .await
        .unwrap_err();
    assert!(matches!(err, DomainError::CircuitOpen));
    assert_eq!(invoked.load(Ordering::SeqCst), 0);

    // After another recovery window a successful probe must restore the breaker.
    tokio::time::sleep(Duration::from_millis(5)).await;
    cb.execute(|| async { Ok::<(), DomainError>(()) })
        .await
        .expect("probe should succeed and close the breaker");
    cb.execute(|| async { Ok::<(), DomainError>(()) })
        .await
        .expect("breaker should be usable after recovery");
}

#[tokio::test]
async fn half_open_admits_only_one_concurrent_probe() {
    let cb = Arc::new(cb(1, Duration::from_secs(10), Duration::from_millis(1)));

    let _ = cb.execute(|| async { Err::<(), _>(unavailable()) }).await;

    tokio::time::sleep(Duration::from_millis(5)).await;

    let invoked = Arc::new(AtomicUsize::new(0));
    let mut set = tokio::task::JoinSet::new();
    for _ in 0..3 {
        let cb = Arc::clone(&cb);
        let invoked = Arc::clone(&invoked);
        set.spawn(async move {
            cb.execute(move || {
                let invoked = Arc::clone(&invoked);
                async move {
                    invoked.fetch_add(1, Ordering::SeqCst);
                    // Yield so other tasks see HalfOpen before we close it.
                    tokio::task::yield_now().await;
                    Ok::<(), DomainError>(())
                }
            })
            .await
        });
    }

    let mut ok = 0usize;
    let mut rejected = 0usize;
    while let Some(res) = set.join_next().await {
        match res.unwrap() {
            Ok(()) => ok += 1,
            Err(DomainError::CircuitOpen) => rejected += 1,
            Err(other) => panic!("unexpected error: {other:?}"),
        }
    }

    assert_eq!(invoked.load(Ordering::SeqCst), 1);
    assert_eq!(ok, 1);
    assert_eq!(rejected, 2);
}
