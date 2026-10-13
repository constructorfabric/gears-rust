//! Exact monetary arithmetic regressions.
#![allow(clippy::unwrap_used)]
use super::{ExactAmount, ExactError, ScaledSum, coefficient_at_scale, sum_posted};
use crate::{CurrencySpec, MoneyError, PostedMoney};
use rust_decimal::Decimal;

#[test]
fn thirds_stay_exact_until_the_owner_rounds() {
    let one = ExactAmount::from_decimal(Decimal::ONE);
    let three = ExactAmount::from_decimal(Decimal::from(3));
    let third = one.checked_div(&three).unwrap();
    let sum = third
        .checked_add(&third)
        .unwrap()
        .checked_add(&third)
        .unwrap();
    let eur = CurrencySpec::try_new("EUR".into(), 2).unwrap();
    assert_eq!(sum.into_posted_exact(eur).unwrap().amount(), Decimal::ONE);
}

use bigdecimal::num_bigint::BigInt;
use bigdecimal::num_traits::{One, Zero};

fn currency(scale: u8) -> CurrencySpec {
    CurrencySpec::try_new("EUR".into(), scale).unwrap()
}

fn exact(text: &str) -> ExactAmount {
    ExactAmount::from_decimal(Decimal::from_str_exact(text).unwrap())
}

fn fraction(numerator: BigInt, denominator: BigInt) -> ExactAmount {
    ExactAmount::reduce(numerator, denominator).unwrap()
}

#[test]
fn half_even_rounds_positive_and_negative_ties() {
    for (input, expected) in [
        ("0.005", "0"),
        ("-0.005", "0"),
        ("0.015", "0.02"),
        ("-0.015", "-0.02"),
        ("0.0049", "0"),
        ("-0.0049", "0"),
        ("0.0051", "0.01"),
        ("-0.0051", "-0.01"),
    ] {
        assert_eq!(
            exact(input).round_half_even(currency(2)).unwrap().amount(),
            Decimal::from_str_exact(expected).unwrap()
        );
    }
}

#[test]
fn thirds_require_explicit_rounding() {
    let third = exact("1").checked_div(&exact("3")).unwrap();
    assert_eq!(
        third.clone().into_posted_exact(currency(2)),
        Err(ExactError::Money(MoneyError::InvalidPostingIncrement))
    );
    assert_eq!(
        third.round_half_even(currency(2)).unwrap().amount(),
        Decimal::from_str_exact("0.33").unwrap()
    );
    let two_thirds = exact("2").checked_div(&exact("3")).unwrap();
    assert_eq!(
        two_thirds.round_half_even(currency(2)).unwrap().amount(),
        Decimal::from_str_exact("0.67").unwrap()
    );
}

#[test]
fn scale_31_product_stays_exact() {
    let value = exact("0.000000000000001")
        .checked_mul(&exact("0.0000000000000001"))
        .unwrap();
    assert_eq!(value.numerator, BigInt::one());
    assert_eq!(value.denominator, BigInt::from(10).pow(31));
    assert_eq!(
        value.clone().into_posted_exact(currency(28)),
        Err(ExactError::Money(MoneyError::InvalidPostingIncrement))
    );
    assert_eq!(
        value.round_half_even(currency(28)).unwrap().amount(),
        Decimal::ZERO
    );
}

#[test]
fn ceil_is_the_smallest_integer_not_below_the_value() {
    for (numerator, denominator, expected) in [
        (7, 3, "3"),
        (6, 3, "2"),
        (0, 1, "0"),
        (-7, 3, "-2"),
        (-6, 3, "-2"),
        (1, 3, "1"),
        (-1, 3, "0"),
        (1, 1_000, "1"),
        (-1, 1_000, "0"),
        (3, 2, "2"),
        (-3, 2, "-1"),
        (5, 1, "5"),
        (-5, 1, "-5"),
    ] {
        let value = fraction(BigInt::from(numerator), BigInt::from(denominator));
        assert_eq!(value.ceil().unwrap(), exact(expected));
    }
}

#[test]
fn ceil_of_a_decimal_keeps_a_whole_value_and_lifts_a_fraction() {
    let cases = [
        ("0", "0"),
        ("2", "2"),
        ("2.0000000000000000000000000001", "3"),
        ("2.9999999999999999999999999999", "3"),
        ("-2.9999999999999999999999999999", "-2"),
        ("-0.0000000000000000000000000001", "0"),
        ("0.0000000000000000000000000001", "1"),
    ];
    // One comparison of all cases: a failure shows the whole list, in case order.
    let ceilings: Vec<ExactAmount> = cases
        .iter()
        .map(|(input, _)| exact(input).ceil().unwrap())
        .collect();
    let expected: Vec<ExactAmount> = cases.iter().map(|(_, ceiling)| exact(ceiling)).collect();
    assert_eq!(ceilings, expected);
}

#[test]
fn ceil_of_a_negative_fraction_is_zero_without_a_sign() {
    let ceiling = exact("-0.5").ceil().unwrap();
    assert!(!ceiling.is_negative());
    assert_eq!(ceiling.canonical_at_scale(0).unwrap(), "0");
    assert_eq!(ceiling.denominator, BigInt::one());
}

#[test]
fn ceil_result_is_a_reduced_integer() {
    let ceiling = fraction(BigInt::from(100), BigInt::from(30))
        .ceil()
        .unwrap();
    assert_eq!(ceiling.numerator, BigInt::from(4));
    assert_eq!(ceiling.denominator, BigInt::one());
}

#[test]
fn ceil_handles_the_largest_budget_values_and_rejects_beyond_it() {
    let largest = BigInt::from(10).pow(256) - 1_u8;
    let whole = fraction(largest.clone(), BigInt::one());
    assert_eq!(whole.ceil().unwrap(), whole);
    // Just over a whole number rounds up without leaving the budget.
    let half = fraction(largest, BigInt::from(2));
    assert_eq!(
        half.ceil().unwrap(),
        fraction(BigInt::from(10).pow(256) / 2_u8, BigInt::one())
    );
    let beyond = BigInt::from(10).pow(256);
    for invalid in [
        ExactAmount {
            numerator: beyond.clone(),
            denominator: BigInt::one(),
        },
        ExactAmount {
            numerator: BigInt::one(),
            denominator: beyond,
        },
    ] {
        assert_eq!(invalid.ceil(), Err(ExactError::ArithmeticLimit));
    }
}

#[test]
fn reduction_normalizes_sign_and_zero() {
    let reduced = fraction(BigInt::from(12), BigInt::from(-18));
    assert_eq!(reduced.numerator, BigInt::from(-2));
    assert_eq!(reduced.denominator, BigInt::from(3));
    assert_eq!(
        fraction(BigInt::from(-12), BigInt::from(-18)),
        fraction(BigInt::from(2), BigInt::from(3))
    );
    let zero = fraction(BigInt::zero(), -BigInt::from(10).pow(300));
    assert_eq!(zero.numerator, BigInt::zero());
    assert_eq!(zero.denominator, BigInt::one());
    assert_eq!(
        ExactAmount::reduce(BigInt::one(), BigInt::zero()),
        Err(ExactError::DivisionByZero)
    );
}

#[test]
fn division_rejects_zero_and_normalizes_negative_divisor() {
    assert_eq!(
        exact("1").checked_div(&exact("0")),
        Err(ExactError::DivisionByZero)
    );
    assert_eq!(
        exact("1").checked_div(&exact("-3")).unwrap(),
        fraction(BigInt::from(-1), BigInt::from(3))
    );
}

#[test]
fn reduced_numerator_and_denominator_budget_boundaries() {
    let largest = BigInt::from(10).pow(256) - 1_u8;
    let beyond = BigInt::from(10).pow(256);
    assert!(ExactAmount::reduce(largest.clone(), BigInt::one()).is_ok());
    assert!(ExactAmount::reduce(BigInt::one(), largest).is_ok());
    assert_eq!(
        ExactAmount::reduce(beyond.clone(), BigInt::one()),
        Err(ExactError::ArithmeticLimit)
    );
    assert_eq!(
        ExactAmount::reduce(BigInt::one(), beyond),
        Err(ExactError::ArithmeticLimit)
    );
    let scratch = BigInt::from(10).pow(300);
    assert_eq!(fraction(scratch.clone(), scratch), exact("1"));
}

#[test]
fn invalid_operands_are_rejected_before_arithmetic() {
    let beyond = BigInt::from(10).pow(256);
    let invalid = ExactAmount {
        numerator: beyond.clone(),
        denominator: BigInt::one(),
    };
    let invalid_denominator = ExactAmount {
        numerator: BigInt::one(),
        denominator: beyond,
    };
    for invalid in [invalid, invalid_denominator] {
        assert_eq!(
            invalid.checked_add(&exact("0")),
            Err(ExactError::ArithmeticLimit)
        );
        assert_eq!(
            invalid.checked_sub(&exact("0")),
            Err(ExactError::ArithmeticLimit)
        );
        assert_eq!(
            invalid.checked_mul(&exact("0")),
            Err(ExactError::ArithmeticLimit)
        );
        assert_eq!(
            exact("1").checked_div(&invalid),
            Err(ExactError::ArithmeticLimit)
        );
    }
}

#[test]
fn large_cross_products_reduce_before_final_limit() {
    let large = BigInt::from(10).pow(255) + 1_u8;
    let small = fraction(BigInt::one(), large.clone());
    let complement = fraction(&large - 1, large.clone());
    assert_eq!(small.checked_add(&complement).unwrap(), exact("1"));
    assert_eq!(small.checked_sub(&small).unwrap(), exact("0"));
    let ratio = fraction(large.clone(), &large - 2);
    let reciprocal = fraction(&large - 2, large);
    assert_eq!(ratio.checked_mul(&reciprocal).unwrap(), exact("1"));
    assert_eq!(ratio.checked_div(&ratio).unwrap(), exact("1"));
}

#[test]
fn sum_with_513_digit_scratch_is_allowed_before_reduction() {
    let denominator = BigInt::from(10).pow(256) - 2_u8;
    let amount = fraction(&denominator - 1, denominator);
    assert_eq!(
        (&amount.numerator * &amount.denominator * 2_u8)
            .to_str_radix(10)
            .len(),
        513
    );
    let doubled = amount.checked_add(&amount).unwrap();
    assert_eq!(doubled.checked_div(&exact("2")).unwrap(), amount);
}

#[test]
fn final_operation_budgets_reject_growth() {
    let large = fraction(BigInt::from(10).pow(256) - 1_u8, BigInt::one());
    assert_eq!(
        large.checked_add(&exact("1")),
        Err(ExactError::ArithmeticLimit)
    );
    assert_eq!(
        large.checked_sub(&exact("-1")),
        Err(ExactError::ArithmeticLimit)
    );
    assert_eq!(
        large.checked_mul(&exact("2")),
        Err(ExactError::ArithmeticLimit)
    );
    let small = fraction(BigInt::one(), BigInt::from(10).pow(256) - 1_u8);
    assert_eq!(
        small.checked_div(&exact("2")),
        Err(ExactError::ArithmeticLimit)
    );
}

#[test]
fn scratch_budgets_reject_oversized_values() {
    assert_eq!(
        ExactAmount::reduce(BigInt::from(10).pow(513), BigInt::one()),
        Err(ExactError::ArithmeticLimit)
    );
    assert_eq!(
        ExactAmount::reduce(BigInt::one(), BigInt::from(10).pow(512)),
        Err(ExactError::ArithmeticLimit)
    );
}

#[test]
fn posted_sum_narrows_only_the_final_amount() {
    let maximum = "9999999999999999999999999999";
    let max = PostedMoney::try_new(Decimal::from_str_exact(maximum).unwrap(), currency(2)).unwrap();
    let negative = PostedMoney::try_new(-max.amount(), currency(2)).unwrap();
    assert_eq!(
        sum_posted(&[max.clone(), max.clone(), negative], currency(2)).unwrap(),
        max
    );
    assert_eq!(
        sum_posted(&[max.clone(), max], currency(2)),
        Err(ExactError::Money(MoneyError::AmountOutOfRange))
    );
    assert_eq!(
        sum_posted(&[] as &[PostedMoney], currency(2))
            .unwrap()
            .amount(),
        Decimal::ZERO
    );
}

#[test]
fn posting_normalizes_coefficient_before_checking_limit() {
    let amount = exact("9999999999999999999999999999")
        .into_posted_exact(currency(28))
        .unwrap();
    assert_eq!(amount.amount().scale(), 0);
    assert_eq!(amount.currency().scale(), 28);
    let overflow = exact("9999999999999999999999999999")
        .checked_add(&exact("1"))
        .unwrap();
    assert_eq!(
        overflow.clone().into_posted_exact(currency(0)),
        Err(ExactError::Money(MoneyError::AmountOutOfRange))
    );
    assert_eq!(
        overflow.round_half_even(currency(0)),
        Err(ExactError::Money(MoneyError::AmountOutOfRange))
    );
    assert_eq!(
        exact("-0.005")
            .round_half_even(currency(2))
            .unwrap()
            .amount()
            .to_string(),
        "0"
    );
}

#[test]
fn posted_sum_rejects_metadata_mismatches() {
    let usd = PostedMoney::try_new(
        Decimal::ONE,
        CurrencySpec::try_new("USD".into(), 2).unwrap(),
    )
    .unwrap();
    assert_eq!(
        sum_posted(&[usd], currency(2)),
        Err(ExactError::Money(MoneyError::CurrencyMismatch))
    );
    let different_scale = PostedMoney::try_new(Decimal::ONE, currency(3)).unwrap();
    assert_eq!(
        sum_posted(&[different_scale], currency(2)),
        Err(ExactError::Money(MoneyError::ScaleMismatch))
    );
}

#[test]
fn mixed_scale_sum_has_a_59_digit_reduced_numerator() {
    let maximum = exact("9999999999999999999999999999");
    let mut sum = exact("0");
    for _ in 0..999 {
        sum = sum.checked_add(&maximum).unwrap();
    }
    sum = sum
        .checked_add(&exact("0.0000000000000000000000000001"))
        .unwrap();
    assert_eq!(sum.numerator.to_str_radix(10).len(), 59);
    assert_eq!(sum.denominator, BigInt::from(10).pow(28));
    let expected = (BigInt::from(10).pow(28) - 1) * 999 * BigInt::from(10).pow(28) + 1;
    assert_eq!(sum.numerator, expected);
    assert_eq!(
        sum.into_posted_exact(currency(28)),
        Err(ExactError::Money(MoneyError::AmountOutOfRange))
    );
}

#[test]
fn exact_half_even_helper_keeps_scale_and_reduced_resource_bounds() {
    assert_eq!(
        exact("1").round_half_even_exact(29),
        Err(ExactError::Money(MoneyError::ScaleOutOfRange))
    );
    let invalid = ExactAmount {
        numerator: BigInt::from(10).pow(256),
        denominator: BigInt::one(),
    };
    assert_eq!(
        invalid.round_half_even_exact(0),
        Err(ExactError::ArithmeticLimit)
    );
    let wide = fraction(BigInt::from(10).pow(256) - 2_u8, BigInt::from(3));
    assert_eq!(
        wide.round_half_even_exact(28),
        Err(ExactError::ArithmeticLimit)
    );
    let above_money = exact("9999999999999999999999999999")
        .checked_mul(&exact("2"))
        .unwrap();
    assert_eq!(
        above_money.clone().round_half_even_exact(0).unwrap(),
        above_money
    );
    assert_eq!(
        above_money.round_half_even(currency(0)),
        Err(ExactError::Money(MoneyError::AmountOutOfRange))
    );
}

#[test]
fn intermediate_evidence_format_does_not_apply_posted_coefficient_bounds() {
    let huge = ExactAmount::from_decimal("1000000000000000000000000000".parse().unwrap());
    let fractional = huge
        .checked_sub(&ExactAmount::from_decimal("0.01".parse().unwrap()))
        .unwrap();
    assert_eq!(
        fractional.canonical_at_scale(2).unwrap(),
        "999999999999999999999999999.99"
    );
    assert!(matches!(
        fractional.into_posted_exact(CurrencySpec::try_new("USD".to_owned(), 2).unwrap()),
        Err(ExactError::Money(MoneyError::AmountOutOfRange))
    ));
    for (text, scale, expected) in [
        ("0", 28, "0"),
        ("-0.01", 2, "-0.01"),
        ("12.3400", 4, "12.34"),
        ("100", 2, "100"),
        ("-5", 0, "-5"),
        ("1200", 0, "1200"),
        ("1200.000", 0, "1200"),
        (
            "0.0000000000000000000000000001",
            28,
            "0.0000000000000000000000000001",
        ),
    ] {
        assert_eq!(
            ExactAmount::from_decimal(text.parse().unwrap())
                .canonical_at_scale(scale)
                .unwrap(),
            expected
        );
    }
    assert!(matches!(
        ExactAmount::from_decimal("0.01".parse().unwrap()).canonical_at_scale(0),
        Err(ExactError::Money(MoneyError::InvalidPostingIncrement))
    ));
    assert!(matches!(
        huge.canonical_at_scale(29),
        Err(ExactError::Money(MoneyError::ScaleOutOfRange))
    ));
}

#[test]
fn exact_amounts_display_as_decimals_or_fractions() {
    assert_eq!(exact("12.340").to_string(), "12.34");
    assert_eq!(
        exact("1").checked_div(&exact("3")).unwrap().to_string(),
        "1/3"
    );
}

#[test]
fn exact_amounts_order_by_value_and_agree_with_equality() {
    use std::cmp::Ordering;
    let third = exact("1").checked_div(&exact("3")).unwrap();
    let minus_third = exact("-1").checked_div(&exact("3")).unwrap();
    assert!(exact("0.33") < third);
    assert!(third < exact("0.34"));
    assert!(minus_third < exact("0"));
    assert!(exact("-0.34") < minus_third);
    assert_eq!(exact("12.340").cmp(&exact("12.34")), Ordering::Equal);
    assert_eq!(third.cmp(&third.clone()), Ordering::Equal);
    // Values at the reduced budget compare without tripping a scratch limit.
    let large = fraction(BigInt::from(10).pow(256) - 1_u8, BigInt::one());
    let tiny = fraction(BigInt::one(), BigInt::from(10).pow(256) - 1_u8);
    let negative_large = fraction(-(BigInt::from(10).pow(256) - 1_u8), BigInt::one());
    assert!(tiny < large);
    assert!(negative_large < tiny);
    assert_eq!(
        [exact("2"), third, exact("-1")].into_iter().max().unwrap(),
        exact("2")
    );
}

/// Equality is exact value equality, defined through the order: an unreduced
/// fraction equals its reduced form, so `==` and `cmp` cannot disagree.
#[test]
fn equality_agrees_with_the_order_even_unreduced() {
    let half = ExactAmount {
        numerator: BigInt::one(),
        denominator: BigInt::from(2),
    };
    let two_quarters = ExactAmount {
        numerator: BigInt::from(2),
        denominator: BigInt::from(4),
    };
    assert_eq!(half.cmp(&two_quarters), std::cmp::Ordering::Equal);
    assert_eq!(half, two_quarters);
    assert_ne!(half, exact("0.25"));
}

/// The sum takes borrowed postings from any cloneable iterator and a borrowed
/// spec, and checks every term's metadata before adding any.
#[test]
fn posted_sum_borrows_terms_and_spec() {
    let values = [
        PostedMoney::try_new(Decimal::from_str_exact("0.1").unwrap(), currency(2)).unwrap(),
        PostedMoney::try_new(Decimal::from_str_exact("0.2").unwrap(), currency(2)).unwrap(),
    ];
    let spec = currency(2);
    let by_ref = sum_posted(values.iter(), &spec).unwrap();
    assert_eq!(by_ref, sum_posted(&values, spec).unwrap());
    assert_eq!(by_ref.amount(), Decimal::from_str_exact("0.3").unwrap());
    assert_eq!(
        sum_posted(std::iter::empty(), &currency(0))
            .unwrap()
            .amount(),
        Decimal::ZERO
    );
    let maximum = PostedMoney::try_new(
        Decimal::from_str_exact("9999999999999999999999999999").unwrap(),
        currency(0),
    )
    .unwrap();
    let late_mismatch = PostedMoney::try_new(Decimal::ONE, currency(1)).unwrap();
    assert_eq!(
        sum_posted([&maximum, &maximum, &late_mismatch], &currency(0)),
        Err(ExactError::Money(MoneyError::ScaleMismatch)),
        "a metadata mismatch wins over the final overflow"
    );
}

/// The coefficient rule ignores trailing zeros and refuses finer amounts and
/// `i128` overflow instead of truncating.
#[test]
fn coefficient_at_scale_is_exact_or_absent() {
    let dec = |text: &str| Decimal::from_str_exact(text).unwrap();
    assert_eq!(coefficient_at_scale(dec("12.3"), 2), Some(1230));
    assert_eq!(coefficient_at_scale(dec("1.50"), 1), Some(15));
    assert_eq!(coefficient_at_scale(dec("-0.001"), 3), Some(-1));
    assert_eq!(coefficient_at_scale(dec("0.125"), 2), None);
    assert_eq!(coefficient_at_scale(dec("100000000000"), 28), None);
    assert_eq!(
        ExactAmount::from_coefficient(1230, 2),
        exact("12.3"),
        "the inverse is exact"
    );
}

/// A scaled sum keeps the `i128` form while it can and moves to the exact
/// fraction without losing value; narrowing normalizes, so a total whose
/// coefficient is past the decimal carrier is still a valid posting.
#[test]
fn scaled_sum_survives_coefficient_and_carrier_overflow() {
    let dec = |text: &str| Decimal::from_str_exact(text).unwrap();
    // 5 + 5 at scale 28: the coefficient 10^29 passes the 96-bit carrier.
    let mut sum = ScaledSum::zero(28);
    sum.add_decimal(dec("5")).unwrap();
    sum.add_decimal(dec("5")).unwrap();
    assert_eq!(sum.to_posted(currency(28)).unwrap().amount(), dec("10"));
    // 10^11 at scale 28 has no i128 coefficient: the fraction form carries it.
    let mut big = ScaledSum::zero(28);
    big.add_decimal(dec("100000000000")).unwrap();
    big.add_decimal(dec("100000000000")).unwrap();
    assert_eq!(
        big.to_posted(currency(28)).unwrap().amount(),
        dec("200000000000")
    );
    // Value equality across forms and scales.
    let mut small = ScaledSum::zero(2);
    small.add_decimal(dec("0.5")).unwrap();
    assert_eq!(small, ScaledSum::from_exact(exact("0.50"), 2));
    assert_eq!(small, {
        let mut other = ScaledSum::zero(1);
        other.add_decimal(dec("0.5")).unwrap();
        other
    });
    let mut grand = ScaledSum::zero(28);
    grand.add_sum(&big).unwrap();
    grand.add_sum(&sum).unwrap();
    assert_eq!(grand.to_exact(), exact("200000000010"));
    assert!(!grand.is_zero());
    let mut cancel = ScaledSum::zero(28);
    cancel.add_sum(&big).unwrap();
    cancel.add_decimal(dec("-200000000000")).unwrap();
    assert!(cancel.is_zero());
    // A total past the posting contract is the range error, not a truncation.
    let mut over = ScaledSum::zero(0);
    over.add_decimal(dec("9999999999999999999999999999"))
        .unwrap();
    over.add_decimal(Decimal::ONE).unwrap();
    assert_eq!(
        over.to_posted(currency(0)),
        Err(ExactError::Money(MoneyError::AmountOutOfRange))
    );
}
