use event_broker_sdk::backend::Partition;
use uuid::Uuid;

use super::catalog::{Segment, SegmentState};
use super::hub::{Hub, Layout};

fn segment(first: i64, end: i64, state: SegmentState) -> Segment {
    Segment {
        state,
        ..Segment::adding(first, end)
    }
}

#[test]
fn a_layout_alarms_at_short_room_and_a_full_open_segment() {
    let layout = Layout::new(&[
        segment(1, 11, SegmentState::Expired),
        segment(11, 1011, SegmentState::Sealed),
        segment(1011, 2011, SegmentState::Open),
        segment(2011, 3011, SegmentState::Open),
    ]);
    assert_eq!(
        layout,
        Layout {
            room_alarm: 1011,
            seal_alarm: 2010,
        }
    );
    assert_eq!(
        [1000, 1011, 2010].map(|head| layout.is_due(head)),
        [false, true, true]
    );
}

#[test]
fn appends_offer_unknown_and_due_partitions_only() {
    let hub = Hub::default();
    let known = (Uuid::from_u128(1), Partition::new(0));
    let unknown = (Uuid::from_u128(2), Partition::new(0));
    hub.replace(
        known,
        Layout {
            room_alarm: 100,
            seal_alarm: 200,
        },
    );
    hub.appended(known, 50);
    hub.appended(unknown, 1);
    assert_eq!(hub.queue.take(), [unknown]);
    hub.appended(known, 100);
    assert_eq!(hub.queue.take(), [known]);
    hub.forget(known);
    hub.appended(known, 1);
    assert_eq!(hub.queue.take(), [known]);
}
