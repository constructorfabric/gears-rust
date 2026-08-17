// Created: 2026-08-12 by Constructor Tech
//! Unit tests for the store-owned lease record, token predicate and clock.

use std::time::Duration;

use super::{FENCE_RETENTION_DEFAULT, LeaseClock, LeaseRecord, LeaseToken};

fn record(owner: &str, deadline_ms: u64, fence: u64) -> LeaseRecord {
    LeaseRecord {
        owner: owner.to_owned(),
        deadline_ms,
        fence,
    }
}

#[test]
fn encode_decode_round_trips_every_field() {
    let original = record("sa/orders-7f3c", 1_777_000_123_456, 42);
    let decoded = LeaseRecord::decode(&original.encode()).expect("a value we wrote must decode");
    assert_eq!(decoded, original);
}

#[test]
fn encode_decode_round_trips_the_extremes() {
    // An empty owner and saturated numerics must survive: `deadline_after`
    // saturates to `u64::MAX` on an absurd TTL, so that value reaches the codec.
    let original = record("", u64::MAX, u64::MAX);
    let decoded = LeaseRecord::decode(&original.encode()).expect("extremes must decode");
    assert_eq!(decoded, original);
}

#[test]
fn decode_rejects_values_cluster_did_not_write() {
    // The pre-lease encoding: a bare holder UUID. Treated as a foreign record
    // rather than stolen or overwritten.
    assert!(LeaseRecord::decode(b"3f2504e0-4f89-41d3-9a0c-0305e82c3301").is_none());
    assert!(LeaseRecord::decode(b"").is_none(), "empty value");
    assert!(
        LeaseRecord::decode(b"CLSL").is_none(),
        "magic but no header"
    );
    let mut wrong_magic = record("o", 1, 1).encode();
    wrong_magic[0] = b'X';
    assert!(LeaseRecord::decode(&wrong_magic).is_none(), "wrong magic");
}

#[test]
fn decode_rejects_an_unrecognised_version() {
    let mut future = record("o", 1, 1).encode();
    future[4] = 2;
    assert!(
        LeaseRecord::decode(&future).is_none(),
        "a later encoding revision must read as a foreign record, never as v1"
    );
}

#[test]
fn decode_rejects_a_non_utf8_owner() {
    let mut broken = record("o", 1, 1).encode();
    let owner_at = broken.len() - 1;
    broken[owner_at] = 0xff;
    assert!(LeaseRecord::decode(&broken).is_none());
}

#[test]
fn encoding_is_canonical_so_a_value_guard_can_use_it() {
    // `release` guards its delete on the exact bytes it read, so two encodings of
    // one record must be byte-identical.
    let first = record("owner-a", 1_777_000_000_000, 7);
    let second = record("owner-a", 1_777_000_000_000, 7);
    assert_eq!(first.encode(), second.encode());
}

#[test]
fn liveness_is_strict_at_the_deadline() {
    let rec = record("owner-a", 1_000, 1);
    assert!(rec.is_live(999));
    assert!(
        !rec.is_live(1_000),
        "a lease whose deadline is exactly now has lapsed"
    );
    assert!(!rec.is_live(1_001));
}

#[test]
fn matches_requires_both_owner_and_fence() {
    let rec = record("owner-a", 1_000, 3);
    assert!(rec.matches(&LeaseToken::new("res", "owner-a", 3)));
    assert!(
        !rec.matches(&LeaseToken::new("res", "owner-b", 3)),
        "another holder's token must not match"
    );
    assert!(
        !rec.matches(&LeaseToken::new("res", "owner-a", 2)),
        "the same holder's superseded token must not match: this is the fence"
    );
}

#[test]
fn matches_ignores_the_token_name() {
    // The name selects the record (it is in the key); the predicate is over owner
    // and fence only.
    let rec = record("owner-a", 1_000, 1);
    assert!(rec.matches(&LeaseToken::new("whatever", "owner-a", 1)));
}

#[tokio::test(start_paused = true)]
async fn the_clock_follows_virtual_time() {
    let clock = LeaseClock::new();
    let before = clock.now_millis();
    tokio::time::advance(Duration::from_secs(30)).await;
    let after = clock.now_millis();
    assert!(
        after >= before + 30_000,
        "advancing virtual time by 30s must move the lease clock at least as far \
         ({before} -> {after})"
    );
}

#[tokio::test(start_paused = true)]
async fn a_lease_lapses_when_virtual_time_passes_its_deadline() {
    let clock = LeaseClock::new();
    let rec = record("owner-a", clock.deadline_after(Duration::from_secs(10)), 1);
    assert!(rec.is_live(clock.now_millis()));
    tokio::time::advance(Duration::from_secs(11)).await;
    assert!(
        !rec.is_live(clock.now_millis()),
        "the deadline is the only liveness authority"
    );
}

#[tokio::test(start_paused = true)]
async fn two_clocks_anchored_together_agree_across_an_advance() {
    // The property the cross-handle renew test rests on: a lease written through
    // one backend handle is evaluated identically by another.
    let first = LeaseClock::new();
    let second = LeaseClock::new();
    tokio::time::advance(Duration::from_mins(1)).await;
    let drift = first.now_millis().abs_diff(second.now_millis());
    assert!(
        drift <= 1,
        "clocks anchored together must agree, drift {drift}ms"
    );
}

#[tokio::test(start_paused = true)]
async fn remaining_until_reports_none_once_passed() {
    let clock = LeaseClock::new();
    let deadline = clock.deadline_after(Duration::from_secs(5));
    let remaining = clock
        .remaining_until(deadline)
        .expect("a future deadline has time remaining");
    assert!(remaining <= Duration::from_secs(5) && remaining > Duration::from_secs(4));
    tokio::time::advance(Duration::from_secs(6)).await;
    assert!(clock.remaining_until(deadline).is_none());
}

#[tokio::test(start_paused = true)]
async fn deadline_after_saturates_instead_of_wrapping() {
    let clock = LeaseClock::new();
    assert_eq!(
        clock.deadline_after(Duration::MAX),
        u64::MAX,
        "an absurd TTL must yield a deadline that never lapses, not one already past"
    );
}

#[test]
fn fence_retention_dwarfs_any_plausible_lease_ttl() {
    assert_eq!(FENCE_RETENTION_DEFAULT, Duration::from_hours(1));
}
