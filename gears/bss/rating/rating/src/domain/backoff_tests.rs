use std::time::Duration;

use super::{Backoff, FIRST_PAUSE};

#[test]
fn the_pause_doubles_up_to_the_cap_and_starts_over_after_a_success() {
    let mut backoff = Backoff::new(Duration::from_secs(5));
    let mut upcoming = Vec::new();
    for _ in 0..5 {
        upcoming.push(backoff.upcoming());
        let pause = backoff.pause();
        let base = *upcoming.last().unwrap_or(&Duration::ZERO);
        assert!(
            pause >= base / 2 && pause <= base,
            "{pause:?} is not within [{base:?}/2, {base:?}]"
        );
    }
    assert_eq!(
        upcoming,
        [1, 2, 4, 5, 5].map(Duration::from_secs).to_vec(),
        "doubles from one second and stops at the cap"
    );
    backoff.reset();
    assert_eq!(backoff.upcoming(), FIRST_PAUSE);
}

#[test]
fn a_cap_below_a_second_is_the_first_pause() {
    let backoff = Backoff::new(Duration::from_millis(50));
    assert_eq!(backoff.upcoming(), Duration::from_millis(50));
}
