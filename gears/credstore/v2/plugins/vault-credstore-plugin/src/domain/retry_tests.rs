// Created: 2026-10-04 by Constructor Tech
// Updated: 2026-10-06 by Constructor Tech
use super::*;

fn policy(max_attempts: u32, base_delay_ms: u64) -> RetryPolicy {
    RetryPolicy::new(&RetryConfig {
        max_attempts,
        base_delay_ms,
    })
}

#[test]
fn defaults_give_three_attempts() {
    let p = RetryPolicy::new(&RetryConfig::default());
    assert_eq!(p.max_attempts(), 3);
    assert_eq!(p.backoff(1), Duration::from_millis(100));
}

#[test]
fn no_retry_is_one_attempt_without_waiting() {
    let p = RetryPolicy::no_retry();
    assert_eq!(p.max_attempts(), 1);
    assert_eq!(p.backoff(1), Duration::ZERO);
    assert_eq!(p.jittered_backoff(1), Duration::ZERO);
}

#[test]
fn backoff_doubles_per_failure() {
    let p = policy(10, 100);
    assert_eq!(p.backoff(1), Duration::from_millis(100));
    assert_eq!(p.backoff(2), Duration::from_millis(200));
    assert_eq!(p.backoff(3), Duration::from_millis(400));
    assert_eq!(p.backoff(4), Duration::from_millis(800));
    assert_eq!(p.backoff(5), Duration::from_millis(1600));
}

#[test]
fn backoff_is_capped_at_two_seconds() {
    let p = policy(10, 100);
    assert_eq!(p.backoff(6), Duration::from_secs(2));
    assert_eq!(p.backoff(30), Duration::from_secs(2));
    assert_eq!(p.backoff(u32::MAX), Duration::from_secs(2));
    // A base above the cap (not accepted by config validation) is clamped too.
    assert_eq!(policy(3, 60_000).backoff(1), Duration::from_secs(2));
}

#[test]
fn zero_failures_use_the_base_delay() {
    assert_eq!(policy(3, 100).backoff(0), Duration::from_millis(100));
}

#[test]
fn zero_attempts_are_treated_as_one() {
    assert_eq!(policy(0, 100).max_attempts(), 1);
}

#[test]
fn jitter_stays_in_the_upper_half_of_the_backoff() {
    let p = policy(10, 100);
    for failed in 1..=8 {
        let upper = p.backoff(failed);
        for _ in 0..50 {
            let d = p.jittered_backoff(failed);
            assert!(d >= upper / 2, "{d:?} below half of {upper:?}");
            assert!(d <= upper, "{d:?} above {upper:?}");
        }
    }
}
