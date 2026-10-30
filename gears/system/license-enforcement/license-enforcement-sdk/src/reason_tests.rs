use super::DenialReason;

const KNOWN: &[&str] = &[
    "no_grant_for_resource",
    "subject_not_bound",
    "signature_invalid",
    "key_untrusted",
    "stale",
    "clock_rollback",
    "duplicated",
    "awaiting_acceptance",
    "suspended",
    "terminated",
    "not_yet_valid",
    "expired",
    "grace_expired",
    "chain_invalid",
    "condition_unmet",
    "condition_budget_exceeded",
];

#[test]
fn every_known_reason_round_trips() {
    for wire in KNOWN {
        let reason = DenialReason::from_wire(wire);
        assert!(!matches!(reason, DenialReason::Unknown(_)), "{wire}");
        assert_eq!(reason.as_wire(), *wire);
        let json = serde_json::to_value(&reason).unwrap();
        assert_eq!(json, serde_json::json!(wire));
        assert_eq!(
            serde_json::from_value::<DenialReason>(json).unwrap(),
            reason
        );
    }
}

#[test]
fn unknown_reason_is_kept_not_rejected() {
    let reason: DenialReason = serde_json::from_str(r#""quota_exhausted""#).unwrap();
    assert_eq!(reason, DenialReason::Unknown("quota_exhausted".to_owned()));
    assert_eq!(
        serde_json::to_string(&reason).unwrap(),
        r#""quota_exhausted""#
    );
}

#[test]
fn plugin_and_evaluator_denials_keep_their_wire_names() {
    use crate::{EvaluatorDenial as E, PluginDenial as P};
    let pairs: Vec<(DenialReason, &str)> = vec![
        (P::SignatureInvalid.into(), "signature_invalid"),
        (P::KeyUntrusted.into(), "key_untrusted"),
        (P::Stale.into(), "stale"),
        (P::ClockRollback.into(), "clock_rollback"),
        (P::Duplicated.into(), "duplicated"),
        (E::AwaitingAcceptance.into(), "awaiting_acceptance"),
        (E::Suspended.into(), "suspended"),
        (E::Terminated.into(), "terminated"),
        (E::NotYetValid.into(), "not_yet_valid"),
        (E::Expired.into(), "expired"),
        (E::GraceExpired.into(), "grace_expired"),
        (E::ConditionUnmet.into(), "condition_unmet"),
        (
            E::ConditionBudgetExceeded.into(),
            "condition_budget_exceeded",
        ),
    ];
    for (reason, wire) in pairs {
        assert_eq!(reason.as_wire(), wire);
    }
}
