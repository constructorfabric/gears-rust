#![allow(clippy::expect_used, clippy::unwrap_used)]

use bss_money::exact::{BigInt, ExactAmount, ExactError};

use super::ArithmeticBudget;

/// The integer of `count` nines.
fn integer(count: usize) -> Result<ExactAmount, ExactError> {
    ExactAmount::from_parts("9".repeat(count).parse().unwrap(), BigInt::from(1))
}

fn digits(count: usize) -> ExactAmount {
    integer(count).unwrap()
}

#[test]
fn generation_one_records_the_money_crate_limits() {
    // The money crate itself holds a reduced integer of `reduced_digits` digits and refuses one
    // more: the budget is the crate's limit, not a number of its own.
    let reduced = usize::try_from(ArithmeticBudget::GEN_1.reduced_digits).unwrap();
    assert!(integer(reduced).is_ok());
    assert_eq!(
        integer(reduced + 1).unwrap_err(),
        ExactError::ArithmeticLimit
    );
    // Scratch products are the cross products of two operands at the reduced bound.
    assert_eq!(
        ArithmeticBudget::GEN_1.scratch_digits,
        2 * ArithmeticBudget::GEN_1.reduced_digits
    );
}

#[test]
fn operands_within_the_budget_pass() {
    let budget = ArithmeticBudget {
        reduced_digits: 10,
        scratch_digits: 20,
    };
    budget
        .check_operands("test", &digits(10), &digits(10))
        .unwrap();
}

#[test]
fn an_operand_beyond_the_budget_fails_before_the_multiply() {
    let budget = ArithmeticBudget {
        reduced_digits: 10,
        scratch_digits: 20,
    };
    let error = budget
        .check_operands("test", &digits(11), &digits(1))
        .unwrap_err();
    assert_eq!(error.reason_code(), "precision_overflow");
    let narrow = ArithmeticBudget {
        reduced_digits: 10,
        scratch_digits: 15,
    };
    let error = narrow
        .check_operands("test", &digits(8), &digits(8))
        .unwrap_err();
    assert_eq!(error.reason_code(), "precision_overflow");
}

#[test]
fn the_budget_is_part_of_the_wire_input() {
    let json = serde_json::to_string(&ArithmeticBudget::GEN_1).unwrap();
    assert_eq!(json, r#"{"reduced_digits":256,"scratch_digits":512}"#);
}
