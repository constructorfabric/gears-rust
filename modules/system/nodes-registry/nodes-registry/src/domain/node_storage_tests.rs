use super::*;
use chrono::Utc;
use nodes_registry_sdk::SysCap;

fn make_syscap_with(fetched_at_secs: i64, cache_ttl_secs: u64) -> SysCap {
    SysCap {
        key: "k".to_owned(),
        category: "c".to_owned(),
        name: "n".to_owned(),
        display_name: "d".to_owned(),
        present: true,
        version: None,
        amount: None,
        amount_dimension: None,
        details: None,
        cache_ttl_secs,
        fetched_at_secs,
    }
}

#[test]
fn cache_not_expired_before_ttl() {
    let now = Utc::now();
    let ttl = 10u64;
    let fetched_at = now.timestamp() - 9; // 9 seconds ago

    let cap = make_syscap_with(fetched_at, ttl);

    assert!(!cap.cache_is_expired(now));
}

#[test]
fn cache_expired_at_ttl_boundary() {
    let now = Utc::now();
    let ttl = 10u64;
    let fetched_at = now.timestamp() - 10; // exactly ttl seconds ago

    let cap = make_syscap_with(fetched_at, ttl);

    assert!(cap.cache_is_expired(now));
}

#[test]
fn future_fetched_at_counts_as_fresh_when_ttl_positive() {
    let now = Utc::now();
    let ttl = 5u64;
    let fetched_at = now.timestamp() + 60; // fetched in the future

    let cap = make_syscap_with(fetched_at, ttl);

    // age is treated as 0, so not expired for positive ttl
    assert!(!cap.cache_is_expired(now));
}

#[test]
fn zero_ttl_always_expired() {
    let now = Utc::now();
    let ttl = 0u64;
    let fetched_at = now.timestamp();

    let cap = make_syscap_with(fetched_at, ttl);

    assert!(cap.cache_is_expired(now));
}
