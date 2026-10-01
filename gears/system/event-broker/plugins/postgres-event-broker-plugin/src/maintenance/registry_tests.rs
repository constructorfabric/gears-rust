use std::collections::HashMap;

use event_broker_sdk::backend::Partition;
use uuid::Uuid;

use crate::maintenance::catalog::{Segment, SegmentState};
use crate::maintenance::prioritizer::Key;
use crate::maintenance::registry::{Check, Layout, Registry, Urgency};

fn key(partition: u32) -> Key {
    (Uuid::from_u128(1), Partition::new(partition))
}

fn row(first: i64, end: i64, state: SegmentState, counted_to: i64) -> Segment {
    Segment {
        first,
        end,
        state,
        counted_to,
        newest_at: None,
    }
}

/// An expired segment, a sealed one, the open one counted to 600, and one
/// ahead.
fn written() -> Vec<Segment> {
    vec![
        row(1, 501, SegmentState::Expired, 500),
        row(501, 1001, SegmentState::Sealed, 1000),
        row(1001, 200_001, SegmentState::Open, 600),
        row(200_001, 400_001, SegmentState::Open, 200_000),
    ]
}

#[test]
fn a_layout_holds_the_live_ranges_and_both_alarms() {
    assert_eq!(
        Layout::new(&[Segment::open(1, 501), Segment::open(501, 1001)], false),
        Layout {
            ranges: vec![(1, 501), (501, 1001)],
            room_alarm: 1,
            count_alarm: 500,
            bounded: false,
        }
    );
    assert_eq!(
        Layout::new(&written(), true),
        Layout {
            ranges: vec![(501, 1001), (1001, 200_001), (200_001, 400_001)],
            room_alarm: 1001,
            count_alarm: 100_600,
            bounded: true,
        }
    );
    assert_eq!(
        Layout::new(&[], true),
        Layout {
            ranges: vec![],
            room_alarm: i64::MIN,
            count_alarm: i64::MAX,
            bounded: true,
        }
    );
}

#[test]
fn a_head_is_checked_against_both_alarms() {
    let registry = Registry::default();
    registry.replace(key(0), Layout::new(&written(), true));
    registry.replace(
        key(1),
        Layout::new(&[Segment::open(1, 501), Segment::open(501, 1001)], false),
    );
    registry.replace(key(2), Layout::new(&[], false));
    // Big segments: the open one is due a count long before room is short.
    registry.replace(
        key(3),
        Layout::new(
            &[
                Segment::open(1, 1_000_001),
                Segment::open(1_000_001, 2_000_001),
                Segment::open(2_000_001, 3_000_001),
            ],
            false,
        ),
    );
    let heads = [
        (key(0), 1000),
        (key(0), 1001),
        (key(0), 100_600),
        (key(1), 0),
        (key(1), 1),
        (key(1), 500),
        (key(2), 0),
        (key(3), 99_999),
        (key(3), 100_000),
        (key(4), 0),
    ];
    assert_eq!(
        heads.map(|(key, head)| registry.check(key, head)),
        [
            Check::Fine,
            Check::Room(Urgency::room(398_999)),
            Check::RoomAndCount(Urgency::room(299_400)),
            Check::Fine,
            Check::Room(Urgency::room(999)),
            Check::RoomAndCount(Urgency::room(500)),
            Check::Room(Urgency::Urgent),
            Check::Fine,
            Check::Count,
            Check::Unknown,
        ]
    );
}

#[test]
fn less_room_is_more_urgent_and_urgent_beats_any_room() {
    let mut urgencies = [
        Urgency::room(10),
        Urgency::Urgent,
        Urgency::room(0),
        Urgency::room(-5),
        Urgency::room(1000),
    ];
    urgencies.sort();
    assert_eq!(
        urgencies,
        [
            Urgency::room(1000),
            Urgency::room(10),
            Urgency::room(0),
            Urgency::room(-5),
            Urgency::Urgent,
        ]
    );
}

#[test]
fn replace_all_and_forget_drop_partitions() {
    let registry = Registry::default();
    let layout = || Layout::new(&[Segment::open(1, 501), Segment::open(501, 1001)], false);
    registry.replace(key(0), layout());
    registry.replace(key(1), layout());
    registry.replace_all(HashMap::from([(key(1), layout()), (key(2), layout())]));
    registry.forget(key(2));
    assert_eq!(
        [key(0), key(1), key(2)].map(|key| registry.check(key, 0)),
        [Check::Unknown, Check::Fine, Check::Unknown]
    );
}
