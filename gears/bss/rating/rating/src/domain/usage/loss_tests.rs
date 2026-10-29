#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::time::Duration;

use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use super::{DayRange, LossKind, LossState, TaintedDays, retention_taint, unidentifiable_taint};
use crate::domain::usage::feed::MeterId;

fn at(text: &str) -> OffsetDateTime {
    OffsetDateTime::parse(text, &Rfc3339).unwrap()
}

fn meters() -> Vec<MeterId> {
    vec![
        MeterId::parse("gts.cf.core.uc.usage_record.v2~cf.test.a.v1~").unwrap(),
        MeterId::parse("gts.cf.core.uc.usage_record.v2~cf.test.b.v1~").unwrap(),
    ]
}

const FLOOR: Duration = Duration::from_hours(125 * 24);

#[test]
fn a_retention_loss_taints_one_floor_before_its_last_good_page_to_one_before_detection() {
    // V71: a page stuck for 130 days, then the cursor is refused.
    let last_good_at = at("2026-01-10T12:00:00Z");
    let detected_at = at("2026-05-20T08:00:00Z");
    let tainted = retention_taint(&meters(), last_good_at, detected_at, FLOOR);
    let expected = DayRange::new(
        at("2025-09-07T12:00:00Z").date(),
        at("2026-01-15T08:00:00Z").date(),
    )
    .unwrap();
    assert_eq!(tainted.ranges().len(), 2, "every meter of the source");
    for ranges in tainted.ranges().values() {
        assert_eq!(ranges, &vec![expected]);
    }
}

#[test]
fn an_unidentifiable_entry_taints_every_day_the_feed_still_holds() {
    let detected_at = at("2026-05-20T08:00:00Z");
    let tainted = unidentifiable_taint(&meters(), detected_at, FLOOR);
    for ranges in tainted.ranges().values() {
        assert_eq!(
            ranges,
            &vec![DayRange::new(at("2026-01-15T08:00:00Z").date(), detected_at.date()).unwrap()]
        );
    }
}

#[test]
fn tainted_days_round_trip_their_stored_form() {
    let tainted = retention_taint(
        &meters(),
        at("2026-01-10T12:00:00Z"),
        at("2026-05-20T08:00:00Z"),
        FLOOR,
    );
    let stored = tainted.to_json();
    assert_eq!(stored[0]["first"], "2025-09-07");
    assert_eq!(TaintedDays::from_json(&stored).unwrap(), tainted);
    assert!(TaintedDays::from_json(&serde_json::json!([{"gts_type": "x"}])).is_err());
    assert_eq!(TaintedDays::none().to_json(), serde_json::json!([]));
}

#[test]
fn kinds_and_states_round_trip() {
    for kind in LossKind::ALL {
        assert_eq!(LossKind::parse(kind.as_str()), Some(kind));
    }
    for state in [
        LossState::Open,
        LossState::Narrowed,
        LossState::PendingApproval,
        LossState::Resolved,
    ] {
        assert_eq!(LossState::parse(state.as_str()), Some(state));
    }
}

#[test]
fn a_day_range_never_ends_before_it_starts() {
    let (first, last) = (
        at("2026-01-10T00:00:00Z").date(),
        at("2026-01-12T00:00:00Z").date(),
    );
    let range = DayRange::new(first, last).unwrap();
    assert_eq!((range.first(), range.last()), (first, last));
    assert!(DayRange::new(first, first).is_some(), "one day");
    assert_eq!(DayRange::new(last, first), None);
}

#[test]
fn a_stored_taint_with_a_reversed_range_is_refused() {
    let stored = serde_json::json!([{
        "gts_type": "gts.cf.core.uc.usage_record.v2~cf.test.a.v1~",
        "first": "2026-01-12",
        "last": "2026-01-10",
    }]);
    assert!(TaintedDays::from_json(&stored).is_err());
    let forward = serde_json::json!([{
        "gts_type": "gts.cf.core.uc.usage_record.v2~cf.test.a.v1~",
        "first": "2026-01-10",
        "last": "2026-01-12",
    }]);
    let tainted = TaintedDays::from_json(&forward).unwrap();
    assert_eq!(tainted.to_json(), forward);
}
