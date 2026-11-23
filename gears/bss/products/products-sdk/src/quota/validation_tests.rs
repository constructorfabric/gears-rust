//! The quota-set validation contract (P-D-275, P-D-276): the stable check codes, the answer's
//! `ok()`, and the definition reference's wire shape.
#![allow(clippy::unwrap_used)]
use super::*;

#[test]
fn every_check_code_has_its_stable_token() {
    let tokens: Vec<&str> = QuotaCheckCode::ALL.iter().map(|c| c.code()).collect();
    assert_eq!(
        tokens,
        [
            "QUOTA_RULE_INVALID",
            "QUOTA_DEFINITION_CHANGED",
            "QUOTA_AMOUNT_INVALID",
            "QUOTA_UNIT_INEXACT",
            "QUOTA_SCOPE_UNSUPPORTED",
            "QUOTA_SET_INCOMPATIBLE",
            "QUOTA_QUALIFICATION_UNAVAILABLE",
        ]
    );
}

#[test]
fn a_validation_is_ok_exactly_without_checks() {
    let mut answer = QuotaSetValidation {
        rules: Vec::new(),
        checks: Vec::new(),
    };
    assert!(answer.ok());
    answer.checks.push(QuotaCheck {
        code: QuotaCheckCode::AmountInvalid,
        rule_ids: vec![uuid::Uuid::from_u128(1)],
        field: "amount".into(),
        detail: "1.5 is not a canonical integer".into(),
    });
    assert!(!answer.ok());
}

#[test]
fn a_definition_ref_round_trips_with_a_string_version() {
    let r: crate::quota::catalog::QuotaDefinitionRef = serde_json::from_value(serde_json::json!({
        "id": "4a1e7d60-34b2-4e5b-8709-eab681e15940", "version": "1", "digest": "a".repeat(64)
    }))
    .unwrap();
    assert_eq!(r.version.get(), 1);
    assert_eq!(serde_json::to_value(&r).unwrap()["version"], "1");
}
