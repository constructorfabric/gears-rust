// Created: 2026-09-24 by Virtuozzo International GmbH
//! The stamps rows are tagged by: aligned to what the store keeps, and always
//! moving with the row they replace.

use time::Duration;

use super::{now, stamp_after};

#[test]
fn the_clock_reads_at_whole_microseconds() {
    // What a `timestamptz` keeps; a finer stamp would round-trip changed.
    for _ in 0..1_000 {
        assert_eq!(now().nanosecond() % 1_000, 0);
    }
}

#[test]
fn a_stamp_is_strictly_after_the_version_it_replaces() {
    // A clock that has not passed the version steps just past it, so the tag
    // moves even when two writes share a microsecond.
    let ahead = now() + Duration::days(1);
    assert_eq!(stamp_after(Some(ahead)), ahead + Duration::microseconds(1));

    // A clock well past the version reads as now — and aligned.
    let behind = now() - Duration::days(1);
    let at = stamp_after(Some(behind));
    assert!(at > behind);
    assert_eq!(at.nanosecond() % 1_000, 0);

    // The version read this very instant: the stamp still moves.
    let same = now();
    assert!(stamp_after(Some(same)) > same);

    // No version to replace: plain now.
    assert!(stamp_after(None) >= same);
}
