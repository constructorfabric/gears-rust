#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::collections::BTreeMap;

use bss_money::exact::ExactAmount;
use rust_decimal::{Decimal, RoundingStrategy};

use super::{
    ArithmeticBudget, DerivedExpr, EvaluationError, RoundMode, evaluate, fits_decimal,
    fits_decimal_text, round_at,
};

fn decimal(text: &str) -> Decimal {
    text.parse().unwrap()
}

fn exact(text: &str) -> ExactAmount {
    ExactAmount::from_decimal(decimal(text))
}

fn input(name: &str) -> DerivedExpr {
    DerivedExpr::Input {
        name: name.to_owned(),
    }
}

fn constant(text: &str) -> DerivedExpr {
    DerivedExpr::Const {
        value: decimal(text),
    }
}

fn run(formula: &DerivedExpr, a: &str) -> Result<ExactAmount, EvaluationError> {
    let inputs = BTreeMap::from([("a", exact(a))]);
    evaluate(formula, &inputs, ArithmeticBudget::GEN_1)
}

#[test]
fn v57_steps_products_would_round_are_refused() {
    let add_small = DerivedExpr::Add {
        left: Box::new(input("a")),
        right: Box::new(constant("0.0000000001")),
    };
    assert_eq!(
        run(&add_small, "100000000000000000000")
            .unwrap_err()
            .reason_code(),
        "derived_formula_inexact"
    );
    let third = DerivedExpr::DivConst {
        operand: Box::new(input("a")),
        divisor: decimal("3"),
    };
    assert_eq!(
        run(&third, "1").unwrap_err().reason_code(),
        "derived_formula_inexact"
    );
    let square = DerivedExpr::Mul {
        left: Box::new(input("a")),
        right: Box::new(input("a")),
    };
    assert_eq!(run(&square, "0.5").unwrap(), exact("0.25"));
    // 31 fractional digits: Products would round the product.
    assert_eq!(
        run(&square, "0.0000000000000001")
            .unwrap_err()
            .reason_code(),
        "derived_formula_inexact"
    );
}

#[test]
fn a_declared_rounding_inside_the_formula_is_not_a_failure() {
    // ceil(a / 128) with a = 300 is exact as 300/128 = 2.34375, then 3.
    let ceil = DerivedExpr::Ceil {
        operand: Box::new(DerivedExpr::DivConst {
            operand: Box::new(input("a")),
            divisor: decimal("128"),
        }),
    };
    assert_eq!(run(&ceil, "300").unwrap(), exact("3"));
    let floor = DerivedExpr::Floor {
        operand: Box::new(DerivedExpr::Sub {
            left: Box::new(constant("0")),
            right: Box::new(input("a")),
        }),
    };
    assert_eq!(run(&floor, "2.5").unwrap(), exact("-3"));
    let round = DerivedExpr::Round {
        operand: Box::new(input("a")),
        scale: 1,
        mode: RoundMode::HalfEven,
    };
    assert_eq!(run(&round, "0.25").unwrap(), exact("0.2"));
    let max = DerivedExpr::Max {
        operands: vec![input("a"), constant("4")],
    };
    assert_eq!(run(&max, "3").unwrap(), exact("4"));
    let min = DerivedExpr::Min {
        operands: vec![input("a"), constant("4")],
    };
    assert_eq!(run(&min, "3").unwrap(), exact("3"));
    assert_eq!(
        run(&DerivedExpr::Input { name: "b".into() }, "3")
            .unwrap_err()
            .reason_code(),
        "derived_evaluation_failed"
    );
}

#[test]
fn exact_rounding_agrees_with_rust_decimal_for_every_mode() {
    let values = [
        "2.5", "-2.5", "3.5", "-3.5", "2.45", "2.55", "-2.45", "0.125", "1.0001", "-1.0001", "7",
        "-0.5", "0.4999",
    ];
    let modes = [
        (RoundMode::HalfEven, RoundingStrategy::MidpointNearestEven),
        (RoundMode::HalfUp, RoundingStrategy::MidpointAwayFromZero),
        (RoundMode::Up, RoundingStrategy::AwayFromZero),
        (RoundMode::Down, RoundingStrategy::ToZero),
    ];
    for value in values {
        for scale in 0..=2 {
            for (mode, strategy) in modes {
                let expected = ExactAmount::from_decimal(
                    decimal(value).round_dp_with_strategy(scale, strategy),
                );
                let actual = round_at(&exact(value), scale, mode).unwrap();
                assert_eq!(actual, expected, "{value} at {scale} {mode:?}");
            }
        }
    }
    let third = exact("1").checked_div(&exact("3")).unwrap();
    assert_eq!(
        round_at(&third, 3, RoundMode::HalfEven).unwrap(),
        exact("0.333")
    );
}

#[test]
fn steps_rust_decimal_computes_exactly_are_accepted_up_to_its_96_bit_coefficient() {
    // A 29-digit coefficient within 2^96 − 1: rust_decimal 1.41 adds and multiplies it exactly.
    let add = DerivedExpr::Add {
        left: Box::new(input("a")),
        right: Box::new(constant("0.000000000001")),
    };
    assert_eq!(
        run(&add, "10000000000000000").unwrap(),
        exact("10000000000000000.000000000001")
    );
    let double = DerivedExpr::Mul {
        left: Box::new(input("a")),
        right: Box::new(constant("2")),
    };
    assert_eq!(
        run(&double, "5000000000000000000000000000").unwrap(),
        exact("10000000000000000000000000000")
    );
    // 8 × 10^28 leaves the 96-bit coefficient.
    assert_eq!(
        run(&double, "40000000000000000000000000000")
            .unwrap_err()
            .reason_code(),
        "derived_formula_inexact"
    );
}

#[test]
fn decimal_text_holds_28_significant_digits_and_decimal_a_96_bit_coefficient() {
    let max = exact("79228162514264337593543950335");
    assert!(fits_decimal(&max));
    assert!(!fits_decimal_text(&max));
    let beyond = max.checked_add(&exact("1")).unwrap();
    assert!(!fits_decimal(&beyond));
    assert!(fits_decimal_text(&exact("9999999999999999999999999999")));
}

#[test]
fn a_decimal_holds_28_fractional_digits() {
    assert!(fits_decimal(&exact("0.0000000000000000000000000001")));
    assert!(fits_decimal(&exact("9999999999999999999999999999")));
    assert!(!fits_decimal(&exact("1").checked_div(&exact("3")).unwrap()));
    let wide = exact("100000000000000000000")
        .checked_add(&exact("0.0000000001"))
        .unwrap();
    assert!(!fits_decimal(&wide));
    let tiny = exact("0.0000000000000001")
        .checked_mul(&exact("0.0000000000000001"))
        .unwrap();
    assert!(!fits_decimal(&tiny));
}
