//! Tests for the tenant FX revaluation-mode value type ([`super`]).

use super::*;

#[test]
fn revaluation_mode_round_trips() {
    assert_eq!(RevaluationMode::parse("MODE_A"), Ok(RevaluationMode::ModeA));
    assert_eq!(RevaluationMode::parse("MODE_B"), Ok(RevaluationMode::ModeB));
    assert_eq!(RevaluationMode::ModeA.as_str(), "MODE_A");
    assert_eq!(RevaluationMode::ModeB.as_str(), "MODE_B");
    assert!(RevaluationMode::parse("nope").is_err());
    assert!(RevaluationMode::parse("mode_a").is_err(), "case-sensitive");
}

#[test]
fn revaluation_mode_default_is_mode_a_fail_safe() {
    assert_eq!(RevaluationMode::default(), RevaluationMode::ModeA);
    assert!(
        !RevaluationMode::default().revalues(),
        "the un-configured default must never revalue (fail-safe vs ERP double-count)"
    );
}

#[test]
fn only_mode_b_revalues() {
    assert!(!RevaluationMode::ModeA.revalues());
    assert!(RevaluationMode::ModeB.revalues());
}

#[test]
fn fleet_default_follows_the_global_flag() {
    assert_eq!(
        RevaluationMode::fleet_default(false),
        RevaluationMode::ModeA,
        "global off ⇒ unconfigured tenants default to fail-safe ModeA"
    );
    assert_eq!(
        RevaluationMode::fleet_default(true),
        RevaluationMode::ModeB,
        "global on ⇒ unconfigured tenants default to ModeB (fleet default-on)"
    );
}
