use std::time::Duration;

use super::*;
use crate::lock::notify::ReleaseWaiters;

#[test]
fn validate_lock_name_accepts_a_name_at_the_index_limit() {
    // Exactly MAX_LOCK_NAME_BYTES (2048) bytes must be accepted: it still fits
    // the `cluster_lock.name` btree tuple *and* the release NOTIFY payload, so
    // the acquire/release round-trip is clean. ASCII → one byte per char, so
    // length == byte length here.
    let name = "a".repeat(MAX_LOCK_NAME_BYTES);
    assert_eq!(name.len(), 2048);
    assert!(validate_lock_name(&name).is_ok());
}

#[test]
fn validate_lock_name_rejects_a_name_over_the_index_limit() {
    // One byte past the limit must be rejected *before* any lock state is
    // mutated, so the metadata INSERT never trips `cluster_lock_name_len_check`
    // and `release` never reaches a lock it cannot NOTIFY about.
    let name = "a".repeat(MAX_LOCK_NAME_BYTES + 1);
    assert_eq!(name.len(), 2049);
    assert!(matches!(
        validate_lock_name(&name),
        Err(ClusterError::InvalidName { .. })
    ));
}

#[test]
fn validate_lock_name_counts_utf8_bytes_not_chars() {
    // The limit is a *byte* limit (both the btree tuple and NOTIFY's payload
    // are sized in bytes) — matching the migration's `octet_length`, not
    // `length`. A multi-byte name with fewer than 2048 chars but more than 2048
    // bytes must be rejected. U+00E9 ('e' + acute) is 2 UTF-8 bytes, so 1025 of
    // them = 2050 bytes > limit.
    let name = "\u{e9}".repeat(1025);
    assert_eq!(name.chars().count(), 1025);
    assert_eq!(name.len(), 2050);
    assert!(matches!(
        validate_lock_name(&name),
        Err(ClusterError::InvalidName { .. })
    ));
}

#[test]
fn ttl_to_millis_converts_normal_durations() {
    assert_eq!(ttl_to_millis(Duration::from_secs(30)).unwrap(), 30_000);
}

#[test]
fn ttl_to_millis_rejects_values_beyond_i64_millis_range() {
    // `Duration::MAX.as_millis()` is far beyond `i64::MAX`.
    assert!(ttl_to_millis(Duration::MAX).is_err());
}

/// **The deadline is the only liveness authority** (DESIGN-DEPLOYABLE-GEAR §5.8.2,
/// invariant I7). The predicate must be exactly that one comparison: any second
/// mechanism vouching for a row is what made a broker restart revoke the fleet's
/// locks, and re-introducing one would break uniform expiry across profiles.
///
/// Structural, and paired with `PG-SPEC-012`, which holds the *plan* to issuing no
/// advisory-lock scan. This is the cheap guard next to it.
#[test]
fn the_stealable_predicate_is_the_deadline_and_nothing_else() {
    let sql = stealable_predicate("public.cluster_lock");
    assert_eq!(
        sql, "public.cluster_lock.expires_at <= now()",
        "the acquire predicate must be the lease deadline alone"
    );
}

/// The removed liveness beacon must not come back by any of the routes it used: an
/// advisory-lock scan, the two key halves, or the `CASE` that existed only to keep
/// that scan off the uncontended path.
#[test]
fn the_stealable_predicate_reads_no_liveness_beacon() {
    let sql = stealable_predicate("public.cluster_lock");
    for banned in [
        "pg_locks", "objsubid", "granted", "classid", "objid", "CASE",
    ] {
        assert!(
            !sql.contains(banned),
            "the predicate must not reach for {banned}: {sql}"
        );
    }
}

/// Acquisition is insert-or-steal-if-lapsed, and the steal must increment the fence
/// **off the row's own current value** rather than off anything the acquirer bound.
///
/// This is the fence guarantee of §5.8.1 reduced to one SQL expression: a stale
/// holder's `renew`/`release` can only fail their predicate if the successor's fence
/// really is different, and only the row knows what the current one is. Binding
/// `fence + 1` computed in Rust from a prior read would be a check-then-act race.
#[test]
fn the_acquire_statement_increments_the_fence_from_the_row() {
    let sql = acquire_sql("public.cluster_lock");
    assert!(
        sql.contains("fence = public.cluster_lock.fence + 1"),
        "the steal must increment the conflicting row's own fence: {sql}"
    );
    assert!(
        sql.trim_end().ends_with("RETURNING fence"),
        "the statement must return the fence so the token is mintable from it: {sql}"
    );
    assert!(
        !sql.contains("holder_id"),
        "the superseded per-acquisition UUID must be gone: {sql}"
    );
}

/// A fresh acquisition starts at [`FIRST_FENCE`], which must be positive so that
/// zero stays available to name the absence of a claim — and so the migration's
/// `cluster_lock_fence_positive_check` can be the backstop for it.
#[test]
fn a_fresh_acquisition_starts_at_a_positive_fence() {
    const { assert!(FIRST_FENCE > 0) }
    assert!(
        acquire_sql("public.cluster_lock").contains(&format!("VALUES ($1, $2, {FIRST_FENCE},")),
        "the insert path must write FIRST_FENCE"
    );
}

/// Two acquisitions in one process must be **distinct owners**, so neither can renew
/// or release the other's lease. A per-process identity would make a re-entrant
/// `try_lock` able to renew a lease it does not hold.
#[test]
fn each_in_process_acquisition_mints_its_own_owner() {
    let a = fresh_owner();
    let b = fresh_owner();
    assert_ne!(a, b);
    assert!(!a.is_empty());
}

/// A fence beyond `i64`'s range cannot match a row this plugin wrote, so it
/// saturates into an unsatisfiable predicate rather than wrapping into one that
/// could match *some other* lease.
#[test]
fn an_out_of_range_fence_saturates_rather_than_wrapping() {
    assert_eq!(fence_to_i64(1), 1);
    assert_eq!(fence_to_i64(u64::MAX), i64::MAX);
    // The boundary itself: the largest fence that is representable must round-trip
    // exactly rather than saturating one short of itself.
    let max = u64::try_from(i64::MAX).expect("i64::MAX is non-negative");
    assert_eq!(fence_to_i64(max), i64::MAX);
}

/// A stored fence outside the token's `u64` is a row this plugin did not write. It
/// is reported rather than panicked on, because `cluster_lock` is shared, mutable
/// state an operator can reach with `psql`.
#[test]
fn a_negative_stored_fence_is_a_provider_error() {
    assert_eq!(fence_to_u64(1).unwrap(), 1);
    assert!(matches!(
        fence_to_u64(-1),
        Err(ClusterError::Provider { .. })
    ));
}

#[test]
fn expires_at_sql_uses_the_database_clock() {
    // PGR-C2: the deadline must be `now() + ttl` evaluated by Postgres, never a
    // timestamp this instance computed from its own (possibly skewed) wall clock
    // — the reaper compares `expires_at` against Postgres `now()`.
    let sql = expires_at_sql(3);
    assert!(
        sql.contains("now()"),
        "must anchor on the database clock: {sql}"
    );
    assert!(
        sql.contains("$3::bigint"),
        "must add the bound ttl at $3: {sql}"
    );
}

#[tokio::test]
async fn release_waiters_wakes_a_registered_waiter() {
    let waiters = ReleaseWaiters::new();
    let waiter = waiters.wait_for("l");

    waiters.notify("l");

    assert!(waiter.await.is_ok());
}

#[tokio::test]
async fn release_waiters_notify_on_an_unregistered_name_is_a_no_op() {
    let waiters = ReleaseWaiters::new();
    // Must not panic when nobody is waiting on this name.
    waiters.notify("nobody-waiting");
}

#[tokio::test]
async fn release_waiters_only_wakes_the_matching_name() {
    let waiters = ReleaseWaiters::new();
    let waiter_a = waiters.wait_for("a");
    let waiter_b = waiters.wait_for("b");

    waiters.notify("a");

    assert!(waiter_a.await.is_ok());
    // `b`'s waiter was never notified — dropping the registry (end of scope)
    // closes its sender, so `await` resolves to `Err` rather than hanging.
    drop(waiters);
    assert!(waiter_b.await.is_err());
}

// ---------------------------------------------------------------------------
// `deadline_hint` gating (`should_hint`).
//
// The reaper's wake `select!` cannot be unit-tested from here — it lives inside
// the spawned task — but the decision that made it pathological can be, and it is
// the part with an exact rule behind it.
// ---------------------------------------------------------------------------

/// A TTL at or beyond the reaper's interval needs no hint: the reaper's own sleep
/// is capped at `interval`, so it re-reads the table from scratch before that
/// deadline can fall due. Signalling anyway is what pinned the reaper at its
/// 100 ms wake floor permanently — `Notify` keeps a permit pending, so the
/// `notified()` branch is always ready and always wins.
///
/// Retention is zero throughout, which is the pre-`L3` arithmetic: these three
/// cases are about the TTL half of the comparison and stay exactly as they were.
#[test]
fn should_not_hint_for_a_ttl_the_reaper_will_wake_for_anyway() {
    let interval = Duration::from_secs(5);
    let no_retention = Duration::ZERO;
    assert!(
        !should_hint(interval, no_retention, interval),
        "exactly at the cap"
    );
    assert!(
        !should_hint(Duration::from_secs(30), no_retention, interval),
        "a typical lease TTL"
    );
    assert!(
        !should_hint(Duration::from_secs(3_599), no_retention, interval),
        "an indefinitely-long lease"
    );
}

/// The case the hint exists for: a lock whose entire lifetime fits inside one of
/// the reaper's sleeps would otherwise go unreclaimed until that sleep ended. A
/// missed hint costs latency, not correctness — the expired `cluster_lock` row
/// stays in the table and its waiters stay unwoken until the reaper's next wake.
#[test]
fn should_hint_for_a_ttl_shorter_than_one_reaper_sleep() {
    let interval = Duration::from_secs(5);
    let no_retention = Duration::ZERO;
    assert!(should_hint(
        Duration::from_millis(200),
        no_retention,
        interval
    ));
    assert!(
        should_hint(Duration::from_millis(4_999), no_retention, interval),
        "just inside the cap"
    );
    assert!(should_hint(Duration::ZERO, no_retention, interval));
}

/// The rule is relative to the configured cadence, not to a fixed threshold: the
/// same TTL flips sides when an operator tunes `lock_reaper_interval_ms`.
#[test]
fn the_hint_threshold_follows_the_configured_interval() {
    let ttl = Duration::from_secs(1);
    let no_retention = Duration::ZERO;
    assert!(should_hint(ttl, no_retention, Duration::from_secs(5)));
    assert!(!should_hint(ttl, no_retention, Duration::from_millis(500)));
}

/// The retention window moves the deadline the hint is about: a row is not work
/// at `expires_at` any more, it is work at `expires_at + retention`
/// (DESIGN-DEPLOYABLE-GEAR §5.8.1). A TTL that would have been hinted stops being
/// hinted once the window pushes its reap past the reaper's sleep cap — which is
/// correct, because waking for it would sweep a row the predicate must skip.
#[test]
fn retention_pushes_a_hintable_ttl_past_the_cap() {
    let interval = Duration::from_secs(5);
    let ttl = Duration::from_millis(200);

    assert!(
        should_hint(ttl, Duration::ZERO, interval),
        "the pre-retention answer"
    );
    assert!(
        !should_hint(ttl, Duration::from_secs(10), interval),
        "reapable in 10.2s, and the reaper wakes every 5s regardless"
    );
    assert!(
        should_hint(ttl, Duration::from_secs(1), interval),
        "reapable in 1.2s, still inside one sleep"
    );
}

/// Under the shipped defaults the gate is simply always closed, and that is worth
/// pinning: an hour of retention against a five-second cadence means no lock, at
/// any TTL, can become reapable inside one of the reaper's sleeps.
#[test]
fn the_default_window_suppresses_every_hint() {
    let interval = Duration::from_millis(crate::config::default_lock_reaper_interval());
    let retention = Duration::from_millis(crate::config::default_fence_retention());

    for ttl in [
        Duration::ZERO,
        Duration::from_millis(1),
        Duration::from_secs(30),
    ] {
        assert!(
            !should_hint(ttl, retention, interval),
            "ttl {ttl:?} must not hint under the shipped defaults"
        );
    }
}
