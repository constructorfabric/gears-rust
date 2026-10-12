use super::{stored_instant, stored_now};
use time::OffsetDateTime;

fn at(nanos: i128) -> OffsetDateTime {
    OffsetDateTime::from_unix_timestamp_nanos(nanos).unwrap()
}

#[test]
fn an_instant_is_stored_at_its_microsecond() {
    assert_eq!(
        stored_instant(at(1_790_000_000_123_456_789)),
        at(1_790_000_000_123_456_000)
    );
    assert_eq!(
        stored_instant(at(1_790_000_000_999_999_999)),
        at(1_790_000_000_999_999_000),
        "cut, never rounded into the next second"
    );
    let whole = at(1_790_000_000_123_456_000);
    assert_eq!(stored_instant(whole), whole, "a whole microsecond is kept");
    assert_eq!(stored_now().nanosecond() % 1_000, 0);
}
