//! Golden JSON fixtures of the `/bss-rating/v1` REST DTOs (DESIGN §3.3, T-D-71): a response may
//! only gain members, and a request an old client sends must still be accepted. Record a new
//! response fixture with `UPDATE_RATING_GOLDEN=1`; request fixtures are written by hand.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::path::PathBuf;

use bss_rating::api::rest::dto::{
    RatingInboxEntryView, RatingInboxReleaseRequest, RatingInboxResolution, RatingInboxState,
};
use bss_rating_golden::{assert_additive_output, assert_census, assert_still_accepted};
use time::OffsetDateTime;
use uuid::Uuid;

// @cpt-begin:cpt-cf-bss-rating-algo-foundation-contract-gates:p2:inst-cg-golden
const FIXTURES: &[&str] = &[
    "inbox_entry_view",
    "inbox_entry_view_released",
    "inbox_release_request",
];

fn dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/golden/v1")
}

fn at(seconds: i64) -> OffsetDateTime {
    OffsetDateTime::from_unix_timestamp(1_792_411_200 + seconds).unwrap()
}

#[test]
fn every_fixture_is_checked() {
    assert_census(&dir(), FIXTURES);
}

#[test]
fn inbox_entry_view_v1() {
    let view = RatingInboxEntryView {
        entry_id: Uuid::from_u128(0x0001),
        tenant_id: Uuid::from_u128(0x000A),
        source: "fact".to_owned(),
        business_id: "fact-1".to_owned(),
        version: 3,
        payload_digest: "ab".repeat(32),
        received_via: "pull".to_owned(),
        state: RatingInboxState::Applied,
        quarantine_reason: None,
        conflict_of: None,
        received_at: at(0),
        applied_at: Some(at(0)),
    };
    let _ = assert_additive_output(&dir(), "inbox_entry_view", &view);
}

#[test]
fn inbox_entry_view_released_v1() {
    let view = RatingInboxEntryView {
        entry_id: Uuid::from_u128(0x0002),
        tenant_id: Uuid::from_u128(0x000A),
        source: "fact".to_owned(),
        business_id: "fact-1".to_owned(),
        version: 3,
        payload_digest: "cd".repeat(32),
        received_via: "event".to_owned(),
        state: RatingInboxState::Discarded,
        quarantine_reason: Some("digest_conflict".to_owned()),
        conflict_of: Some(Uuid::from_u128(0x0001)),
        received_at: at(60),
        applied_at: None,
    };
    let _ = assert_additive_output(&dir(), "inbox_entry_view_released", &view);
}

#[test]
fn inbox_release_request_v1() {
    assert_still_accepted::<RatingInboxReleaseRequest>(&dir(), "inbox_release_request");
    let request: RatingInboxReleaseRequest = serde_json::from_str(
        &std::fs::read_to_string(dir().join("inbox_release_request.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(request.resolution, RatingInboxResolution::Apply);
}
// @cpt-end:cpt-cf-bss-rating-algo-foundation-contract-gates:p2:inst-cg-golden
