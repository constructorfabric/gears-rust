//! Segment sizing and bounds (`docs/DESIGN.md` §4.9).

use crate::append::MAX_EVENTS_PER_APPEND;
use crate::settings::SegmentRule;

/// [`MAX_EVENTS_PER_APPEND`] as a sequence count.
#[allow(
    clippy::cast_possible_wrap,
    reason = "a thousand fits any integer type"
)]
const MAX_EVENTS_PER_APPEND_I64: i64 = MAX_EVENTS_PER_APPEND as i64;

/// What the last segment of a partition saw, for sizing the next one under a
/// `period` rule.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Observed {
    /// Events the segment holds.
    pub events: i64,
    /// From the segment's first to its last `sequence_time`.
    pub seconds: f64,
}

/// The floor rate under a `period` rule, events per second: an idle partition
/// still gets segments of a sensible size.
const FLOOR_RATE: f64 = 1.0;

/// The span of the next segment: `events` exactly, or the observed rate times
/// the period, floored at one event per second; never below one full
/// append, so a table's partition limit is not reached by a few appends.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    reason = "a span is a positive sequence count well inside f64's exact range"
)]
pub fn span(rule: SegmentRule, observed: Option<Observed>) -> i64 {
    match rule {
        SegmentRule::Events(events) => i64::try_from(events).unwrap_or(i64::MAX),
        SegmentRule::Period(period) => {
            let rate = observed
                .filter(|o| o.seconds > 0.0)
                .map_or(FLOOR_RATE, |o| {
                    (o.events as f64 / o.seconds).max(FLOOR_RATE)
                });
            ((rate * period.as_secs_f64()).ceil() as i64).max(MAX_EVENTS_PER_APPEND_I64)
        }
    }
}

/// Whether a partition whose head is `hwm` needs more segments: fewer than two
/// lie wholly above it, or they leave less room than one append can take.
/// `ranges` are the attached segments' `[first, end)`, oldest first.
pub fn needs_room(hwm: i64, ranges: &[(i64, i64)]) -> bool {
    let empty = ranges.iter().filter(|(first, _)| *first > hwm).count();
    let end = ranges.last().map_or(1, |(_, end)| *end);
    empty < 2 || end - 1 - hwm < MAX_EVENTS_PER_APPEND_I64
}

/// The lowest head at which [`needs_room`] holds for `ranges`, brought
/// forward by `margin` sequences. `needs_room` only grows with the head, so
/// one compare against this tells whether a head needs room.
pub fn room_alarm(ranges: &[(i64, i64)], margin: i64) -> i64 {
    let [.., (second_last, _), (_, end)] = ranges else {
        return i64::MIN;
    };
    // Fewer than two segments wholly above the head, or less than one
    // append's room below the end.
    let alarm = (*second_last).min(end - MAX_EVENTS_PER_APPEND_I64);
    alarm.saturating_sub(margin)
}

/// The `[first, end)` of each segment to attach, `span` sequences each, so
/// that [`needs_room`] no longer holds; nothing when it does not.
pub fn ahead(hwm: i64, ranges: &[(i64, i64)], span: i64) -> Vec<(i64, i64)> {
    let mut planned = ranges.to_vec();
    let mut added = Vec::new();
    while needs_room(hwm, &planned) {
        let first = planned.last().map_or(1, |(_, end)| *end);
        planned.push((first, first + span));
        added.push((first, first + span));
    }
    added
}

/// The upper bound of a range partition from `information_schema.PARTITIONS`
/// `PARTITION_DESCRIPTION`, e.g. `1000001`; `None` for `MAXVALUE` or anything
/// else.
pub fn parse_bound(description: &str) -> Option<i64> {
    description.trim().parse().ok()
}

/// What one stored event weighs for a byte bound: its body and a fixed
/// allowance for the row's columns and `InnoDB`'s row header. Counted the
/// same way at append and when a segment is sealed, so the two agree.
pub const ROW_OVERHEAD: i64 = 48;

/// The weight of an event whose body is `body` bytes long.
pub fn weight(body: usize) -> i64 {
    i64::try_from(body)
        .unwrap_or(i64::MAX)
        .saturating_add(ROW_OVERHEAD)
}
