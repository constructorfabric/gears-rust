//! Window geometry, split points and covered seconds (rating DESIGN §3.3, 03 §4.3, 09 §4.1, §4.3;
//! feature 02 §3.4; T-D-75, T-D-77).
//!
//! - [`window_geometry`] gives the expected child windows of a fact from a usage binding's
//!   `rating_window`: `BillingCycle` → the fact's billing period; `CalendarHour{Utc}` → each UTC
//!   hour of the served extent, a partial hour keeping its canonical bounds with a clipped served
//!   range. Rating derives no anchor: the period is the fact's.
//! - [`split_points`] gives the interior cuts of a child: each binding's `ends_on` (its own end,
//!   never `effective_to`, pricing D-425) and, for a recurring line, each start and end of a
//!   committed quantity interval. Hours, billing-period boundaries and plan changes separate
//!   children, so none of them is a split point; coincident cuts collapse to one. Term slices
//!   (TARGET) are not carried by the fact yet, so they cut nothing. A cut adds a slice, never a
//!   child, and never resets the band axis.
//! - [`covered`] is a slice less the suspensions of a recurring line whose fact pauses recurring
//!   charges; its UTC seconds are the numerator of the covered fraction of T-D-77. A price's
//!   covered time is the [`union`] of its slices' covered parts, so two dimension values bound to
//!   one price share one floor (DESIGN 09 §4.2, V34).
//!
//! The functions are pure and shared by the pipeline (layout, expected children) and `evaluate`,
//! which recomputes the split points and fails `layout_mismatch` when the slices disagree.

// @cpt-algo:cpt-cf-bss-rating-algo-core-geometry:p1

use time::{Date, Duration, OffsetDateTime, Time};

use crate::error::EvaluationError;
use crate::input::{ChildKind, CoreUsagePolicy, Interval, RatingWindow, RatingWindowKind};

/// A fact's served extent and the billing period it lies in: named, so the two intervals cannot be
/// swapped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FactExtent {
    pub served: Interval,
    pub period: Interval,
}

/// What the split points of a child depend on (DESIGN 01 §3.3, `SplitInput`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SplitInput {
    pub kind: ChildKind,
    /// The child's window; only its served range `[served_from, served_to)` is cut.
    pub window: RatingWindow,
    /// The `ends_on` of each binding that prices the child, in any order.
    pub ends_on: Vec<Date>,
    /// The committed quantity intervals of a recurring line; ignored for any other kind.
    pub quantity_intervals: Vec<Interval>,
}

// @cpt-begin:cpt-cf-bss-rating-algo-core-geometry:p1:inst-geo-windows
/// The expected child windows of a fact for a usage binding's rating policy (T-D-75).
///
/// # Errors
///
/// `period_geometry_mismatch` when the served extent is empty or leaves the billing period.
pub fn window_geometry(
    policy: &CoreUsagePolicy,
    extent: FactExtent,
) -> Result<Vec<RatingWindow>, EvaluationError> {
    let FactExtent { served, period } = extent;
    check_served(served, period)?;
    match policy.rating_window {
        RatingWindowKind::BillingCycle => Ok(vec![billing_cycle(served, period)]),
        RatingWindowKind::CalendarHourUtc => {
            let mut windows = Vec::new();
            let mut start = hour_of(served.start);
            while start < served.end {
                let window = calendar_hour(start, served)?;
                start = window.window_end;
                windows.push(window);
            }
            Ok(windows)
        }
    }
}

/// The window of `policy` that contains `instant` of the served extent: what
/// [`window_geometry`] lists, without listing every hour of a period.
///
/// # Errors
///
/// As [`window_geometry`]; also when `instant` is not served.
pub fn window_containing(
    policy: &CoreUsagePolicy,
    extent: FactExtent,
    instant: OffsetDateTime,
) -> Result<RatingWindow, EvaluationError> {
    let FactExtent { served, period } = extent;
    check_served(served, period)?;
    if instant < served.start || instant >= served.end {
        return Err(EvaluationError::PeriodGeometryMismatch {
            detail: format!(
                "{instant} is outside the served extent [{}, {})",
                served.start, served.end
            ),
        });
    }
    Ok(match policy.rating_window {
        RatingWindowKind::BillingCycle => billing_cycle(served, period),
        RatingWindowKind::CalendarHourUtc => calendar_hour(hour_of(instant), served)?,
    })
}
// @cpt-end:cpt-cf-bss-rating-algo-core-geometry:p1:inst-geo-windows

// @cpt-begin:cpt-cf-bss-rating-algo-core-geometry:p1:inst-geo-splits
/// The ordered interior split instants of a child (DESIGN 09 §4.3).
#[must_use]
pub fn split_points(input: &SplitInput) -> Vec<OffsetDateTime> {
    let (from, to) = (input.window.served_from, input.window.served_to);
    let mut cuts: Vec<OffsetDateTime> = input.ends_on.iter().copied().map(midnight).collect();
    if input.kind == ChildKind::PeriodLine {
        for interval in &input.quantity_intervals {
            cuts.extend([interval.start, interval.end]);
        }
    }
    cuts.retain(|cut| from < *cut && *cut < to);
    cuts.sort_unstable();
    cuts.dedup();
    // @cpt-begin:cpt-cf-bss-rating-algo-core-geometry:p1:inst-geo-return
    cuts
    // @cpt-end:cpt-cf-bss-rating-algo-core-geometry:p1:inst-geo-return
}
// @cpt-end:cpt-cf-bss-rating-algo-core-geometry:p1:inst-geo-splits

/// The covered parts of `slice`: the slice less the `suspensions`, which are ordered and
/// disjoint (the validator checks it).
#[must_use]
pub fn covered(slice: Interval, suspensions: &[Interval]) -> Vec<Interval> {
    let mut parts = Vec::new();
    let mut from = slice.start;
    for suspension in suspensions {
        if suspension.end <= from || suspension.start >= slice.end {
            continue;
        }
        if from < suspension.start {
            parts.push(Interval {
                start: from,
                end: suspension.start,
            });
        }
        from = from.max(suspension.end);
    }
    if from < slice.end {
        parts.push(Interval {
            start: from,
            end: slice.end,
        });
    }
    parts
}

/// The union of `intervals` as ordered, disjoint intervals; touching intervals merge.
#[must_use]
pub fn union(intervals: impl IntoIterator<Item = Interval>) -> Vec<Interval> {
    let mut sorted: Vec<Interval> = intervals
        .into_iter()
        .filter(|interval| interval.start < interval.end)
        .collect();
    sorted.sort_by_key(|interval| (interval.start, interval.end));
    let mut merged: Vec<Interval> = Vec::with_capacity(sorted.len());
    for interval in sorted {
        match merged.last_mut() {
            Some(last) if interval.start <= last.end => last.end = last.end.max(interval.end),
            _ => merged.push(interval),
        }
    }
    merged
}

/// The UTC seconds of disjoint intervals.
#[must_use]
pub fn total_seconds(intervals: &[Interval]) -> u64 {
    intervals
        .iter()
        .map(|interval| whole_seconds(interval.start, interval.end))
        .sum()
}

/// The whole UTC seconds of a half-open interval.
///
/// # Errors
///
/// `period_geometry_mismatch` for an empty or reversed interval.
pub fn seconds(interval: &Interval, what: &str) -> Result<u64, EvaluationError> {
    if interval.start >= interval.end {
        return Err(EvaluationError::PeriodGeometryMismatch {
            detail: format!(
                "the {what} [{}, {}) has no length",
                interval.start, interval.end
            ),
        });
    }
    Ok(whole_seconds(interval.start, interval.end))
}

/// 00:00 UTC of `date`: where a binding's `effective_from` or `ends_on` cuts (DESIGN 09 §4.1).
#[must_use]
pub fn midnight(date: Date) -> OffsetDateTime {
    OffsetDateTime::new_utc(date, Time::MIDNIGHT)
}

/// The seconds from `start` to a later `end`; whole seconds by the validator.
fn whole_seconds(start: OffsetDateTime, end: OffsetDateTime) -> u64 {
    u64::try_from(end.unix_timestamp() - start.unix_timestamp()).unwrap_or(0)
}

/// The served extent is non-empty and inside the billing period.
fn check_served(served: Interval, period: Interval) -> Result<(), EvaluationError> {
    if period.start <= served.start && served.start < served.end && served.end <= period.end {
        return Ok(());
    }
    Err(EvaluationError::PeriodGeometryMismatch {
        detail: format!(
            "the served extent [{}, {}) is empty or leaves the billing period [{}, {})",
            served.start, served.end, period.start, period.end
        ),
    })
}

/// The billing period as the one window of a `BillingCycle` binding.
fn billing_cycle(served: Interval, period: Interval) -> RatingWindow {
    RatingWindow {
        window_start: period.start,
        window_end: period.end,
        served_from: served.start,
        served_to: served.end,
    }
}

/// The UTC hour starting at `start`, its served range clipped to the served extent.
fn calendar_hour(start: OffsetDateTime, served: Interval) -> Result<RatingWindow, EvaluationError> {
    let end = hour_after(start)?;
    Ok(RatingWindow {
        window_start: start,
        window_end: end,
        served_from: start.max(served.start),
        served_to: end.min(served.end),
    })
}

/// The instant an hour after `start`.
///
/// # Errors
///
/// `period_geometry_mismatch` past the last instant `time` represents.
pub fn hour_after(start: OffsetDateTime) -> Result<OffsetDateTime, EvaluationError> {
    start
        .checked_add(Duration::HOUR)
        .ok_or_else(|| EvaluationError::PeriodGeometryMismatch {
            detail: format!("the hour after {start} is beyond the representable instants"),
        })
}

/// The UTC hour that contains `instant`.
fn hour_of(instant: OffsetDateTime) -> OffsetDateTime {
    let utc = instant.to_offset(time::UtcOffset::UTC);
    OffsetDateTime::new_utc(
        utc.date(),
        Time::from_hms(utc.hour(), 0, 0).unwrap_or(Time::MIDNIGHT),
    )
}

#[cfg(test)]
#[path = "geometry_tests.rs"]
mod tests;
