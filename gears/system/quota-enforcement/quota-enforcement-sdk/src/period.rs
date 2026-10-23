//! Calendar-aligned period arithmetic (PRD section 5.4).
//!
//! Every period is a half-open UTC window `[start, end)`. A period is current
//! when it contains the instant in question, so an operation at exactly
//! `period_end` belongs to the next period, never the closing one: the
//! predicate "closed" is `now >= end` everywhere in the system.
//!
//! One-time Quotas have no reset. They still carry a window, ending at the
//! sentinel [`PeriodWindow::OPEN_END`], so that storage columns stay
//! `NOT NULL` and the closed predicate needs no special case: it is simply
//! never true before the year 10000.

use time::{Date, Duration, Month, OffsetDateTime, PrimitiveDateTime, Time, UtcOffset};

use crate::models::{PeriodType, PeriodWindow};

/// `9999-12-31T23:59:59Z` as a Unix timestamp.
const OPEN_END_TIMESTAMP: i64 = 253_402_300_799;

/// Midnight UTC of `date`.
fn midnight(date: Date) -> OffsetDateTime {
    PrimitiveDateTime::new(date, Time::MIDNIGHT).assume_utc()
}

/// The first day of the month after `date`'s, handling the December wrap.
fn first_of_next_month(date: Date) -> Option<Date> {
    let (year, month) = match date.month() {
        Month::December => (date.year().checked_add(1)?, Month::January),
        other => (date.year(), other.next()),
    };
    Date::from_calendar_date(year, month, 1).ok()
}

impl PeriodWindow {
    /// End instant of a period that never resets.
    ///
    /// An explicit far-future constant rather than the maximum representable
    /// date: the maximum depends on the `time` crate's `large-dates` feature,
    /// which another crate in the same build could enable, and a sentinel that
    /// moved would change every stored row's meaning. This value also formats
    /// as RFC 3339 without a sign prefix.
    #[must_use]
    pub fn open_end() -> OffsetDateTime {
        OffsetDateTime::from_unix_timestamp(OPEN_END_TIMESTAMP)
            .unwrap_or(OffsetDateTime::UNIX_EPOCH)
    }

    /// Whether `at` falls in this window. Half-open: the end instant does not.
    #[must_use]
    pub fn contains(&self, at: OffsetDateTime) -> bool {
        at >= self.start && at < self.end
    }

    /// Whether the window has ended by `now`. The inverse of being current.
    #[must_use]
    pub fn has_elapsed(&self, now: OffsetDateTime) -> bool {
        now >= self.end
    }

    /// Whether this window is the one-time sentinel, which never elapses.
    #[must_use]
    pub fn is_open_ended(&self) -> bool {
        self.end == Self::open_end()
    }
}

impl PeriodType {
    /// Whether the period resets on a calendar boundary.
    #[must_use]
    pub const fn is_recurring(self) -> bool {
        !matches!(self, Self::OneTime)
    }

    /// The window containing `at`, aligned to the UTC calendar.
    ///
    /// `at` is normalized to UTC first, so a caller's local offset never
    /// shifts the boundary: `2026-03-02T00:30+02:00` is `2026-03-01T22:30Z`
    /// and belongs to March 1st.
    ///
    /// A calendar computation that cannot be represented (only reachable near
    /// the maximum year) falls back to the open-ended window rather than
    /// panicking: an unbounded period denies nothing that a bounded one would
    /// have allowed.
    #[must_use]
    pub fn window_containing(self, at: OffsetDateTime) -> PeriodWindow {
        let at = at.to_offset(UtcOffset::UTC);
        let date = at.date();
        let bounds = match self {
            Self::Day => date.next_day().map(|next| (midnight(date), midnight(next))),
            Self::Week => {
                let since_monday = i64::from(date.weekday().number_days_from_monday());
                date.checked_sub(Duration::days(since_monday))
                    .and_then(|monday| {
                        let next = monday.checked_add(Duration::days(7))?;
                        Some((midnight(monday), midnight(next)))
                    })
            }
            Self::Month => Date::from_calendar_date(date.year(), date.month(), 1)
                .ok()
                .and_then(|first| Some((midnight(first), midnight(first_of_next_month(date)?)))),
            Self::Year => date.year().checked_add(1).and_then(|next_year| {
                let first = Date::from_calendar_date(date.year(), Month::January, 1).ok()?;
                let next = Date::from_calendar_date(next_year, Month::January, 1).ok()?;
                Some((midnight(first), midnight(next)))
            }),
            Self::OneTime => Some((OffsetDateTime::UNIX_EPOCH, PeriodWindow::open_end())),
        };
        let (start, end) = bounds.unwrap_or((OffsetDateTime::UNIX_EPOCH, PeriodWindow::open_end()));
        PeriodWindow {
            start,
            end,
            // The next reset is the boundary itself. A one-time period never
            // resets, and its sentinel says so.
            next_reset: end,
        }
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "period_tests.rs"]
mod tests;
