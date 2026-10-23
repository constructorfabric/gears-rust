#![allow(clippy::expect_used, clippy::unwrap_used)]

use time::macros::{date, datetime};
use time::{OffsetDateTime, Time};

use super::{
    ChildKind, FactExtent, Interval, RatingWindow, RatingWindowKind, SplitInput, covered,
    hour_after, midnight, seconds, split_points, total_seconds, union, window_containing,
    window_geometry,
};
use crate::test_input::{PERIOD_END, PERIOD_START, usage_policy};

fn interval(start: OffsetDateTime, end: OffsetDateTime) -> Interval {
    Interval { start, end }
}

const PERIOD: Interval = Interval {
    start: PERIOD_START,
    end: PERIOD_END,
};

fn extent(served: Interval, period: Interval) -> FactExtent {
    FactExtent { served, period }
}

#[test]
fn a_billing_cycle_window_is_the_period_with_its_served_part() {
    let served = interval(datetime!(2026-09-11 00:00:00 UTC), PERIOD_END);
    let windows = window_geometry(
        &usage_policy(RatingWindowKind::BillingCycle),
        extent(served, PERIOD),
    )
    .unwrap();
    assert_eq!(
        windows,
        [RatingWindow {
            window_start: PERIOD_START,
            window_end: PERIOD_END,
            served_from: served.start,
            served_to: served.end,
        }]
    );
}

#[test]
fn calendar_hours_keep_their_canonical_bounds_and_clip_the_served_range() {
    // F29: activation at 10:30, served until 12:15.
    let served = interval(
        datetime!(2026-09-15 10:30:00 UTC),
        datetime!(2026-09-15 12:15:00 UTC),
    );
    let policy = usage_policy(RatingWindowKind::CalendarHourUtc);
    let windows = window_geometry(&policy, extent(served, PERIOD)).unwrap();
    let bounds: Vec<_> = windows
        .iter()
        .map(|w| {
            (
                w.window_start.hour(),
                w.served_from.minute(),
                w.served_to.minute(),
            )
        })
        .collect();
    assert_eq!(bounds, [(10, 30, 0), (11, 0, 0), (12, 0, 15)]);
    assert_eq!(windows[0].window_end, datetime!(2026-09-15 11:00:00 UTC));
    let containing = window_containing(
        &policy,
        extent(served, PERIOD),
        datetime!(2026-09-15 11:59:59 UTC),
    )
    .unwrap();
    assert_eq!(containing, windows[1]);
    let month = window_geometry(&policy, extent(PERIOD, PERIOD)).unwrap();
    assert_eq!(month.len(), 720);
}

#[test]
fn a_served_extent_outside_the_period_has_no_window() {
    let policy = usage_policy(RatingWindowKind::BillingCycle);
    let beyond = interval(PERIOD_START, datetime!(2026-10-02 00:00:00 UTC));
    let error = window_geometry(&policy, extent(beyond, PERIOD)).unwrap_err();
    assert_eq!(error.reason_code(), "period_geometry_mismatch");
    let error = window_containing(&policy, extent(PERIOD, PERIOD), PERIOD_END).unwrap_err();
    assert_eq!(error.reason_code(), "period_geometry_mismatch");
}

#[test]
fn split_points_are_the_ends_on_and_quantity_changes_inside_the_served_range() {
    let window = RatingWindow {
        window_start: PERIOD_START,
        window_end: PERIOD_END,
        served_from: PERIOD_START,
        served_to: PERIOD_END,
    };
    let fifteenth = datetime!(2026-09-15 00:00:00 UTC);
    let input = SplitInput {
        kind: ChildKind::PeriodLine,
        window,
        // One inside, one at the period start, one after it; the inside one twice.
        ends_on: vec![
            date!(2026 - 09 - 15),
            date!(2026 - 09 - 01),
            date!(2026 - 10 - 20),
            date!(2026 - 09 - 15),
        ],
        quantity_intervals: vec![
            interval(PERIOD_START, fifteenth),
            interval(fifteenth, datetime!(2026-09-20 12:00:00 UTC)),
        ],
    };
    assert_eq!(
        split_points(&input),
        [fifteenth, datetime!(2026-09-20 12:00:00 UTC)]
    );
    // Quantity changes cut recurring lines only.
    let usage = SplitInput {
        kind: ChildKind::UsageWindow,
        ..input
    };
    assert_eq!(split_points(&usage), [fifteenth]);
}

#[test]
fn covered_time_is_the_slice_less_its_suspensions() {
    let slice = interval(PERIOD_START, PERIOD_END);
    let suspensions = [
        interval(
            datetime!(2026-08-20 00:00:00 UTC),
            datetime!(2026-09-03 00:00:00 UTC),
        ),
        interval(
            datetime!(2026-09-11 00:00:00 UTC),
            datetime!(2026-09-21 00:00:00 UTC),
        ),
    ];
    let parts = covered(slice, &suspensions);
    assert_eq!(
        parts,
        [
            interval(
                datetime!(2026-09-03 00:00:00 UTC),
                datetime!(2026-09-11 00:00:00 UTC)
            ),
            interval(datetime!(2026-09-21 00:00:00 UTC), PERIOD_END),
        ]
    );
    assert_eq!(total_seconds(&parts), 18 * 86_400);
    assert!(
        covered(
            interval(PERIOD_START, datetime!(2026-09-02 00:00:00 UTC)),
            &suspensions
        )
        .is_empty()
    );
}

#[test]
fn a_union_merges_overlapping_and_touching_intervals() {
    let day = |d: u8| {
        OffsetDateTime::new_utc(
            time::Date::from_calendar_date(2026, time::Month::September, d).unwrap(),
            Time::MIDNIGHT,
        )
    };
    let merged = union([
        interval(day(10), day(15)),
        interval(day(1), day(5)),
        interval(day(5), day(8)),
        interval(day(12), day(20)),
        interval(day(25), day(25)),
    ]);
    assert_eq!(
        merged,
        [interval(day(1), day(8)), interval(day(10), day(20))]
    );
}

#[test]
fn an_empty_interval_has_no_seconds() {
    let error = seconds(&interval(PERIOD_END, PERIOD_START), "test").unwrap_err();
    assert_eq!(error.reason_code(), "period_geometry_mismatch");
    assert_eq!(seconds(&PERIOD, "test").unwrap(), 2_592_000);
    assert_eq!(
        midnight(date!(2026 - 09 - 15)),
        datetime!(2026-09-15 00:00:00 UTC)
    );
}

#[test]
fn an_empty_or_early_served_extent_has_no_window() {
    let policy = usage_policy(RatingWindowKind::CalendarHourUtc);
    let empty = interval(PERIOD_START, PERIOD_START);
    assert!(window_geometry(&policy, extent(empty, PERIOD)).is_err());
    let early = interval(datetime!(2026-08-31 23:00:00 UTC), PERIOD_END);
    assert!(window_geometry(&policy, extent(early, PERIOD)).is_err());
    let served = interval(datetime!(2026-09-15 10:30:00 UTC), PERIOD_END);
    let before = window_containing(
        &policy,
        extent(served, PERIOD),
        datetime!(2026-09-15 10:00:00 UTC),
    );
    assert!(before.is_err());
}

#[test]
fn the_hour_after_the_last_representable_one_is_an_error() {
    let last = datetime!(9999-12-31 23:00:00 UTC);
    assert_eq!(
        hour_after(last).unwrap_err().reason_code(),
        "period_geometry_mismatch"
    );
}
