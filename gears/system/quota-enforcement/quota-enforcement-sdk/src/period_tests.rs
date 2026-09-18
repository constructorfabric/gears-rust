use time::{Duration, OffsetDateTime, UtcOffset};

use crate::models::{PeriodType, PeriodWindow};

/// Parse an RFC 3339 instant, so each expectation reads as the date it means.
fn at(text: &str) -> OffsetDateTime {
    OffsetDateTime::parse(text, &time::format_description::well_known::Rfc3339)
        .expect("test timestamp is valid RFC 3339")
}

fn rfc3339(value: OffsetDateTime) -> String {
    value
        .format(&time::format_description::well_known::Rfc3339)
        .expect("formattable timestamp")
}

// --- calendar alignment ----------------------------------------------------

#[test]
fn a_day_period_spans_one_utc_midnight_to_the_next() {
    let window = PeriodType::Day.window_containing(at("2026-03-17T13:45:09Z"));

    assert_eq!(window.start, at("2026-03-17T00:00:00Z"));
    assert_eq!(window.end, at("2026-03-18T00:00:00Z"));
    assert_eq!(window.next_reset, window.end);
}

#[test]
fn a_week_period_starts_on_monday_whichever_day_it_contains() {
    // 2026-03-17 is a Tuesday; 2026-03-22 the Sunday of the same week.
    let from_tuesday = PeriodType::Week.window_containing(at("2026-03-17T13:45:09Z"));
    let from_sunday = PeriodType::Week.window_containing(at("2026-03-22T23:59:59Z"));

    assert_eq!(from_tuesday.start, at("2026-03-16T00:00:00Z"));
    assert_eq!(from_tuesday.end, at("2026-03-23T00:00:00Z"));
    assert_eq!(from_sunday, from_tuesday);
}

#[test]
fn a_week_period_containing_a_monday_starts_that_same_day() {
    let window = PeriodType::Week.window_containing(at("2026-03-16T00:00:00Z"));

    assert_eq!(window.start, at("2026-03-16T00:00:00Z"));
}

#[test]
fn a_month_period_covers_a_leap_february_to_its_last_day() {
    let window = PeriodType::Month.window_containing(at("2028-02-29T12:00:00Z"));

    assert_eq!(window.start, at("2028-02-01T00:00:00Z"));
    assert_eq!(window.end, at("2028-03-01T00:00:00Z"));
}

#[test]
fn a_december_month_period_rolls_into_january_of_the_next_year() {
    let window = PeriodType::Month.window_containing(at("2026-12-31T23:59:59Z"));

    assert_eq!(window.start, at("2026-12-01T00:00:00Z"));
    assert_eq!(window.end, at("2027-01-01T00:00:00Z"));
}

#[test]
fn a_year_period_spans_january_to_january() {
    let window = PeriodType::Year.window_containing(at("2026-07-04T00:00:00Z"));

    assert_eq!(window.start, at("2026-01-01T00:00:00Z"));
    assert_eq!(window.end, at("2027-01-01T00:00:00Z"));
}

// --- boundaries ------------------------------------------------------------

#[test]
fn the_end_instant_belongs_to_the_next_period_not_the_closing_one() {
    let window = PeriodType::Day.window_containing(at("2026-03-17T10:00:00Z"));

    assert!(!window.contains(window.end));
    assert!(window.contains(window.end - Duration::nanoseconds(1)));
    assert_eq!(
        PeriodType::Day.window_containing(window.end).start,
        window.end,
        "an operation at exactly the boundary opens the successor period"
    );
}

#[test]
fn a_window_has_elapsed_from_its_end_instant_onwards() {
    let window = PeriodType::Day.window_containing(at("2026-03-17T10:00:00Z"));

    assert!(!window.has_elapsed(window.end - Duration::seconds(1)));
    assert!(window.has_elapsed(window.end));
    assert!(window.has_elapsed(window.end + Duration::seconds(1)));
}

#[test]
fn a_non_utc_input_is_normalized_before_the_calendar_is_read() {
    // 00:30 on March 2nd at +02:00 is 22:30 on March 1st in UTC.
    let local = at("2026-03-02T00:30:00Z")
        .to_offset(UtcOffset::from_hms(2, 0, 0).expect("valid offset"))
        + Duration::hours(2);
    let window = PeriodType::Day.window_containing(local);

    assert_eq!(window.start, at("2026-03-02T00:00:00Z"));
}

// --- the one-time sentinel -------------------------------------------------

#[test]
fn a_one_time_period_never_elapses_and_is_marked_open_ended() {
    let window = PeriodType::OneTime.window_containing(at("2026-03-17T10:00:00Z"));

    assert!(window.is_open_ended());
    assert!(!window.has_elapsed(at("3000-01-01T00:00:00Z")));
    assert!(window.contains(at("2026-03-17T10:00:00Z")));
    assert!(!PeriodType::OneTime.is_recurring());
    assert!(PeriodType::Day.is_recurring());
}

#[test]
fn the_open_end_sentinel_is_the_documented_instant_and_formats_as_rfc3339() {
    assert_eq!(rfc3339(PeriodWindow::open_end()), "9999-12-31T23:59:59Z");
}

#[test]
fn every_calendar_period_containing_an_instant_actually_contains_it() {
    let instant = at("2026-11-30T23:59:59Z");
    for period in PeriodType::ALL {
        assert!(
            period.window_containing(instant).contains(instant),
            "{period} window excludes the instant it was built from"
        );
    }
}
