use std::time::Duration;

use crate::segment::{Observed, ahead, needs_room, parse_bound, room_alarm, span};
use crate::settings::SegmentRule;

#[test]
fn an_events_rule_is_exact() {
    assert_eq!(span(SegmentRule::Events(1_000_000), None), 1_000_000);
}

#[test]
fn a_period_rule_follows_the_observed_rate() {
    let hour = SegmentRule::Period(Duration::from_hours(1));
    // 7200 events over 3600 s: 2 events/s for an hour.
    assert_eq!(
        span(
            hour,
            Some(Observed {
                events: 7200,
                seconds: 3600.0
            })
        ),
        7200
    );
}

#[test]
fn a_period_rule_is_floored_at_one_event_per_second() {
    let hour = SegmentRule::Period(Duration::from_hours(1));
    assert_eq!(span(hour, None), 3600);
    assert_eq!(
        span(
            hour,
            Some(Observed {
                events: 10,
                seconds: 3600.0
            })
        ),
        3600
    );
    assert_eq!(
        span(
            hour,
            Some(Observed {
                events: 1,
                seconds: 0.0
            })
        ),
        3600
    );
}

#[test]
fn room_is_two_empty_segments_and_one_append() {
    // Two segments wholly above the head, and 1000 sequences of room.
    assert!(!needs_room(0, &[(1, 1001), (1001, 2001)]));
    // One of them is in use now.
    assert!(needs_room(1, &[(1, 1001), (1001, 2001)]));
    // Two empty segments, but too small for one append.
    assert!(needs_room(0, &[(1, 11), (11, 21)]));
    assert!(needs_room(0, &[]));
}

#[test]
fn ahead_plans_segments_until_there_is_room() {
    assert_eq!(
        ahead(0, &[], 1_000_000),
        [(1, 1_000_001), (1_000_001, 2_000_001)]
    );
    assert_eq!(
        ahead(1, &[(1, 1_000_001), (1_000_001, 2_000_001)], 1_000_000),
        [(2_000_001, 3_000_001)]
    );
    assert_eq!(
        ahead(0, &[(1, 1_000_001), (1_000_001, 2_000_001)], 1_000_000),
        []
    );
    // Small segments: as many as one append needs.
    assert_eq!(
        ahead(250, &[(1, 251)], 250),
        [(251, 501), (501, 751), (751, 1001), (1001, 1251)]
    );
}

#[test]
fn a_bound_parses_quoted_and_bare() {
    assert_eq!(
        parse_bound("FOR VALUES FROM ('1') TO ('1000001')"),
        Some((1, 1_000_001))
    );
    assert_eq!(parse_bound("FOR VALUES FROM (11) TO (21)"), Some((11, 21)));
    assert_eq!(parse_bound("DEFAULT"), None);
}

#[test]
fn the_room_alarm_is_the_first_head_that_needs_room() {
    let cases: [&[(i64, i64)]; 5] = [
        &[],
        &[(1, 1001)],
        &[(1, 501), (501, 1001)],
        &[(1, 1_000_001), (1_000_001, 2_000_001)],
        &[(1, 251), (251, 501), (501, 751), (751, 1001), (1001, 1251)],
    ];
    let alarms = cases.map(|ranges| {
        let alarm = room_alarm(ranges, 0);
        let first_needing = (alarm.max(-1)..=2_000_000).find(|h| needs_room(*h, ranges));
        (alarm, first_needing)
    });
    assert_eq!(
        alarms,
        [
            (i64::MIN, Some(-1)),
            (i64::MIN, Some(-1)),
            (1, Some(1)),
            (1, Some(1)),
            (251, Some(251)),
        ]
    );
    assert_eq!(room_alarm(&[(1, 501), (501, 1001)], 100), -99);
}
