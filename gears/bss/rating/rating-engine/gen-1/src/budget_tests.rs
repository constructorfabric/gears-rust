#![allow(clippy::expect_used, clippy::unwrap_used)]

use super::*;
use crate::fraction::from_integer;

fn digits(count: usize) -> ExactAmount {
    from_integer(&"9".repeat(count).parse().unwrap()).unwrap()
}

#[test]
fn generation_one_records_the_vendored_crate_limits() {
    assert_eq!(ArithmeticBudget::GEN_1.reduced_digits, 256);
    assert_eq!(ArithmeticBudget::GEN_1.scratch_digits, 512);
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
