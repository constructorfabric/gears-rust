use time::{Duration, OffsetDateTime};

use super::{FIRST_BIND_RETRY, backoff};

#[test]
fn the_binding_retry_doubles_from_ten_seconds_to_five_minutes() {
    let received = OffsetDateTime::UNIX_EPOCH;
    assert_eq!(backoff(received, received), received + FIRST_BIND_RETRY);
    let later = received + Duration::seconds(40);
    assert_eq!(backoff(later, received), later + Duration::seconds(40));
    let much_later = received + Duration::hours(2);
    assert_eq!(
        backoff(much_later, received),
        much_later + Duration::minutes(5)
    );
}
