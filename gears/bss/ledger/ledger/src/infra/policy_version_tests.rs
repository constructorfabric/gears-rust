//! Unit tests for the pure evidence-ref-reuse set-check
//! ([`first_unreused_tuple`]) backing [`PolicyVersionGuard`] (§4.6, AC #15).
//! The DB-touching `check` is exercised end-to-end in
//! `tests/postgres_policy_version.rs`.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use super::*;

/// Build an evidence tuple from a single `pricing_snapshot_ref` (the most common
/// pinned ref); the other four refs are `None`.
fn snapshot(snapshot_ref: &str) -> EvidenceTuple {
    (Some(snapshot_ref.to_owned()), None, None, None, None)
}

/// The all-NULL tuple (a line with no pinned evidence).
fn empty() -> EvidenceTuple {
    (None, None, None, None, None)
}

#[test]
fn correction_reusing_original_refs_passes() {
    let original = vec![snapshot("snap-A"), snapshot("snap-B")];
    // The correction reuses a subset of the original's refs.
    let correction = vec![snapshot("snap-A")];
    assert_eq!(
        first_unreused_tuple(&original, &correction),
        None,
        "a correction that reuses the original's refs must pass"
    );
}

#[test]
fn correction_inventing_a_new_ref_is_flagged() {
    let original = vec![snapshot("snap-A")];
    // The correction carries a ref the original never had.
    let correction = vec![snapshot("snap-A"), snapshot("snap-NEW")];
    assert_eq!(
        first_unreused_tuple(&original, &correction),
        Some(snapshot("snap-NEW")),
        "a correction that invents a new pinned ref must be flagged"
    );
}

#[test]
fn all_null_tuples_are_ignored_on_both_sides() {
    // A reversal's lines (built from a read-back that drops pinned refs) are all
    // all-NULL — they must never trip the guard, even when the original carried
    // refs.
    let original = vec![snapshot("snap-A"), empty()];
    let correction = vec![empty(), empty()];
    assert_eq!(
        first_unreused_tuple(&original, &correction),
        None,
        "all-NULL correction tuples carry no evidence and must be ignored"
    );
}

#[test]
fn empty_correction_passes() {
    let original = vec![snapshot("snap-A")];
    let correction: Vec<EvidenceTuple> = Vec::new();
    assert_eq!(
        first_unreused_tuple(&original, &correction),
        None,
        "a correction with no lines reuses nothing and cannot violate"
    );
}

#[test]
fn multi_field_tuple_must_match_exactly() {
    // A tuple that differs in only the second field (po_allocation_group) is a
    // distinct pinned-evidence tuple and must not be treated as reused.
    let original = vec![(
        Some("snap-A".to_owned()),
        Some("po-1".to_owned()),
        None,
        None,
        None,
    )];
    let correction = vec![(
        Some("snap-A".to_owned()),
        Some("po-2".to_owned()),
        None,
        None,
        None,
    )];
    assert_eq!(
        first_unreused_tuple(&original, &correction),
        Some((
            Some("snap-A".to_owned()),
            Some("po-2".to_owned()),
            None,
            None,
            None,
        )),
        "a tuple differing in any field is a distinct pinned ref, not a reuse"
    );
}
