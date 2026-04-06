use super::*;

#[test]
fn test_compute_backoff_first_attempt_no_jitter() {
    let base = Duration::from_millis(100);
    let max = Duration::from_secs(5);
    // attempt=1: base * 2^0 = 100ms
    assert_eq!(
        compute_backoff(base, max, 1, 0.0),
        Duration::from_millis(100)
    );
}

#[test]
fn test_compute_backoff_exponential_growth() {
    let base = Duration::from_millis(100);
    let max = Duration::from_secs(5);
    // attempt=2: 100ms * 2^1 = 200ms
    assert_eq!(
        compute_backoff(base, max, 2, 0.0),
        Duration::from_millis(200)
    );
    // attempt=3: 100ms * 2^2 = 400ms
    assert_eq!(
        compute_backoff(base, max, 3, 0.0),
        Duration::from_millis(400)
    );
}

#[test]
fn test_compute_backoff_capped_at_max() {
    let base = Duration::from_millis(100);
    let max = Duration::from_millis(150);
    // attempt=2 gives 200ms without cap; expect 150ms
    assert_eq!(
        compute_backoff(base, max, 2, 0.0),
        Duration::from_millis(150)
    );
}

#[test]
fn test_compute_backoff_jitter_does_not_exceed_max() {
    let base = Duration::from_millis(100);
    let max = Duration::from_millis(100);
    // With max jitter (25%), raw = 100ms; 100ms + 25ms would be 125ms but must be capped
    assert_eq!(
        compute_backoff(base, max, 1, 0.25),
        Duration::from_millis(100)
    );
}

#[test]
fn test_compute_backoff_jitter_applied() {
    let base = Duration::from_millis(100);
    let max = Duration::from_secs(5);
    // With 10% jitter: 100ms + 10ms = 110ms
    assert_eq!(
        compute_backoff(base, max, 1, 0.10),
        Duration::from_millis(110)
    );
}

#[test]
fn test_compute_backoff_huge_attempt_does_not_overflow() {
    let base = Duration::from_millis(100);
    let max = Duration::from_secs(5);
    // Large attempt → exp clamped to i32::MAX, exponential saturates to f64::INFINITY,
    // then .min(max_backoff) clamps the result
    assert_eq!(compute_backoff(base, max, u32::MAX, 0.0), max);
}
