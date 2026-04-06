use crate::domain::model::{
    BurstConfig, RateLimitAlgorithm, RateLimitScope, RateLimitStrategy, SustainedRate,
};

use super::*;

fn make_config(rate: u32, window: Window, burst_capacity: Option<u32>) -> RateLimitConfig {
    RateLimitConfig {
        sharing: Default::default(),
        algorithm: RateLimitAlgorithm::TokenBucket,
        sustained: SustainedRate { rate, window },
        burst: burst_capacity.map(|c| BurstConfig { capacity: c }),
        scope: RateLimitScope::Tenant,
        strategy: RateLimitStrategy::Reject,
        cost: 1,
    }
}

#[test]
fn allows_within_capacity() {
    let limiter = RateLimiter::new();
    let config = make_config(10, Window::Second, None);
    for _ in 0..10 {
        assert!(limiter.try_consume("test", &config, "/test").is_ok());
    }
}

#[test]
fn denies_when_exhausted() {
    let limiter = RateLimiter::new();
    let config = make_config(2, Window::Second, None);
    assert!(limiter.try_consume("test", &config, "/test").is_ok());
    assert!(limiter.try_consume("test", &config, "/test").is_ok());
    let err = limiter.try_consume("test", &config, "/test").unwrap_err();
    assert!(matches!(err, DomainError::RateLimitExceeded { .. }));
}

#[test]
fn retry_after_is_calculated() {
    let limiter = RateLimiter::new();
    let config = make_config(1, Window::Minute, None);
    assert!(limiter.try_consume("test", &config, "/test").is_ok());
    match limiter.try_consume("test", &config, "/test") {
        Err(DomainError::RateLimitExceeded {
            retry_after_secs, ..
        }) => {
            // ~60 seconds (1 token per minute).
            assert!(retry_after_secs.unwrap() > 0);
            assert!(retry_after_secs.unwrap() <= 60);
        }
        other => panic!("expected RateLimitExceeded, got {other:?}"),
    }
}

#[test]
fn burst_capacity_used() {
    let limiter = RateLimiter::new();
    let config = make_config(1, Window::Second, Some(5));
    for _ in 0..5 {
        assert!(limiter.try_consume("test", &config, "/test").is_ok());
    }
    assert!(limiter.try_consume("test", &config, "/test").is_err());
}

#[test]
fn separate_keys_independent() {
    let limiter = RateLimiter::new();
    let config = make_config(1, Window::Second, None);
    assert!(limiter.try_consume("key-a", &config, "/test").is_ok());
    assert!(limiter.try_consume("key-b", &config, "/test").is_ok());
    assert!(limiter.try_consume("key-a", &config, "/test").is_err());
    assert!(limiter.try_consume("key-b", &config, "/test").is_err());
}

#[test]
fn purge_removes_stale_entries() {
    let limiter = RateLimiter::new();
    let config = make_config(10, Window::Second, None);
    limiter.try_consume("a", &config, "/test").unwrap();
    limiter.try_consume("b", &config, "/test").unwrap();
    limiter.try_consume("c", &config, "/test").unwrap();

    let active: HashSet<String> = ["a", "c"].iter().map(|s| (*s).into()).collect();
    limiter.purge_keys(&active);

    // a and c survive, b is gone.
    assert!(limiter.buckets.contains_key("a"));
    assert!(!limiter.buckets.contains_key("b"));
    assert!(limiter.buckets.contains_key("c"));
}

#[test]
fn remove_key_deletes_single_bucket() {
    let limiter = RateLimiter::new();
    let config = make_config(10, Window::Second, None);
    limiter
        .try_consume("upstream:aaa", &config, "/test")
        .unwrap();
    limiter.try_consume("route:bbb", &config, "/test").unwrap();

    limiter.remove_key("upstream:aaa");

    assert!(!limiter.buckets.contains_key("upstream:aaa"));
    assert!(limiter.buckets.contains_key("route:bbb"));
}

#[test]
fn remove_key_noop_for_missing_key() {
    let limiter = RateLimiter::new();
    // Should not panic.
    limiter.remove_key("nonexistent");
    assert!(limiter.buckets.is_empty());
}

#[test]
fn purge_with_empty_set_removes_all() {
    let limiter = RateLimiter::new();
    let config = make_config(10, Window::Second, None);
    limiter.try_consume("x", &config, "/test").unwrap();
    limiter.try_consume("y", &config, "/test").unwrap();

    limiter.purge_keys(&HashSet::new());

    assert!(limiter.buckets.is_empty());
}
