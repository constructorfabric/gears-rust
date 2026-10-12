//! Wire shape of the money error mappings: the `(field, reason)` pair each
//! `MoneyError` variant carries (what clients branch on, not only the 400), and
//! the split between a reversal's stored-original money failure (500) and a
//! mapping correction's caller-supplied line mismatch (400).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use bss_ledger_sdk::MoneyError;
use toolkit::api::canonical_prelude::{CanonicalError, Problem};

use super::{
    money_error_to_canonical, reversal_error_to_canonical, stored_reversal_error_to_canonical,
};
use crate::domain::invoice::reversal::ReversalError;

/// Every `(field, reason)` field violation in the rendered Problem.
fn violations(err: CanonicalError) -> Vec<(String, String)> {
    fn walk(value: &serde_json::Value, out: &mut Vec<(String, String)>) {
        match value {
            serde_json::Value::Object(map) => {
                if let (
                    Some(serde_json::Value::String(field)),
                    Some(serde_json::Value::String(reason)),
                ) = (map.get("field"), map.get("reason"))
                {
                    out.push((field.clone(), reason.clone()));
                }
                map.values().for_each(|v| walk(v, out));
            }
            serde_json::Value::Array(items) => items.iter().for_each(|v| walk(v, out)),
            _ => {}
        }
    }
    let value = serde_json::to_value(Problem::from(err)).unwrap();
    let mut out = Vec::new();
    walk(&value, &mut out);
    out
}

/// A money error and the `(field, reason)` violation it must render, if any.
type ExpectedViolation = (MoneyError, Option<(&'static str, &'static str)>);

#[test]
fn each_money_error_carries_its_own_field_and_reason() {
    let cases: [ExpectedViolation; 7] = [
        // Malformed text / code: a request constraint, no field violation.
        (MoneyError::InvalidDecimal, None),
        (MoneyError::InvalidCurrency, None),
        (
            MoneyError::ScaleOutOfRange,
            Some(("currency_scales", "CURRENCY_SCALE_OUT_OF_RANGE")),
        ),
        (
            MoneyError::AmountOutOfRange,
            Some(("amount", "AMOUNT_OUT_OF_RANGE")),
        ),
        (
            MoneyError::InvalidPostingIncrement,
            Some(("amount", "INVALID_POSTING_INCREMENT")),
        ),
        (
            MoneyError::CurrencyMismatch,
            Some(("currency", "CURRENCY_MISMATCH")),
        ),
        (
            MoneyError::ScaleMismatch,
            Some(("lines", "CURRENCY_SCALE_MISMATCH")),
        ),
    ];
    for (error, expected) in cases {
        let canonical = money_error_to_canonical(error.clone());
        assert_eq!(canonical.status_code(), 400, "{error:?}");
        let found = violations(canonical);
        match expected {
            Some((field, reason)) => assert_eq!(
                found,
                vec![(field.to_owned(), reason.to_owned())],
                "{error:?}"
            ),
            None => assert!(found.is_empty(), "{error:?}: {found:?}"),
        }
    }
}

#[test]
fn corrected_line_money_mismatch_is_a_caller_400_with_its_wire_code() {
    for (error, reason) in [
        (MoneyError::CurrencyMismatch, "CURRENCY_MISMATCH"),
        (MoneyError::ScaleMismatch, "CURRENCY_SCALE_MISMATCH"),
    ] {
        let canonical = reversal_error_to_canonical(ReversalError::Money(error));
        assert_eq!(canonical.status_code(), 400);
        let found = violations(canonical);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].1, reason);
    }
}

#[test]
fn stored_original_money_mismatch_is_internal_not_a_field_violation() {
    for error in [MoneyError::CurrencyMismatch, MoneyError::ScaleMismatch] {
        let canonical = stored_reversal_error_to_canonical(ReversalError::Money(error));
        assert_eq!(canonical.status_code(), 500);
        assert!(violations(canonical).is_empty());
    }
    // The business refusals are unchanged on the stored path.
    let cannot = stored_reversal_error_to_canonical(ReversalError::CannotReverseReversal);
    assert_eq!(cannot.status_code(), 400);
    assert_eq!(violations(cannot)[0].1, "CANNOT_REVERSE_REVERSAL");
    let credit = stored_reversal_error_to_canonical(ReversalError::CreditGrantNotReconstructible);
    assert_eq!(credit.status_code(), 400);
    assert_eq!(violations(credit)[0].1, "CANNOT_REVERSE_CREDIT_GRANT");
}
