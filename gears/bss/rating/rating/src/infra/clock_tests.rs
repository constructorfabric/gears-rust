use time::OffsetDateTime;

use super::{WallClock, to_micros};
use crate::domain::ports::Clock;

fn at(nanos: i128) -> OffsetDateTime {
    OffsetDateTime::from_unix_timestamp_nanos(nanos).unwrap()
}

#[test]
fn an_instant_keeps_its_microseconds_and_drops_the_nanoseconds_below_them() {
    let second: i128 = 1_792_411_200 * 1_000_000_000;
    assert_eq!(
        to_micros(at(second + 123_456_789)),
        at(second + 123_456_000)
    );
    // Truncated, never rounded up into the next microsecond.
    assert_eq!(
        to_micros(at(second + 999_999_999)),
        at(second + 999_999_000)
    );
}

#[test]
fn the_wall_clock_reads_whole_microseconds() {
    assert_eq!(WallClock.now().nanosecond() % 1_000, 0);
}
