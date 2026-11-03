use chrono::{TimeZone, Utc};

use crate::clock::{instant, micros, micros_ceil};

#[test]
fn whole_microseconds_round_trip() {
    let at = Utc
        .timestamp_opt(1_767_225_600, 123_456_000)
        .single()
        .unwrap();
    assert_eq!(micros(at), 1_767_225_600_123_456);
    assert_eq!(micros_ceil(at), 1_767_225_600_123_456);
    assert_eq!(instant(1_767_225_600_123_456).unwrap(), at);
}

#[test]
fn a_fraction_of_a_microsecond_rounds_down_or_up() {
    let at = Utc
        .timestamp_opt(1_767_225_600, 123_456_001)
        .single()
        .unwrap();
    assert_eq!(micros(at), 1_767_225_600_123_456);
    assert_eq!(micros_ceil(at), 1_767_225_600_123_457);
}

#[test]
fn an_instant_out_of_range_is_internal() {
    assert_eq!(
        format!("{:?}", instant(i64::MAX)),
        r#"Err(Internal("a stored instant is out of range"))"#
    );
}
