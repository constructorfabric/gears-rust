#![allow(clippy::expect_used, clippy::unwrap_used)]

use bigdecimal::num_bigint::BigInt;

use super::*;

fn big(text: &str) -> BigInt {
    text.parse().unwrap()
}

fn ratio(numerator: i128, denominator: i128) -> ExactAmount {
    ExactAmount::from_coefficient(numerator, 0)
        .checked_div(&ExactAmount::from_coefficient(denominator, 0))
        .unwrap()
}

#[test]
fn parts_read_both_text_forms_back_reduced() {
    for (value, numerator, denominator) in [
        (ratio(47, 400), "47", "400"),
        (ratio(-1, 3), "-1", "3"),
        (ratio(2, 4), "1", "2"),
        (ratio(0, 7), "0", "1"),
        (ratio(30, 1), "30", "1"),
        (ratio(-5, 2), "-5", "2"),
        (
            ExactAmount::from_coefficient(1, 28),
            "1",
            &format!("1{}", "0".repeat(28)),
        ),
    ] {
        assert_eq!(
            parts(&value).unwrap(),
            (big(numerator), big(denominator)),
            "{value}"
        );
    }
}

#[test]
fn integers_of_any_length_are_built_exactly() {
    for text in [
        "0".to_owned(),
        "1".to_owned(),
        "-36".to_owned(),
        "9".repeat(36),
        "9".repeat(37),
        format!("-1{}", "0".repeat(72)),
        "123456789".repeat(14),
    ] {
        let value = from_integer(&big(&text)).unwrap();
        assert_eq!(
            parts(&value).unwrap(),
            (big(&text), BigInt::from(1)),
            "{text}"
        );
    }
}

#[test]
fn fractions_round_trip_through_their_parts() {
    let numerator = big(&format!("-{}", "7".repeat(80)));
    let denominator = big(&format!("3{}", "0".repeat(60)));
    let value = from_parts(&numerator, &denominator).unwrap();
    let (n, d) = parts(&value).unwrap();
    assert_eq!(from_parts(&n, &d).unwrap(), value);
    assert_eq!(n * &denominator, &d * numerator);
    assert_eq!(digit_counts(&ratio(-47, 400)).unwrap(), (2, 3));
}

#[test]
fn a_zero_denominator_is_a_contract_violation() {
    let error = from_parts(&BigInt::from(1), &BigInt::from(0)).unwrap_err();
    assert_eq!(error.reason_code(), "money_contract_violation");
}

#[test]
fn scaled_decimals_are_exact() {
    let value = from_scaled(&big(&"9".repeat(88)), 28).unwrap();
    let (n, d) = parts(&value).unwrap();
    assert_eq!(
        (n, d),
        (big(&"9".repeat(88)), big(&format!("1{}", "0".repeat(28))))
    );
}
