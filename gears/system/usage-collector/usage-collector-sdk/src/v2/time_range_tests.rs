use time::{Duration, OffsetDateTime, UtcOffset};

use crate::v2::error::UsageCollectorError;
use crate::v2::time_range::TimeRange;

fn at(secs: i64) -> OffsetDateTime {
    OffsetDateTime::from_unix_timestamp(secs).expect("in-range unix timestamp")
}

#[test]
fn new_accepts_an_ordered_range() {
    let range = TimeRange::new(at(1_700_000_000), at(1_700_003_600)).expect("ordered range");
    assert_eq!(range.lower_inclusive(), at(1_700_000_000));
    assert_eq!(range.upper_exclusive(), at(1_700_003_600));
}

#[test]
fn new_rejects_an_inverted_range() {
    let err = TimeRange::new(at(1_700_003_600), at(1_700_000_000))
        .expect_err("to < from must be rejected");
    let UsageCollectorError::InvalidArgument { field, .. } = err else {
        panic!("expected InvalidArgument, got {err:?}");
    };
    assert_eq!(field, "time_range");
}

#[test]
fn new_rejects_an_empty_range() {
    // `from == to` selects nothing at all: the predicate is
    // `from <= window_end < to`. Accepting it would report "no usage" for
    // what is really a caller passing one instant twice.
    TimeRange::new(at(1_700_000_000), at(1_700_000_000)).expect_err("from == to must be rejected");
}

#[test]
fn new_normalizes_both_bounds_to_utc() {
    let offset = UtcOffset::from_hms(5, 30, 0).expect("valid offset");
    let range = TimeRange::new(
        at(1_700_000_000).to_offset(offset),
        at(1_700_003_600).to_offset(offset),
    )
    .expect("ordered range");
    assert_eq!(range.lower_inclusive().offset(), UtcOffset::UTC);
    assert_eq!(range.upper_exclusive().offset(), UtcOffset::UTC);
    // Normalization moves the offset, never the instant, on both bounds.
    assert_eq!(range.lower_inclusive(), at(1_700_000_000));
    assert_eq!(range.upper_exclusive(), at(1_700_003_600));
}

#[test]
fn selection_is_inclusive_at_the_lower_bound() {
    // This inclusive lower bound is also what selects a point event sitting
    // on a range boundary: a point event has window_start == window_end, so
    // `contains_window_end` — which reads only window_end — cannot tell it
    // apart from any other entry ending here, and needs no case of its own.
    let range = TimeRange::new(at(1_700_000_000), at(1_700_003_600)).expect("range");
    assert!(range.contains_window_end(at(1_700_000_000)));
}

#[test]
fn selection_is_exclusive_at_the_upper_bound() {
    // Per `cpt-cf-usage-collector-adr-window-end-selection`: an entry whose
    // period ends exactly on the upper bound belongs to the NEXT range. This
    // is what makes adjacent ranges sum without double counting.
    let range = TimeRange::new(at(1_700_000_000), at(1_700_003_600)).expect("range");
    assert!(!range.contains_window_end(at(1_700_003_600)));
}

#[test]
fn selection_holds_inside_the_range() {
    // Every other test passes a window_end equal to a bound, or past the
    // upper one. Nothing pins the ordinary case: a window_end strictly
    // between from and to.
    let range = TimeRange::new(at(1_700_000_000), at(1_700_003_600)).expect("range");
    assert!(range.contains_window_end(at(1_700_001_800)));
}

#[test]
fn an_entry_ending_before_the_lower_bound_is_not_selected() {
    // Without this, a predicate that only checks the upper bound (dropping
    // the `from <=` conjunct) is indistinguishable from the correct one.
    let range = TimeRange::new(at(1_700_000_000), at(1_700_003_600)).expect("range");
    assert!(!range.contains_window_end(at(1_699_999_999)));
}

#[test]
fn an_entry_wider_than_the_range_is_selected_by_exactly_one_range() {
    // The predicate reads window_end and nothing else, so an entry covering
    // [range.from - 1h, range.to + 1h) is invisible to `range` and to the
    // range before it — but the partition property in
    // `cpt-cf-usage-collector-adr-window-end-selection` means some range does
    // select it: the one whose upper bound reaches its window_end.
    let range = TimeRange::new(at(1_700_000_000), at(1_700_003_600)).expect("range");
    let wide_entry_end = range.upper_exclusive() + Duration::hours(1);
    assert!(!range.contains_window_end(wide_entry_end));

    let earlier = TimeRange::new(at(1_699_996_400), at(1_700_000_000)).expect("range");
    assert!(!earlier.contains_window_end(wide_entry_end));

    let next = TimeRange::new(range.upper_exclusive(), wide_entry_end + Duration::hours(1))
        .expect("range");
    assert!(next.contains_window_end(wide_entry_end));
}

#[test]
fn canonical_form_pins_the_exact_rendering() {
    // The string is compared across a page boundary — the follow-up
    // request renders it again from its own `from` / `to` and the two have
    // to be byte-identical — so the rendering is a wire-visible constant
    // and not an implementation detail. Pinned literally: a "reasonable"
    // reformat (padding, a different separator, seconds instead of
    // nanoseconds) refuses every cursor minted under the old spelling, and
    // a test that only compared two `canonical_form` calls to each other
    // would stay green through all of it.
    let range = TimeRange::new(at(1_700_000_000), at(1_700_003_600)).expect("range");
    assert_eq!(
        range.canonical_form(),
        "1700000000000000000~1700003600000000000",
    );
}

#[test]
fn canonical_form_separates_ranges_differing_only_in_one_bound() {
    // Both bounds have to reach the string. A rendering that dropped
    // either would fingerprint two different ranges identically, which is
    // exactly the cursor confusion the fingerprint exists to prevent.
    let base = TimeRange::new(at(1_700_000_000), at(1_700_003_600)).expect("range");
    let later_upper = TimeRange::new(at(1_700_000_000), at(1_700_007_200)).expect("range");
    let later_lower = TimeRange::new(at(1_700_000_001), at(1_700_003_600)).expect("range");

    assert_ne!(base.canonical_form(), later_upper.canonical_form());
    assert_ne!(base.canonical_form(), later_lower.canonical_form());
}

#[test]
fn canonical_form_separates_ranges_differing_only_below_the_microsecond() {
    // The truncation hole. `id::canonical_period_bound` renders a fixed
    // six-digit microsecond fraction, and `TimeRange::new` applies no
    // precision check, so composing the two here would collapse these two
    // ranges onto one string — and a cursor minted under either would then
    // validate against the other.
    let plain = TimeRange::new(at(1_700_000_000), at(1_700_003_600)).expect("range");
    let nudged = TimeRange::new(
        at(1_700_000_000),
        at(1_700_003_600) + Duration::nanoseconds(1),
    )
    .expect("range");

    assert_ne!(
        plain.canonical_form(),
        nudged.canonical_form(),
        "a sub-microsecond difference must survive the rendering",
    );
}

#[test]
fn canonical_form_is_the_same_for_two_spellings_of_one_instant() {
    // The other half of injectivity: the rendering must not manufacture a
    // difference either. `13:00:00+01:00` and `12:00:00Z` are one instant,
    // so a caller who paged with the first and continued with the second
    // must not be refused — the same equivalence `TimeRange::new`'s UTC
    // normalization establishes, restated where it becomes wire-visible.
    let offset = UtcOffset::from_hms(1, 0, 0).expect("valid offset");
    let utc = TimeRange::new(at(1_700_000_000), at(1_700_003_600)).expect("range");
    let shifted = TimeRange::new(
        at(1_700_000_000).to_offset(offset),
        at(1_700_003_600).to_offset(offset),
    )
    .expect("range");

    assert_eq!(utc.canonical_form(), shifted.canonical_form());
}
