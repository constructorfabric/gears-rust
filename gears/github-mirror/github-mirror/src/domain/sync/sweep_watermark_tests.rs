use chrono::{DateTime, Duration, Utc};

use super::{SWEEP_OVERLAP, high_water, is_stale, later_watermark, stop_threshold};
use crate::domain::repo::SyncWatermarkRecord;

fn stored(last_seen: Option<&str>) -> SyncWatermarkRecord {
    SyncWatermarkRecord {
        repo_id: 1,
        family: "issues".to_owned(),
        last_seen_updated_at: last_seen.map(ToOwned::to_owned),
        page1_etag: None,
        sweep_in_progress: false,
        candidate_high_water: None,
    }
}

fn at(raw: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(raw)
        .expect("test timestamps must parse")
        .with_timezone(&Utc)
}

#[test]
fn a_first_sweep_has_no_lower_bound() {
    assert_eq!(stop_threshold(None, false), None);
}

#[test]
fn force_ignores_the_stored_watermark() {
    let row = stored(Some("2026-09-01T10:00:00Z"));
    assert_eq!(stop_threshold(Some(&row), true), None);
}

#[test]
fn the_threshold_steps_back_by_the_overlap() {
    let row = stored(Some("2026-09-01T10:00:00Z"));
    assert_eq!(
        stop_threshold(Some(&row), false),
        Some(at("2026-09-01T10:00:00Z") - SWEEP_OVERLAP)
    );
}

/// Neither side can be read, so there is nothing to choose between them: the
/// stored value stays, and the run does not write a second unreadable stamp
/// over the first.
#[test]
fn two_unreadable_stamps_leave_the_stored_one_alone() {
    assert_eq!(
        later_watermark(Some("not a date"), "also not a date".to_owned()),
        "not a date"
    );
    assert_eq!(
        later_watermark(Some("not a date"), "2026-08-25T10:00:00Z".to_owned()),
        "2026-08-25T10:00:00Z",
        "a readable candidate still replaces an unreadable stored value"
    );
}

#[test]
fn an_unparseable_watermark_falls_back_to_a_full_sweep() {
    let row = stored(Some("not a timestamp"));
    assert_eq!(stop_threshold(Some(&row), false), None);
}

#[test]
fn the_high_water_is_the_newest_entity_seen() {
    let seen = ["2026-09-01T10:00:00Z", "2026-09-03T08:00:00Z"];
    assert_eq!(high_water(&seen, None), Some(at("2026-09-03T08:00:00Z")));
}

#[test]
fn an_empty_sweep_keeps_the_threshold_it_started_from() {
    let threshold = at("2026-09-01T10:00:00Z");
    assert_eq!(high_water(&[], Some(threshold)), Some(threshold));
}

#[test]
fn the_high_water_never_moves_backwards() {
    let threshold = at("2026-09-03T00:00:00Z");
    let seen = ["2026-09-01T10:00:00Z"];
    assert_eq!(
        high_water(&seen, Some(threshold)),
        Some(threshold),
        "an older entity must not pull the watermark back"
    );
}

#[test]
fn the_overlap_is_wide_enough_for_clock_skew() {
    assert!(SWEEP_OVERLAP >= Duration::minutes(1));
}

#[test]
fn an_entity_is_stale_only_when_it_is_older_than_the_bound() {
    let bound = Some(at("2026-09-01T10:00:00Z"));
    assert!(is_stale(Some("2026-09-01T09:59:59Z"), bound));
    assert!(
        !is_stale(Some("2026-09-01T10:00:00Z"), bound),
        "equal is kept"
    );
    assert!(!is_stale(Some("2026-09-01T10:00:01Z"), bound));
}

#[test]
fn a_missing_timestamp_or_an_unbounded_sweep_keeps_the_entity() {
    let bound = Some(at("2026-09-01T10:00:00Z"));
    assert!(!is_stale(None, bound), "nothing to compare, so keep it");
    assert!(
        !is_stale(Some("2020-01-01T00:00:00Z"), None),
        "a full sweep has no bound and keeps everything"
    );
    assert!(
        !is_stale(Some("not a timestamp"), bound),
        "an unreadable timestamp keeps the entity rather than dropping it"
    );
}
