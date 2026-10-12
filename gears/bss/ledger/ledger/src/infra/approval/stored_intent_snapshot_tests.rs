//! Rejection branches of the stored threshold-snapshot validator: the checks
//! that let `approve` / `resubmit` refuse a tampered or inconsistent snapshot.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use super::*;
use crate::domain::approval::policy::D2_DEFAULT_RULE;

fn stored(amount: &str, currency: &str, scale: u8) -> StoredMoney {
    StoredMoney {
        amount: amount.to_owned(),
        currency: currency.to_owned(),
        currency_scale: scale,
    }
}

/// A consistent snapshot: one EUR override, resolved threshold = that override.
fn valid() -> StoredThresholdSnapshot {
    StoredThresholdSnapshot {
        d2_default: D2_DEFAULT_RULE.to_owned(),
        d2_thresholds: vec![stored("2500", "EUR", 2)],
        d2_threshold: Some(stored("2500", "EUR", 2)),
        policy_version: Some(3),
        policy_effective_from: Some("2026-01-01T00:00:00Z".to_owned()),
        basis: SnapshotBasis::TransactionGate,
        a6_backdating_biz_days: 5,
        pending_ttl_seconds: 3600,
        resolved_at: "2026-10-09T00:00:00Z".to_owned(),
    }
}

fn rejects(snapshot: StoredThresholdSnapshot, why: &str) {
    let value = serde_json::to_value(&snapshot).unwrap();
    assert!(
        matches!(
            validate_threshold_snapshot(value),
            Err(DomainError::Internal(_))
        ),
        "{why}"
    );
    assert!(
        matches!(
            check_threshold_snapshot(snapshot),
            Err(DomainError::Internal(_))
        ),
        "{why} (typed)"
    );
}

#[test]
fn a_consistent_snapshot_round_trips_with_canonical_money() {
    let mut snapshot = valid();
    // Fractional trailing zeros are accepted and re-encoded canonically.
    snapshot.d2_thresholds = vec![stored("2500.00", "EUR", 2)];
    snapshot.d2_threshold = Some(stored("2500.0", "EUR", 2));
    let out = validate_threshold_snapshot(serde_json::to_value(&snapshot).unwrap()).unwrap();
    assert_eq!(out, valid());
    // The default fallback for an unconfigured currency is consistent too.
    let mut default = valid();
    default.d2_threshold = Some(stored("1000", "USD", 2));
    assert!(check_threshold_snapshot(default).is_ok());
    // No resolved threshold (a non-amount kind) is accepted.
    let mut none = valid();
    none.d2_threshold = None;
    assert!(check_threshold_snapshot(none).is_ok());
}

#[test]
fn undecodable_json_is_internal() {
    assert!(matches!(
        validate_threshold_snapshot(serde_json::json!({"d2_default": D2_DEFAULT_RULE})),
        Err(DomainError::Internal(_))
    ));
    let mut value = serde_json::to_value(valid()).unwrap();
    value["basis"] = serde_json::json!("guessed_gate");
    assert!(matches!(
        validate_threshold_snapshot(value),
        Err(DomainError::Internal(_))
    ));
}

#[test]
fn unknown_default_rule_is_rejected() {
    let mut snapshot = valid();
    snapshot.d2_default = "flat_1000_usd".to_owned();
    rejects(snapshot, "unknown d2_default");
}

#[test]
fn a_captured_threshold_violating_its_own_spec_is_rejected() {
    let mut finer = valid();
    finer.d2_thresholds = vec![stored("2500.001", "EUR", 2)];
    rejects(finer, "threshold finer than its scale");
    let mut resolved = valid();
    resolved.d2_threshold = Some(stored("2500.001", "EUR", 2));
    rejects(resolved, "resolved threshold finer than its scale");
    let mut code = valid();
    code.d2_thresholds = vec![stored("2500", "eur", 2)];
    rejects(code, "invalid currency code");
}

#[test]
fn a_policy_failing_validation_is_rejected() {
    let mut a6 = valid();
    a6.a6_backdating_biz_days = 0;
    rejects(a6, "A6 out of range");
    let mut ttl = valid();
    ttl.pending_ttl_seconds = 0;
    rejects(ttl, "TTL not positive");
    let mut range = valid();
    range.d2_thresholds = vec![stored("1", "EUR", 2)];
    range.d2_threshold = None;
    rejects(range, "D2 out of range");
    let mut duplicate = valid();
    duplicate.d2_thresholds = vec![stored("2500", "EUR", 2), stored("3000", "EUR", 2)];
    rejects(duplicate, "duplicate currency");
}

#[test]
fn a_resolved_threshold_differing_from_the_captured_policy_is_rejected() {
    let mut amount = valid();
    amount.d2_threshold = Some(stored("2400", "EUR", 2));
    rejects(amount, "resolved amount differs");
    let mut scale = valid();
    scale.d2_threshold = Some(stored("2500", "EUR", 3));
    rejects(scale, "resolved scale differs");
    let mut default = valid();
    default.d2_threshold = Some(stored("999", "USD", 2));
    rejects(default, "resolved default differs");
}
