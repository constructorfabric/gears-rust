// Created: 2026-08-12 by Constructor Tech
//! Tests for the precondition-violation vocabulary.
//!
//! Mirrors `gts_tests.rs`: the two typed sub-enums carry the same
//! preserve-don't-discard guarantee, so they earn the same coverage.

use super::{PreconditionKind, SETTING_RETIRED};

#[test]
fn the_retired_marker_round_trips() {
    let kind = PreconditionKind::from_wire(SETTING_RETIRED);
    assert_eq!(kind, PreconditionKind::SettingRetired);
    assert_eq!(kind.as_wire(), SETTING_RETIRED);
}

#[test]
fn an_unmodelled_violation_is_preserved_not_discarded() {
    // A precondition type this version does not know must still be reportable
    // verbatim, so a consumer can surface it and a later version can model it
    // without a migration.
    let kind = PreconditionKind::from_wire("SOME_FUTURE_VIOLATION");
    assert_eq!(
        kind,
        PreconditionKind::Unknown("SOME_FUTURE_VIOLATION".to_owned())
    );
    assert_eq!(kind.as_wire(), "SOME_FUTURE_VIOLATION");
}

#[test]
fn every_wire_string_survives_a_round_trip() {
    for wire in [SETTING_RETIRED, "SOMETHING_ELSE", ""] {
        assert_eq!(
            PreconditionKind::from_wire(wire).as_wire(),
            wire,
            "`{wire}` did not survive from_wire -> as_wire"
        );
    }
}

#[test]
fn the_retired_marker_is_not_swallowed_by_the_unknown_arm() {
    // Ordering guard: if the catch-all were matched first, every violation would
    // become `Unknown` and the `Retired` projection would silently stop firing.
    assert!(matches!(
        PreconditionKind::from_wire(SETTING_RETIRED),
        PreconditionKind::SettingRetired
    ));
}
