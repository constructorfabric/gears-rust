//! Permanent exact allocation regressions and owner-policy boundary cases.

use super::{Residual, allocate, validate_count};
use crate::exact::{ExactAmount, ExactError, sum_posted};
use crate::{CurrencySpec, MoneyError, PostedMoney, canonical_decimal, parse_decimal};
use rust_decimal::Decimal;

fn money(text: &str, scale: u8) -> PostedMoney {
    PostedMoney::try_new(
        parse_decimal(text).unwrap(),
        CurrencySpec::try_new("EUR".into(), scale).unwrap(),
    )
    .unwrap()
}

fn texts(shares: &[PostedMoney]) -> Vec<String> {
    shares
        .iter()
        .map(|share| canonical_decimal(share.amount()))
        .collect()
}

fn assert_conservation(total: &PostedMoney, shares: &[PostedMoney]) {
    assert!(
        shares
            .iter()
            .all(|share| share.currency() == total.currency())
    );
    assert_eq!(
        sum_posted(shares, total.currency().clone()).unwrap(),
        *total
    );
}

#[test]
fn allocation_thirds_at_every_declared_scale() {
    for (scale, expected) in [
        (0, ["0", "0", "1"]),
        (2, ["0.33", "0.33", "0.34"]),
        (3, ["0.333", "0.333", "0.334"]),
        (8, ["0.33333333", "0.33333333", "0.33333334"]),
        (
            28,
            [
                "0.3333333333333333333333333333",
                "0.3333333333333333333333333333",
                "0.3333333333333333333333333334",
            ],
        ),
    ] {
        let total = money("1", scale);
        let shares = allocate(&total, &[Decimal::ONE; 3], Residual::Last).unwrap();
        assert_eq!(texts(&shares), expected);
        assert_conservation(&total, &shares);
    }
}

#[test]
fn allocation_largest_residual_and_lowest_index_tie() {
    let total = money("1", 2);
    for (weights, expected) in [
        (["1", "1", "1"], ["0.34", "0.33", "0.33"]),
        (["1", "4", "1"], ["0.17", "0.66", "0.17"]),
        (["1", "4", "4"], ["0.11", "0.45", "0.44"]),
    ] {
        let weights = weights.map(|text| parse_decimal(text).unwrap());
        let shares = allocate(&total, &weights, Residual::Largest).unwrap();
        assert_eq!(texts(&shares), expected);
        assert_conservation(&total, &shares);
    }
}

#[test]
fn allocation_half_even_ties_and_signed_reversal_net_to_zero() {
    for (text, expected) in [("0.01", ["0", "0.01"]), ("0.03", ["0.02", "0.01"])] {
        for disposition in [Residual::Last, Residual::Largest] {
            let total = money(text, 2);
            let shares = allocate(&total, &[Decimal::ONE; 2], disposition).unwrap();
            if disposition == Residual::Last {
                assert_eq!(texts(&shares), expected);
            }
            let reversal = money(&format!("-{text}"), 2);
            let reversed = allocate(&reversal, &[Decimal::ONE; 2], disposition).unwrap();
            assert_conservation(&total, &shares);
            assert_conservation(&reversal, &reversed);
            for (share, reverse) in shares.iter().zip(&reversed) {
                assert_eq!(
                    ExactAmount::from_decimal(share.amount())
                        .checked_add(&ExactAmount::from_decimal(reverse.amount()))
                        .unwrap()
                        .into_posted_exact(total.currency().clone())
                        .unwrap()
                        .amount(),
                    Decimal::ZERO
                );
            }
        }
    }
}

#[test]
fn allocation_negative_thirds_preserve_both_residual_policies() {
    let total = money("-1", 2);
    for (disposition, expected) in [
        (Residual::Last, ["-0.33", "-0.33", "-0.34"]),
        (Residual::Largest, ["-0.34", "-0.33", "-0.33"]),
    ] {
        let shares = allocate(&total, &[Decimal::ONE; 3], disposition).unwrap();
        assert_eq!(texts(&shares), expected);
        assert_conservation(&total, &shares);
    }
}

#[test]
fn allocation_last_zero_weight_receives_signed_residual() {
    for (text, expected) in [
        ("0.02", ["0.01", "0.01", "0.01", "-0.01"]),
        ("-0.02", ["-0.01", "-0.01", "-0.01", "0.01"]),
    ] {
        let total = money(text, 2);
        let weights = [Decimal::ONE, Decimal::ONE, Decimal::ONE, Decimal::ZERO];
        let last = allocate(&total, &weights, Residual::Last).unwrap();
        assert_eq!(texts(&last), expected);
        assert_conservation(&total, &last);
        let largest = allocate(&total, &weights, Residual::Largest).unwrap();
        assert_eq!(largest[3].amount(), Decimal::ZERO);
        assert_eq!(largest[0].amount(), Decimal::ZERO);
        assert_conservation(&total, &largest);
    }
}

#[test]
fn allocation_equal_positive_weights_can_leave_negative_last_share() {
    let total = money("0.04", 2);
    let shares = allocate(&total, &[Decimal::ONE; 6], Residual::Last).unwrap();
    assert_eq!(
        texts(&shares),
        ["0.01", "0.01", "0.01", "0.01", "0.01", "-0.01"]
    );
    assert_conservation(&total, &shares);
}

#[test]
fn allocation_zero_total_keeps_metadata_and_zero_weight_slots() {
    let total = money("0", 28);
    let shares = allocate(
        &total,
        &[Decimal::ZERO, Decimal::ONE, Decimal::ZERO],
        Residual::Last,
    )
    .unwrap();
    assert_eq!(texts(&shares), ["0", "0", "0"]);
    assert_conservation(&total, &shares);
    let total = money("1", 2);
    let shares = allocate(
        &total,
        &[Decimal::ZERO, Decimal::ONE, Decimal::ZERO],
        Residual::Largest,
    )
    .unwrap();
    assert_eq!(texts(&shares), ["0", "1", "0"]);
    assert_conservation(&total, &shares);
}

#[test]
fn allocation_empty_negative_zero_and_out_of_contract_weights_rejected() {
    let total = money("1", 2);
    assert_eq!(
        allocate(&total, &[], Residual::Last),
        Err(ExactError::EmptyWeights)
    );
    assert_eq!(
        allocate(&total, &[Decimal::ONE, -Decimal::ONE], Residual::Last),
        Err(ExactError::NegativeWeight)
    );
    assert_eq!(
        allocate(&total, &[Decimal::ZERO; 2], Residual::Last),
        Err(ExactError::DivisionByZero)
    );
    assert_eq!(
        allocate(&total, &[Decimal::MAX], Residual::Last),
        Err(ExactError::Money(MoneyError::AmountOutOfRange))
    );
    // A 29-digit normalized fractional coefficient also fails, below 10^28 magnitude.
    let invalid = Decimal::from_i128_with_scale(10_i128.pow(28) + 1, 28);
    assert_eq!(
        allocate(&total, &[invalid], Residual::Last),
        Err(ExactError::Money(MoneyError::AmountOutOfRange))
    );
}

#[test]
fn allocation_count_guard_follows_segment_type_without_billions_of_terms() {
    assert_eq!(validate_count(0), Err(ExactError::EmptyWeights));
    assert_eq!(validate_count(120), Ok(()));
    assert_eq!(validate_count(1001), Ok(()));
    let maximum = i32::MAX as usize;
    assert_eq!(validate_count(maximum), Ok(()));
    assert_eq!(validate_count(maximum + 1), Err(ExactError::TooManyTerms));
    assert_eq!(validate_count(usize::MAX), Err(ExactError::TooManyTerms));
}

#[test]
fn allocation_dimensionless_fractional_weights_preserve_all_digits() {
    let total = money("1", 28);
    let weights = [
        parse_decimal("0.0000000000000000000000000001").unwrap(),
        parse_decimal("0.0000000000000000000000000002").unwrap(),
    ];
    let shares = allocate(&total, &weights, Residual::Last).unwrap();
    assert_eq!(
        texts(&shares),
        [
            "0.3333333333333333333333333333",
            "0.6666666666666666666666666667"
        ]
    );
    assert_conservation(&total, &shares);
    // Trailing zeros normalize before the coefficient check.
    let shares = allocate(
        &money("1", 2),
        &[Decimal::from_i128_with_scale(10_i128.pow(28), 28)],
        Residual::Last,
    )
    .unwrap();
    assert_eq!(texts(&shares), ["1"]);
}

#[test]
fn allocation_large_weight_sum_and_products_stay_exact_before_division() {
    let total = money("9999999999999999999999999999", 0);
    let weight = total.amount();
    let shares = allocate(&total, &[weight; 3], Residual::Last).unwrap();
    assert_eq!(texts(&shares), ["3333333333333333333333333333"; 3]);
    assert_conservation(&total, &shares);
}

#[test]
fn allocation_rounded_aggregate_can_exceed_posted_bound_before_residual() {
    let total = money("9999999999999999999999999999", 0);
    for (disposition, expected) in [
        (
            Residual::Last,
            [
                "5000000000000000000000000000",
                "4999999999999999999999999999",
            ],
        ),
        (
            Residual::Largest,
            [
                "4999999999999999999999999999",
                "5000000000000000000000000000",
            ],
        ),
    ] {
        let shares = allocate(&total, &[Decimal::ONE; 2], disposition).unwrap();
        assert_eq!(texts(&shares), expected);
        assert_conservation(&total, &shares);
    }
}

#[test]
fn allocation_declared_share_bounds_are_revalidated_without_saturation() {
    let total = money("9999999999999999999999999999", 28);
    // Exact halves have a 29-digit coefficient: a bounded input total does not
    // authorize an out-of-contract declared share, even if the sum would fit.
    assert_eq!(
        allocate(&total, &[Decimal::ONE; 2], Residual::Last),
        Err(ExactError::Money(MoneyError::AmountOutOfRange))
    );
}

#[test]
fn allocation_final_residual_adjustment_rejects_out_of_contract_coefficient() {
    let total = money("1000000000000000000000000000", 2);
    let mut weights = vec![parse_decimal("0.0006").unwrap(); 17];
    weights.push(parse_decimal("100000000000000000000000000").unwrap());
    // Every initial rounded share fits. Each small share rounds to 0.01,
    // while the large share rounds to total - 0.1 (a valid coefficient).
    let sum = weights
        .iter()
        .fold(ExactAmount::from_decimal(Decimal::ZERO), |sum, &weight| {
            sum.checked_add(&ExactAmount::from_decimal(weight)).unwrap()
        });
    let rounded: Vec<_> = weights
        .iter()
        .map(|&weight| {
            ExactAmount::from_decimal(total.amount())
                .checked_mul(&ExactAmount::from_decimal(weight))
                .unwrap()
                .checked_div(&sum)
                .unwrap()
                .round_half_even(total.currency().clone())
                .unwrap()
        })
        .collect();
    assert!(
        rounded[..17]
            .iter()
            .all(|share| share.amount() == parse_decimal("0.01").unwrap())
    );
    assert_eq!(
        canonical_decimal(rounded[17].amount()),
        "999999999999999999999999999.9"
    );
    // Conserving the total requires the final large share to become
    // total - 0.17, whose normalized coefficient has 28+ digits.
    for disposition in [Residual::Last, Residual::Largest] {
        assert_eq!(
            allocate(&total, &weights, disposition),
            Err(ExactError::Money(MoneyError::AmountOutOfRange))
        );
    }
}
