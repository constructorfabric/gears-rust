use bss_rating_core::engine::current::input::ChildKind;

use super::{ChildFailure, ChildStatus, kind_name, parse_kind};

#[test]
fn statuses_and_kinds_read_back_from_their_stored_names() {
    for status in [
        ChildStatus::Pending,
        ChildStatus::Provisional,
        ChildStatus::Final,
        ChildStatus::FinalStale,
        ChildStatus::Failed,
    ] {
        assert_eq!(ChildStatus::parse(status.as_str()), Some(status));
    }
    for kind in [
        ChildKind::UsageWindow,
        ChildKind::PeriodLine,
        ChildKind::OneTime,
    ] {
        assert_eq!(parse_kind(kind_name(kind)), Some(kind));
    }
    assert_eq!(ChildStatus::parse("deleted"), None);
}

#[test]
fn a_newer_version_turns_a_final_child_stale_and_keeps_every_other_status() {
    assert_eq!(ChildStatus::Final.bumped(), ChildStatus::FinalStale);
    for status in [
        ChildStatus::Pending,
        ChildStatus::Provisional,
        ChildStatus::FinalStale,
        ChildStatus::Failed,
    ] {
        assert_eq!(status.bumped(), status);
    }
    assert_eq!(
        ChildFailure::UsagePolicyMismatch.as_str(),
        "usage_policy_mismatch"
    );
}
