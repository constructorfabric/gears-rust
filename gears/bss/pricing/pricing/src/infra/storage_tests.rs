use super::{stored_instant, stored_now};
use time::macros::datetime;

#[test]
fn an_instant_is_stored_at_its_microsecond() {
    assert_eq!(
        stored_instant(datetime!(2026-10-01 09:15:30.123_456_789 UTC)),
        datetime!(2026-10-01 09:15:30.123_456 UTC)
    );
    assert_eq!(
        stored_instant(datetime!(2026-10-01 09:15:30.999_999_999 UTC)),
        datetime!(2026-10-01 09:15:30.999_999 UTC),
        "cut, never rounded into the next second"
    );
    let whole = datetime!(2026-10-01 09:15:30.123_456 UTC);
    assert_eq!(stored_instant(whole), whole, "a whole microsecond is kept");
    assert_eq!(stored_now().nanosecond() % 1_000, 0);
}
