//! The wall clock.

use time::OffsetDateTime;

use crate::domain::ports::Clock;

/// UTC now, truncated to the microsecond PostgreSQL's `timestamptz` keeps, so an instant reads
/// back equal to the one written.
#[derive(Debug, Default, Clone, Copy)]
pub struct WallClock;

impl Clock for WallClock {
    fn now(&self) -> OffsetDateTime {
        to_micros(OffsetDateTime::now_utc())
    }
}

/// `at` without its sub-microsecond nanoseconds.
fn to_micros(at: OffsetDateTime) -> OffsetDateTime {
    at.replace_microsecond(at.microsecond()).unwrap_or(at)
}

#[cfg(test)]
#[path = "clock_tests.rs"]
mod tests;
