use super::*;

fn wall() -> OffsetDateTime {
    OffsetDateTime::from_unix_timestamp(1_900_000_000).unwrap()
}

#[test]
fn a_deadline_tracks_the_remaining_lifetime_on_both_clocks() {
    let (wall, now) = (wall(), Instant::now());
    let absolute = wall + Duration::from_secs(90);
    let deadline = Deadline::at(absolute, wall, now);
    assert_eq!(deadline.monotonic, now + Duration::from_secs(90));
    assert_eq!(deadline.absolute, absolute);
    assert!(deadline.usable(now + Duration::from_secs(89), wall));
    assert!(!deadline.usable(now + Duration::from_secs(90), wall));
    assert!(!deadline.usable(now, absolute));
}

#[test]
fn a_far_future_deadline_is_tracked_monotonically_for_at_most_a_year() {
    let (wall, now) = (wall(), Instant::now());
    let absolute = wall + Duration::from_hours(2 * 8760);
    let deadline = Deadline::at(absolute, wall, now);
    let one_year = Duration::from_hours(365 * 24);
    assert_eq!(deadline.monotonic, now + one_year);
    assert_eq!(deadline.absolute, absolute);
    // Far beyond what `Instant` can represent on every platform.
    let year_9999 = OffsetDateTime::from_unix_timestamp(253_402_300_799).unwrap();
    assert_eq!(Deadline::at(year_9999, wall, now).monotonic, now + one_year);
}

#[test]
fn an_elapsed_deadline_is_not_usable() {
    let (wall, now) = (wall(), Instant::now());
    let deadline = Deadline::at(wall - Duration::from_secs(1), wall, now);
    assert_eq!(deadline.monotonic, now);
    assert!(!deadline.usable(now, wall));
}
