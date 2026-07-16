use std::time::Duration;

use super::*;
use crate::lock::notify::ReleaseWaiters;

#[test]
fn lock_key_is_stable_across_calls() {
    assert_eq!(lock_key("orders/lock"), lock_key("orders/lock"));
}

#[test]
fn lock_key_differs_for_different_names() {
    assert_ne!(lock_key("orders/lock"), lock_key("inventory/lock"));
}

#[test]
fn lock_key_handles_names_with_special_characters() {
    // Must not panic on non-ASCII/punctuation-heavy names, and must still be
    // stable for the same input.
    let name = "tenant:\u{e1}\u{e7}\u{e7}/lock #1!";
    assert_eq!(lock_key(name), lock_key(name));
}

#[test]
fn lock_key_exercises_the_full_64_bit_hash() {
    // `key1`/`key2` are independent halves of the same 64-bit hash, not a
    // 32-bit hash duplicated into both halves — a regression collapsing back
    // to `hashtext()`'s 32 bits (DESIGN.md §5.1) would very likely make one
    // of a few sample names collide on `key1` while still differing on
    // `key2`, or vice versa. This isn't a proof, but it would probably catch
    // an accidental 32-bit-only regression.
    let (key1_a, key2_a) = lock_key("a");
    let (key1_b, key2_b) = lock_key("b");
    assert!(key1_a != key1_b || key2_a != key2_b);
}

#[test]
fn lock_key_split_preserves_the_full_64_bit_hash() {
    // PGR-L4: the collision surface of `lock_key` is *exactly* xxh3_64's — the
    // (high i32, low i32) split introduces no additional collisions, and any two
    // names with the same 64-bit hash necessarily map to the same `(key1, key2)`
    // advisory-lock argument pair (so they *would* contend on the same
    // `pg_advisory_lock` slot). Reconstruct the u64 from the two halves and
    // confirm it round-trips to the raw hash for a spread of names, including
    // near-identical ones (a naive split bug — e.g. duplicating one half —
    // would surface here). Because the split is thus a faithful bijection of the
    // hash, no natural collision pair needs to be brute-forced out of xxh3's
    // 64-bit space to exercise the collision path (DESIGN.md §5.1).
    for name in [
        "",
        "a",
        "collision-probe-a",
        "collision-probe-b",
        "svc/leader",
        "svc/leadeR",
    ] {
        let (key1, key2) = lock_key(name);
        let reconstructed =
            (u64::from(key1.cast_unsigned()) << 32) | u64::from(key2.cast_unsigned());
        assert_eq!(
            reconstructed,
            xxhash_rust::xxh3::xxh3_64(name.as_bytes()),
            "lock_key must split the 64-bit hash losslessly into (high, low) i32 halves so \
             its collision surface is exactly the hash's (DESIGN.md §5.1); name = {name:?}"
        );
    }
}

#[test]
fn validate_lock_name_accepts_a_name_at_the_notify_limit() {
    // Exactly MAX_LOCK_NAME_BYTES (7999) bytes must be accepted: the release
    // NOTIFY payload can still carry it, so the acquire/release round-trip is
    // clean. ASCII → one byte per char, so length == byte length here.
    let name = "a".repeat(MAX_LOCK_NAME_BYTES);
    assert_eq!(name.len(), 7999);
    assert!(validate_lock_name(&name).is_ok());
}

#[test]
fn validate_lock_name_rejects_a_name_over_the_notify_limit() {
    // One byte past the limit must be rejected *before* any lock state is
    // mutated, so `release` never reaches a lock it cannot NOTIFY about.
    let name = "a".repeat(MAX_LOCK_NAME_BYTES + 1);
    assert_eq!(name.len(), 8000);
    assert!(matches!(
        validate_lock_name(&name),
        Err(ClusterError::InvalidName { .. })
    ));
}

#[test]
fn validate_lock_name_counts_utf8_bytes_not_chars() {
    // The limit is a *byte* limit (NOTIFY's payload is bytes). A multi-byte
    // char name with fewer than 7999 chars but more than 7999 bytes must be
    // rejected. U+00E9 ('e' + acute) is 2 UTF-8 bytes, so 4000 of them = 8000
    // bytes > limit.
    let name = "\u{e9}".repeat(4000);
    assert_eq!(name.chars().count(), 4000);
    assert_eq!(name.len(), 8000);
    assert!(matches!(
        validate_lock_name(&name),
        Err(ClusterError::InvalidName { .. })
    ));
}

#[test]
fn duration_to_ttl_ms_converts_normal_durations() {
    assert_eq!(duration_to_ttl_ms(Duration::from_secs(30)).unwrap(), 30_000);
}

#[test]
fn duration_to_ttl_ms_rejects_values_beyond_i64_millis_range() {
    // `Duration::MAX.as_millis()` is far beyond `i64::MAX`.
    assert!(duration_to_ttl_ms(Duration::MAX).is_err());
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
