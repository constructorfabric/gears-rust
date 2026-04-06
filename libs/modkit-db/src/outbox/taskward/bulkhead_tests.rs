use super::*;

fn test_config(initial: Duration, max: Duration, multiplier: f64) -> BulkheadConfig {
    BulkheadConfig {
        semaphore: ConcurrencyLimit::Unlimited,
        backoff: BackoffConfig {
            initial,
            max,
            multiplier,
            jitter: 0.0,
        },
    }
}

// -- BackoffConfig validation tests --

#[test]
fn new_valid_config() {
    let cfg = BackoffConfig::new(Duration::from_secs(1), Duration::from_secs(60), 2.0);
    assert_eq!(cfg.initial, Duration::from_secs(1));
    assert_eq!(cfg.max, Duration::from_secs(60));
    assert!((cfg.multiplier - 2.0).abs() < f64::EPSILON);
    assert!((cfg.jitter - 0.3).abs() < f64::EPSILON);
}

#[test]
fn default_backoff_config() {
    let cfg = BackoffConfig::default();
    assert_eq!(cfg.initial, Duration::from_millis(100));
    assert_eq!(cfg.max, Duration::from_secs(30));
    assert!((cfg.multiplier - 2.0).abs() < f64::EPSILON);
    assert!((cfg.jitter - 0.3).abs() < f64::EPSILON);
}

#[test]
fn struct_update_syntax_with_default() {
    let cfg = BackoffConfig {
        max: Duration::from_secs(60),
        ..Default::default()
    };
    assert_eq!(cfg.initial, Duration::from_millis(100));
    assert_eq!(cfg.max, Duration::from_secs(60));
    assert!((cfg.multiplier - 2.0).abs() < f64::EPSILON);
}

#[test]
#[should_panic(expected = "multiplier must be >= 1.0")]
fn new_panics_on_low_multiplier() {
    let _cfg = BackoffConfig::new(Duration::from_secs(1), Duration::from_secs(60), 0.5);
}

#[test]
#[should_panic(expected = "initial")]
fn new_panics_on_initial_exceeds_max() {
    let _cfg = BackoffConfig::new(Duration::from_secs(60), Duration::from_secs(1), 2.0);
}

// -- Escalation tests (jitter=0.0 for deterministic) --

#[test]
fn escalate_first() {
    let mut bh = Bulkhead::new(
        "test",
        test_config(Duration::from_secs(1), Duration::from_secs(60), 2.0),
    );
    bh.escalate();
    assert_eq!(bh.consecutive_failures(), 1);
    assert_eq!(bh.min_interval(), Duration::from_secs(1));
}

#[test]
fn escalate_exponential() {
    let mut bh = Bulkhead::new(
        "test",
        test_config(Duration::from_secs(1), Duration::from_secs(60), 2.0),
    );
    let expected = [1, 2, 4, 8];
    for &exp in &expected {
        bh.escalate();
        assert_eq!(bh.min_interval(), Duration::from_secs(exp));
    }
}

#[test]
fn escalate_caps_at_max() {
    let mut bh = Bulkhead::new(
        "test",
        test_config(Duration::from_secs(1), Duration::from_secs(30), 2.0),
    );
    for _ in 0..10 {
        bh.escalate();
    }
    assert_eq!(bh.min_interval(), Duration::from_secs(30));
}

#[test]
fn escalate_linear() {
    let mut bh = Bulkhead::new(
        "test",
        test_config(Duration::from_secs(5), Duration::from_secs(5), 1.0),
    );
    for _ in 0..3 {
        bh.escalate();
        assert_eq!(bh.min_interval(), Duration::from_secs(5));
    }
}

// -- Reset tests --

#[test]
fn reset_clears_state() {
    let mut bh = Bulkhead::new(
        "test",
        test_config(Duration::from_secs(1), Duration::from_secs(60), 2.0),
    );
    for _ in 0..3 {
        bh.escalate();
    }
    bh.reset();
    assert_eq!(bh.consecutive_failures(), 0);
    assert_eq!(bh.min_interval(), Duration::ZERO);
}

#[test]
fn reset_on_fresh() {
    let mut bh = Bulkhead::default();
    bh.reset();
    assert_eq!(bh.consecutive_failures(), 0);
    assert_eq!(bh.min_interval(), Duration::ZERO);
}

#[test]
fn default_bulkhead() {
    let bh = Bulkhead::default();
    assert_eq!(bh.min_interval(), Duration::ZERO);
    assert!(matches!(bh.semaphore, ConcurrencyLimit::Unlimited));
}

// -- consecutive_failures getter tests --

#[test]
fn consecutive_failures_fresh_is_zero() {
    let bh = Bulkhead::default();
    assert_eq!(bh.consecutive_failures(), 0);
}

#[test]
fn consecutive_failures_after_escalation() {
    let mut bh = Bulkhead::new(
        "test",
        test_config(Duration::from_secs(1), Duration::from_secs(60), 2.0),
    );
    bh.escalate();
    bh.escalate();
    bh.escalate();
    assert_eq!(bh.consecutive_failures(), 3);
}

#[test]
fn consecutive_failures_after_reset() {
    let mut bh = Bulkhead::new(
        "test",
        test_config(Duration::from_secs(1), Duration::from_secs(60), 2.0),
    );
    bh.escalate();
    bh.escalate();
    bh.reset();
    assert_eq!(bh.consecutive_failures(), 0);
}

// -- Jitter tests --

#[test]
fn jitter_varies_interval_within_bounds() {
    let config = BulkheadConfig {
        semaphore: ConcurrencyLimit::Unlimited,
        backoff: BackoffConfig {
            initial: Duration::from_secs(10),
            max: Duration::from_secs(60),
            multiplier: 1.0,
            jitter: 0.1,
        },
    };
    let mut bh = Bulkhead::new("test-worker", config);
    bh.escalate();
    let interval = bh.min_interval();
    // ±10% of 10s = [9s, 11s]
    assert!(
        interval >= Duration::from_secs(9) && interval <= Duration::from_secs(11),
        "interval {interval:?} should be in [9s, 11s]"
    );
}

#[test]
fn zero_jitter_is_deterministic() {
    let mut bh = Bulkhead::new(
        "test",
        test_config(Duration::from_secs(1), Duration::from_secs(60), 2.0),
    );
    bh.escalate();
    assert_eq!(bh.min_interval(), Duration::from_secs(1));
    bh.escalate();
    assert_eq!(bh.min_interval(), Duration::from_secs(2));
}

#[test]
fn jitter_does_not_exceed_max() {
    let config = BulkheadConfig {
        semaphore: ConcurrencyLimit::Unlimited,
        backoff: BackoffConfig {
            initial: Duration::from_secs(28),
            max: Duration::from_secs(30),
            multiplier: 1.0,
            jitter: 0.2,
        },
    };
    let mut bh = Bulkhead::new("test-worker", config);
    // Even with +20% jitter on 28s = 33.6s, it should be capped at 30s
    for _ in 0..10 {
        bh.escalate();
        assert!(
            bh.min_interval() <= Duration::from_secs(30),
            "interval {:?} should not exceed max 30s",
            bh.min_interval()
        );
        bh.reset();
    }
}

// -- Acquire tests --

#[tokio::test]
async fn acquire_with_permit() {
    let sem = Arc::new(Semaphore::new(1));
    let bh = Bulkhead::new(
        "test",
        BulkheadConfig {
            semaphore: ConcurrencyLimit::Fixed(sem),
            backoff: BackoffConfig {
                initial: Duration::ZERO,
                max: Duration::ZERO,
                multiplier: 1.0,
                jitter: 0.0,
            },
        },
    );
    let cancel = CancellationToken::new();
    assert!(bh.acquire(&cancel).await.is_some());
}

#[tokio::test]
async fn acquire_cancel_during_wait() {
    let sem = Arc::new(Semaphore::new(0));
    let bh = Bulkhead::new(
        "test",
        BulkheadConfig {
            semaphore: ConcurrencyLimit::Fixed(sem),
            backoff: BackoffConfig {
                initial: Duration::ZERO,
                max: Duration::ZERO,
                multiplier: 1.0,
                jitter: 0.0,
            },
        },
    );
    let cancel = CancellationToken::new();
    let cancel_c = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(10)).await;
        cancel_c.cancel();
    });
    assert!(bh.acquire(&cancel).await.is_none());
}

#[tokio::test]
async fn acquire_already_cancelled() {
    let sem = Arc::new(Semaphore::new(1));
    let bh = Bulkhead::new(
        "test",
        BulkheadConfig {
            semaphore: ConcurrencyLimit::Fixed(sem),
            backoff: BackoffConfig {
                initial: Duration::ZERO,
                max: Duration::ZERO,
                multiplier: 1.0,
                jitter: 0.0,
            },
        },
    );
    let cancel = CancellationToken::new();
    cancel.cancel();
    assert!(bh.acquire(&cancel).await.is_none());
}

#[tokio::test]
async fn acquire_closed_semaphore() {
    let sem = Arc::new(Semaphore::new(1));
    sem.close();
    let bh = Bulkhead::new(
        "test",
        BulkheadConfig {
            semaphore: ConcurrencyLimit::Fixed(sem),
            backoff: BackoffConfig {
                initial: Duration::ZERO,
                max: Duration::ZERO,
                multiplier: 1.0,
                jitter: 0.0,
            },
        },
    );
    let cancel = CancellationToken::new();
    assert!(bh.acquire(&cancel).await.is_none());
}

#[tokio::test]
async fn acquire_no_semaphore() {
    let bh = Bulkhead::default();
    let cancel = CancellationToken::new();
    assert!(bh.acquire(&cancel).await.is_some());
}

// -- Priority mode tests --

fn priority_bulkhead(guaranteed: Arc<Semaphore>, shared: Arc<Semaphore>) -> Bulkhead {
    Bulkhead::new(
        "test",
        BulkheadConfig {
            semaphore: ConcurrencyLimit::Tiered { guaranteed, shared },
            backoff: BackoffConfig {
                initial: Duration::ZERO,
                max: Duration::ZERO,
                multiplier: 1.0,
                jitter: 0.0,
            },
        },
    )
}

#[tokio::test]
async fn priority_prefers_shared_when_both_available() {
    let guaranteed = Arc::new(Semaphore::new(1));
    let shared = Arc::new(Semaphore::new(1));
    let bh = priority_bulkhead(Arc::clone(&guaranteed), Arc::clone(&shared));
    let cancel = CancellationToken::new();

    let permit = bh.acquire(&cancel).await;
    assert!(permit.is_some());
    // shared should have been consumed (biased select prefers it)
    assert_eq!(shared.available_permits(), 0);
    assert_eq!(guaranteed.available_permits(), 1);
}

#[tokio::test]
async fn priority_falls_back_to_guaranteed_when_shared_exhausted() {
    let guaranteed = Arc::new(Semaphore::new(1));
    let shared = Arc::new(Semaphore::new(0));
    let bh = priority_bulkhead(Arc::clone(&guaranteed), Arc::clone(&shared));
    let cancel = CancellationToken::new();

    let permit = bh.acquire(&cancel).await;
    assert!(permit.is_some());
    assert_eq!(guaranteed.available_permits(), 0);
}

#[tokio::test]
async fn priority_acquires_shared_when_guaranteed_exhausted() {
    let guaranteed = Arc::new(Semaphore::new(0));
    let shared = Arc::new(Semaphore::new(1));
    let bh = priority_bulkhead(Arc::clone(&guaranteed), Arc::clone(&shared));
    let cancel = CancellationToken::new();

    let permit = bh.acquire(&cancel).await;
    assert!(permit.is_some());
    assert_eq!(shared.available_permits(), 0);
}

#[tokio::test(start_paused = true)]
async fn priority_cancel_during_wait() {
    let guaranteed = Arc::new(Semaphore::new(0));
    let shared = Arc::new(Semaphore::new(0));
    let bh = priority_bulkhead(guaranteed, shared);
    let cancel = CancellationToken::new();
    let cancel_c = cancel.clone();

    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(10)).await;
        cancel_c.cancel();
    });
    assert!(bh.acquire(&cancel).await.is_none());
}

#[tokio::test(start_paused = true)]
async fn priority_blocks_when_neither_available() {
    let guaranteed = Arc::new(Semaphore::new(0));
    let shared = Arc::new(Semaphore::new(0));
    let bh = priority_bulkhead(Arc::clone(&guaranteed), Arc::clone(&shared));
    let cancel = CancellationToken::new();

    // Release a guaranteed permit after a short delay
    let g = Arc::clone(&guaranteed);
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(20)).await;
        g.add_permits(1);
    });

    let permit = tokio::time::timeout(Duration::from_millis(100), bh.acquire(&cancel)).await;
    assert!(permit.is_ok());
    assert!(permit.unwrap().is_some());
}
