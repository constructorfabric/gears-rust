use std::collections::HashMap;

use event_broker_sdk::backend::Partition;
use uuid::Uuid;

use crate::maintenance::catalog::{Segment, SegmentState};
use crate::maintenance::prioritizer::Key;
use crate::maintenance::registry::{Check, Due, Layout, Registry, Urgency};

fn key(partition: u32) -> Key {
    (Uuid::from_u128(1), Partition::new(partition))
}

fn row(first: i64, end: i64, state: SegmentState, bytes: i64) -> Segment {
    Segment {
        first,
        end,
        state,
        bytes,
        newest_at: None,
    }
}

/// An expired segment, a sealed one, the open one being filled, and one
/// ahead.
fn written() -> Vec<Segment> {
    vec![
        row(1, 501, SegmentState::Expired, 24_576),
        row(501, 1001, SegmentState::Sealed, 24_576),
        row(1001, 200_001, SegmentState::Open, 0),
        row(200_001, 400_001, SegmentState::Open, 0),
    ]
}

#[test]
fn a_layout_holds_the_live_ranges_and_both_alarms() {
    assert_eq!(
        Layout::new(&[Segment::open(1, 501), Segment::open(501, 1001)]),
        Layout {
            ranges: vec![(1, 501), (501, 1001)],
            room_alarm: 1,
            seal_alarm: 500,
        }
    );
    assert_eq!(
        Layout::new(&written()),
        Layout {
            ranges: vec![(501, 1001), (1001, 200_001), (200_001, 400_001)],
            room_alarm: 1001,
            seal_alarm: 200_000,
        }
    );
    assert_eq!(
        Layout::new(&[]),
        Layout {
            ranges: vec![],
            room_alarm: i64::MIN,
            seal_alarm: i64::MAX,
        }
    );
}

#[test]
fn a_head_is_checked_against_both_alarms() {
    let registry = Registry::default();
    registry.replace(key(0), Layout::new(&written()));
    registry.replace(
        key(1),
        Layout::new(&[Segment::open(1, 501), Segment::open(501, 1001)]),
    );
    registry.replace(key(2), Layout::new(&[]));
    let heads = [
        (key(0), 1000),
        (key(0), 1001),
        (key(0), 200_000),
        (key(1), 0),
        (key(1), 1),
        (key(1), 500),
        (key(2), 0),
        (key(4), 0),
    ];
    assert_eq!(
        heads.map(|(key, head)| registry.check(key, head)),
        [
            Check::Known(Due::default()),
            Check::Known(Due {
                room: Some(Urgency::room(398_999)),
                seal: false,
            }),
            Check::Known(Due {
                room: Some(Urgency::room(200_000)),
                seal: true,
            }),
            Check::Known(Due::default()),
            Check::Known(Due {
                room: Some(Urgency::room(999)),
                seal: false,
            }),
            Check::Known(Due {
                room: Some(Urgency::room(500)),
                seal: true,
            }),
            Check::Known(Due {
                room: Some(Urgency::Urgent),
                seal: false,
            }),
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
fn replace_all_keeps_what_it_does_not_name_and_forget_drops() {
    let registry = Registry::default();
    let layout = || Layout::new(&[Segment::open(1, 501), Segment::open(501, 1001)]);
    registry.replace(key(0), layout());
    registry.replace(key(1), layout());
    registry.replace_all(HashMap::from([(key(1), layout()), (key(2), layout())]));
    registry.forget(key(2));
    assert_eq!(
        [key(0), key(1), key(2)].map(|key| registry.check(key, 0)),
        [
            Check::Known(Due::default()),
            Check::Known(Due::default()),
            Check::Unknown
        ]
    );
}
