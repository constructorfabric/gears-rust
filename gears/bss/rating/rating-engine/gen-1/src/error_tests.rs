#![allow(clippy::expect_used, clippy::unwrap_used)]

use bss_money::MoneyError;
use bss_money::exact::ExactError;

use super::EvaluationError;

#[test]
fn the_reason_code_is_the_wire_tag() {
    let error = EvaluationError::PriceUncovered {
        detail: "item 1 is uncovered".into(),
    };
    let json = serde_json::to_string(&error).unwrap();
    assert_eq!(
        json,
        r#"{"reason_code":"price_uncovered","detail":"item 1 is uncovered"}"#
    );
    assert_eq!(
        serde_json::from_str::<EvaluationError>(&json).unwrap(),
        error
    );
    assert_eq!(error.reason_code(), "price_uncovered");
    assert_eq!(error.detail(), "item 1 is uncovered");
    assert_eq!(error.to_string(), "price_uncovered: item 1 is uncovered");
}

#[test]
fn every_variant_round_trips_with_its_reason_code() {
    let codes = [
        "price_uncovered",
        "binding_mismatch",
        "binding_currency_mismatch",
        "non_injective_mapping",
        "unroutable_dimension_tuple",
        "unknown_enum_value",
        "unsupported_input_fold",
        "usage_type_not_chargeable",
        "derived_evaluation_failed",
        "derived_formula_inexact",
        "unsupported_primitive",
        "negative_window_quantity",
        "adjustment_unsupported",
        "fx_not_supported",
        "intra_window_policy_change",
        "precision_overflow",
        "quantity_out_of_range",
        "engine_unavailable",
        "layout_mismatch",
        "period_geometry_mismatch",
        "money_contract_violation",
    ];
    for code in codes {
        let json = format!(r#"{{"reason_code":"{code}","detail":"d"}}"#);
        let error: EvaluationError = serde_json::from_str(&json).unwrap();
        assert_eq!(error.reason_code(), code);
        assert_eq!(error.detail(), "d");
        assert_eq!(error.to_string(), format!("{code}: d"));
        assert_eq!(serde_json::to_string(&error).unwrap(), json);
    }
    let unknown = r#"{"reason_code":"selection_ambiguity","detail":"d"}"#;
    assert!(serde_json::from_str::<EvaluationError>(unknown).is_err());
}

#[test]
fn money_crate_errors_map_to_the_taxonomy() {
    let cases = [
        (ExactError::ArithmeticLimit, "precision_overflow"),
        (ExactError::DivisionByZero, "money_contract_violation"),
        (ExactError::EmptyWeights, "money_contract_violation"),
        (ExactError::NegativeWeight, "money_contract_violation"),
        (ExactError::TooManyTerms, "money_contract_violation"),
        (
            ExactError::Money(MoneyError::InvalidPostingIncrement),
            "money_contract_violation",
        ),
    ];
    for (error, code) in cases {
        assert_eq!(
            EvaluationError::from_money("site", &error).reason_code(),
            code
        );
    }
}
