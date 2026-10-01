use std::time::Duration;

use crate::segment::{Observed, next_is_due, parse_bound, span};
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
fn the_next_segment_is_due_at_half() {
    assert!(!next_is_due(1, 11, 5));
    assert!(next_is_due(1, 11, 6));
    assert!(next_is_due(1, 11, 10));
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
