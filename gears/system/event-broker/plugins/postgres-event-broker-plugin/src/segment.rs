//! Segment sizing and bounds (`docs/DESIGN.md` §4.9).

use crate::settings::SegmentRule;

/// What the last segment of a partition saw, for sizing the next one under a
/// `period` rule.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Observed {
    pub events: i64,
    /// From the segment's first to its last `sequence_time`.
    pub seconds: f64,
}

/// The floor rate under a `period` rule, events per second: an idle partition
/// still gets segments of a sensible size.
const FLOOR_RATE: f64 = 1.0;

/// The span of the next segment: `events` exactly, or the observed rate times
/// the period, floored at one event per second.
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
            ((rate * period.as_secs_f64()).ceil() as i64).max(2)
        }
    }
}

/// Whether the next segment is due: once `last_sequence` passes half of the
/// segment covering `[first, end)`.
pub fn next_is_due(first: i64, end: i64, last_sequence: i64) -> bool {
    last_sequence >= first + (end - first).div_euclid(2)
}

/// `[from, to)` of a range partition from `pg_get_expr(relpartbound, oid)`,
/// e.g. `FOR VALUES FROM ('1') TO ('1000001')`.
pub fn parse_bound(expression: &str) -> Option<(i64, i64)> {
    let number = |text: &str| {
        text.trim()
            .trim_matches(|c| c == '(' || c == ')' || c == '\'')
            .parse()
            .ok()
    };
    let rest = expression.trim().strip_prefix("FOR VALUES FROM")?;
    let (from, to) = rest.split_once(" TO ")?;
    Some((number(from)?, number(to)?))
}
