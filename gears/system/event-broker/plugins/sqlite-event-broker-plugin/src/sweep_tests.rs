use uuid::Uuid;

use crate::sweep::{Bounded, past_bytes};

#[test]
fn the_cut_is_the_first_event_that_sheds_the_excess() {
    let oldest = [(4, 100), (5, 100), (6, 100)];
    assert_eq!(past_bytes(&oldest, 1), Some(4));
    assert_eq!(past_bytes(&oldest, 100), Some(4));
    assert_eq!(past_bytes(&oldest, 101), Some(5));
    assert_eq!(past_bytes(&oldest, 300), Some(6));
}

#[test]
fn a_batch_short_of_the_excess_goes_whole() {
    assert_eq!(past_bytes(&[(4, 100), (5, 100)], 1000), Some(5));
}

#[test]
fn nothing_stored_cuts_nothing() {
    assert_eq!(past_bytes(&[], 10), None);
}

const NOW: i64 = 1_767_225_600_000_000;

fn bounded(
    max_age_ms: Option<i64>,
    max_bytes: Option<i64>,
    bytes: i64,
    oldest: Option<i64>,
) -> Bounded {
    Bounded {
        topic_uuid: Uuid::from_u128(1),
        partition: 0,
        max_age_ms,
        max_bytes,
        bytes,
        oldest_time: oldest,
    }
}

#[test]
fn a_partition_within_its_bounds_is_left_unlocked() {
    let within = [
        bounded(Some(1000), None, 10, Some(NOW - 1_000_000)),
        bounded(Some(1000), None, 10, Some(NOW)),
        bounded(None, Some(10), 10, Some(NOW - 9_000_000_000)),
        bounded(Some(1000), Some(10), 10, None),
    ];
    for partition in within {
        assert!(!partition.crossed(NOW), "{partition:?}");
    }
}

#[test]
fn a_partition_past_either_bound_is_swept() {
    let past = [
        bounded(Some(1000), None, 10, Some(NOW - 1_000_001)),
        bounded(None, Some(10), 11, Some(NOW)),
        bounded(Some(1000), Some(10), 11, Some(NOW)),
        bounded(Some(i64::MAX), Some(0), 1, Some(NOW)),
    ];
    for partition in past {
        assert!(partition.crossed(NOW), "{partition:?}");
    }
}
