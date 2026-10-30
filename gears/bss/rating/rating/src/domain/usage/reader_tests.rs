#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::time::Duration;

use serde_json::json;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use super::{FeedPosition, PauseReason, ReaderSettings, elapsed, feed_failure, meter_of, position};
use crate::domain::error::DomainError;
use crate::domain::ports::Dependency;
use crate::domain::usage::feed::{FeedError, MAX_PAGE_LIMIT, MeterId};

fn at(text: &str) -> OffsetDateTime {
    OffsetDateTime::parse(text, &Rfc3339).unwrap()
}

#[test]
fn a_caught_up_reader_has_no_lag() {
    let now = at("2026-10-08T12:00:00Z");
    let position = position(&[], Duration::from_secs(5), true, now);
    assert_eq!(
        position,
        FeedPosition {
            lag: Duration::ZERO,
            cursor_age: Duration::from_secs(5),
            oldest_unread_age: Duration::ZERO,
        }
    );
}

#[test]
fn a_full_page_reports_its_lag_and_oldest_unread_entry() {
    let now = at("2026-10-08T12:00:00Z");
    let entries = [
        json!({"accepted_at": "2026-10-08T11:00:00Z", "window_end": "2026-10-08T10:00:00Z"}),
        json!({"accepted_at": "2026-10-08T11:30:00Z", "window_end": "2026-10-08T09:00:00Z"}),
        json!({"accepted_at": "garbage"}),
    ];
    let position = position(&entries, Duration::ZERO, false, now);
    assert_eq!(position.lag, Duration::from_mins(30));
    assert_eq!(position.oldest_unread_age, Duration::from_hours(3));
}

#[test]
fn a_future_instant_is_no_age() {
    assert_eq!(
        elapsed(at("2026-10-09T00:00:00Z"), at("2026-10-08T00:00:00Z")),
        Duration::ZERO
    );
}

#[test]
fn pause_reasons_have_their_labels() {
    assert_eq!(PauseReason::GrantNarrowed.as_str(), "grant_narrowed");
    assert_eq!(PauseReason::Backpressure.as_str(), "backpressure");
}

#[test]
fn a_feed_failure_counts_against_the_usage_feed() {
    let error = feed_failure(FeedError::Unavailable("down".to_owned(), None));
    assert_eq!(
        error.dependency_failure().map(|(dependency, _)| dependency),
        Some(Dependency::UsageFeed)
    );
}

#[test]
fn reader_settings_refuse_what_would_turn_the_reader_into_a_hot_loop() {
    let second = Duration::from_secs(1);
    assert!(ReaderSettings::new(1, second, 1, second, second).is_ok());
    assert!(ReaderSettings::new(MAX_PAGE_LIMIT, second, 1, second, second).is_ok());
    for (what, refused) in [
        (
            "no entry per page",
            ReaderSettings::new(0, second, 1, second, second),
        ),
        (
            "a page above the feed's",
            ReaderSettings::new(MAX_PAGE_LIMIT + 1, second, 1, second, second),
        ),
        (
            "no poll pause",
            ReaderSettings::new(1, Duration::ZERO, 1, second, second),
        ),
        (
            "no queue depth",
            ReaderSettings::new(1, second, 0, second, second),
        ),
        (
            "no backoff",
            ReaderSettings::new(1, second, 1, Duration::ZERO, second),
        ),
        (
            "no call time",
            ReaderSettings::new(1, second, 1, second, Duration::ZERO),
        ),
    ] {
        assert!(
            matches!(refused, Err(DomainError::InvalidArgument { .. })),
            "{what}: {refused:?}"
        );
    }
}

#[test]
fn a_meter_is_read_only_from_a_well_formed_gts_type_id() {
    let hours = "gts.cf.core.uc.usage_record.v2~cf.test.hours.v1~";
    assert_eq!(
        meter_of(&json!({"gts_type_id": hours})),
        Some(MeterId::parse(hours).unwrap())
    );
    assert_eq!(meter_of(&json!({"gts_type_id": "gts.other~"})), None);
    assert_eq!(meter_of(&json!({"gts_type_id": 7})), None);
    assert_eq!(meter_of(&json!({})), None);
}
