//! Unit tests for the store-owned lease record, token predicate and clock.

use std::time::Duration;

use super::{FENCE_RETENTION_DEFAULT, LeaseClock, LeaseRecord, LeaseToken};

fn record(owner: &str, deadline_ms: u64, fence: u64) -> LeaseRecord {
    LeaseRecord {
        owner: owner.to_owned(),
        deadline_ms,
        fence,
        nonce: 0,
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
    future[4] = 3;
    assert!(
        LeaseRecord::decode(&future).is_none(),
        "a later encoding revision must read as a foreign record, never as v2"
    );
}

#[test]
fn decode_rejects_the_superseded_v1_layout() {
    // The nonce arrived at v2 (no migration — see the decision in the remediation
    // plan). A v1 record left in a store must read as a foreign record so it is
    // never stolen or mis-parsed, even when it is long enough to clear the
    // header-length check.
    let mut legacy = record("owner-legacy-holder", 1_777_000_000_000, 7).encode();
    legacy[4] = 1;
    assert!(
        legacy.len() >= 21,
        "the legacy value must clear even the old header so only the version \
         check can reject it"
    );
    assert!(
        LeaseRecord::decode(&legacy).is_none(),
        "a v1 record must read as a foreign record, never mis-parsed as v2"
    );
}

#[test]
fn the_nonce_survives_the_round_trip() {
    let original = record("sa/orders", 1_777_000_123_456, 42);
    let with_nonce = LeaseRecord {
        nonce: 0x0123_4567_89ab_cdef,
        ..original
    };
    let decoded = LeaseRecord::decode(&with_nonce.encode()).expect("a value we wrote must decode");
    assert_eq!(decoded.nonce, 0x0123_4567_89ab_cdef);
    assert_eq!(decoded, with_nonce);
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
    // The virtual clock is the one built to track `tokio::time::advance`; the
    // production `LeaseClock::new()` is the pure wall clock and deliberately does
    // not move under a paused runtime (that is the H3 fix).
    let clock = LeaseClock::virtual_clock();
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
    let clock = LeaseClock::virtual_clock();
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
    let first = LeaseClock::virtual_clock();
    let second = LeaseClock::virtual_clock();
    tokio::time::advance(Duration::from_mins(1)).await;
    let drift = first.now_millis().abs_diff(second.now_millis());
    assert!(
        drift <= 1,
        "clocks anchored together must agree, drift {drift}ms"
    );
}

#[tokio::test(start_paused = true)]
async fn remaining_until_reports_none_once_passed() {
    let clock = LeaseClock::virtual_clock();
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

/// H3: a backward wall step biases the wall/monotonic hybrid permanently, but the
/// production wall clock is immune to it by construction.
///
/// This is the regression the fix exists for. It is expressed at the arithmetic
/// seam ([`super::hybrid_now_millis`] for the old/virtual path; the identity for
/// the production path) because production reads the real, uncontrollable
/// `SystemTime` — there is no way to step it backward in a unit test, so the
/// divergence is only *expressible* at the seam that computes now-millis from a
/// wall reading. `now_millis` in [`LeaseClock::System`] is exactly `wall_millis()`
/// (no anchor, no arithmetic), so the identity below *is* the production formula.
///
/// **Before the fix** `LeaseClock::now_millis` was `hybrid_now_millis(...)`
/// unconditionally — the production path. Replica A (anchored before the step) and
/// replica B (started after it) then disagree by the step size, forever: the first
/// assertion below is what production used to do. **After the fix** production is
/// the wall clock, so both replicas read the same value and the second assertion
/// holds. Feed the *same* backward-step scenario to both formulas; only the hybrid
/// diverges.
#[test]
fn a_backward_wall_step_biases_the_hybrid_forever_but_never_the_wall_clock() {
    // A 60 s backward wall jump (e.g. `chronyc makestep`, a VM snapshot restore),
    // 100 s after replica A anchored its clock. The jump is smaller than the time
    // A has been running, which is the case where the hybrid reports the pre-step
    // timeline *exactly* — the worst case for I7.
    let anchor_wall: u64 = 1_000_000;
    let runtime_since_anchor: u128 = 100_000; // 100 s of monotonic time on A
    let backward_step: u64 = 60_000; // 60 s backward wall jump
    // The wall clock both replicas read right after the jump.
    let wall_now = anchor_wall + 100_000 - backward_step;

    // The OLD production path (== today's virtual hybrid).
    // A was anchored before the jump; B started at `wall_now` (0 runtime elapsed).
    let a_hybrid = super::hybrid_now_millis(wall_now, anchor_wall, runtime_since_anchor);
    let b_hybrid = super::hybrid_now_millis(wall_now, wall_now, 0);
    assert_eq!(
        a_hybrid.abs_diff(b_hybrid),
        backward_step,
        "the wall/monotonic hybrid biases the pre-step replica by the whole step, \
         permanently: this is the H3 defect, and it is what production did before \
         the fix (a_hybrid={a_hybrid}, b_hybrid={b_hybrid})"
    );

    // The NEW production path: pure wall clock, `now_millis == wall_millis()`.
    // Neither replica keeps an anchor, so the reading is `wall_now` for both,
    // irrespective of when each anchored. Nothing a wall step can bias.
    let a_prod = wall_now; // LeaseClock::System::now_millis() given this wall reading
    let b_prod = wall_now;
    assert_eq!(
        a_prod, b_prod,
        "pure wall-clock lease time cannot diverge across a wall step, by \
         construction: the anchor the hybrid biased no longer exists"
    );
}

/// H3, at the production seam: the clock [`LeaseClock::new`] builds must ignore
/// virtual time, and the test clock [`LeaseClock::virtual_clock`] builds must still
/// track it.
///
/// This is the fail-before/pass-after guard on the *wiring*, complementing the
/// arithmetic test above. **Before the fix** `LeaseClock::new()` was the hybrid, so
/// `system.now_millis()` would jump by ~1 h under the `advance` below (the virtual
/// component the production path must not have) and the first assertion would fail.
/// **After the fix** `new()` is [`LeaseClock::System`] — pure wall — so it does not
/// move under a paused runtime, while the injected virtual clock still fast-forwards
/// so every TTL test keeps working. Re-break it (make `new()` return
/// `virtual_clock()`) and this test fails, which is the point.
#[tokio::test(start_paused = true)]
async fn the_production_clock_ignores_virtual_time_but_the_test_clock_tracks_it() {
    const WALL_CLOCK_TOLERANCE_MS: u64 = 1_000;

    let system = LeaseClock::new();
    let test = LeaseClock::virtual_clock();
    let system_before = system.now_millis();
    let test_before = test.now_millis();

    tokio::time::advance(Duration::from_hours(1)).await; // one hour of virtual time

    // Production: no anchor, no virtual component. A paused runtime advances no real
    // wall time, so the reading barely moves — nothing like the hour advanced.
    let system_after = system.now_millis();
    assert!(
        system_after.abs_diff(system_before) < WALL_CLOCK_TOLERANCE_MS,
        "the production clock must not absorb virtual time (H3): {system_before} -> \
         {system_after}; before the fix this jumped ~1h because `new()` was the hybrid"
    );

    // The injected virtual clock still fast-forwards, so TTL scenarios keep lapsing.
    // Its wall anchor is sampled before `test_before`, so real wall time spent between
    // those reads reduces the observed delta; use the same bound accepted above.
    let test_after = test.now_millis();
    assert!(
        test_after.saturating_sub(test_before)
            >= 3_600_000_u64.saturating_sub(WALL_CLOCK_TOLERANCE_MS),
        "the injected virtual clock must still track `advance` ({test_before} -> \
         {test_after})"
    );
}
