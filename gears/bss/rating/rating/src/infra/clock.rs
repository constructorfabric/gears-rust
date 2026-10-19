//! The wall clock.

use time::OffsetDateTime;

use crate::domain::ports::Clock;

/// UTC now, truncated to the microsecond PostgreSQL's `timestamptz` keeps, so an instant reads
/// back equal to the one written.
#[derive(Debug, Default, Clone, Copy)]
pub struct WallClock;

impl Clock for WallClock {
    fn now(&self) -> OffsetDateTime {
        let now = OffsetDateTime::now_utc();
        let micros = now.microsecond();
        now.replace_microsecond(micros).unwrap_or(now)
    }
}
