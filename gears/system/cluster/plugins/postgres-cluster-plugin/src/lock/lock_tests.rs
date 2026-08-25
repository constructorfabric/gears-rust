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

/// The acquire predicate must let the cheap indexed comparison short-circuit the
/// `pg_locks` scan: `CASE`, never `OR`, whose operand evaluation order SQL does
/// not guarantee. `PG-SPEC-012` holds the *runtime* behaviour to this with
/// `EXPLAIN ANALYZE`; this is the cheap structural guard next to it.
#[test]
fn stealable_predicate_short_circuits_with_case_not_or() {
    let sql = stealable_predicate("public.cluster_lock");
    assert!(
        sql.starts_with("CASE WHEN") && sql.contains("expires_at <= now()"),
        "the expiry test must be the CASE's first branch: {sql}"
    );
    assert!(
        !sql.contains(" OR "),
        "an OR would let Postgres evaluate the pg_locks scan on the uncontended path: {sql}"
    );
    assert!(
        sql.contains("objsubid = 2") && sql.contains("granted"),
        "the liveness test must match the two-argument advisory form, granted only: {sql}"
    );
}

/// The predicate reads the beacon halves off the *conflicting row*, not off a
/// bind parameter: it is asking "is the current holder alive?", and binding our
/// own key there would ask something else entirely.
#[test]
fn stealable_predicate_compares_against_the_rows_own_beacon() {
    // Collapsed to single spaces so the assertion tests the correlation, not the
    // column alignment `stealable_predicate`'s `format!` happens to use.
    let sql = stealable_predicate("public.cluster_lock")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    assert!(
        sql.contains("classid = public.cluster_lock.holder_beacon_hi::oid")
            && sql.contains("objid = public.cluster_lock.holder_beacon_lo::oid"),
        "must correlate against the conflicting row's own beacon columns: {sql}"
    );
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
#[test]
fn should_not_hint_for_a_ttl_the_reaper_will_wake_for_anyway() {
    let interval = Duration::from_secs(5);
    assert!(!should_hint(interval, interval), "exactly at the cap");
    assert!(
        !should_hint(Duration::from_secs(30), interval),
        "a typical lease TTL"
    );
    assert!(
        !should_hint(Duration::from_secs(3_599), interval),
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
    assert!(should_hint(Duration::from_millis(200), interval));
    assert!(
        should_hint(Duration::from_millis(4_999), interval),
        "just inside the cap"
    );
    assert!(should_hint(Duration::ZERO, interval));
}

/// The rule is relative to the configured cadence, not to a fixed threshold: the
/// same TTL flips sides when an operator tunes `lock_reaper_interval_ms`.
#[test]
fn the_hint_threshold_follows_the_configured_interval() {
    let ttl = Duration::from_secs(1);
    assert!(should_hint(ttl, Duration::from_secs(5)));
    assert!(!should_hint(ttl, Duration::from_millis(500)));
}
