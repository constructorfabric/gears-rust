//! Exact translation, quote precision and functional residual contract tests.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
use super::*;
use bss_ledger_sdk::MoneyError;
fn spec(code: &str, scale: u8) -> CurrencySpec {
    CurrencySpec::try_new(code.into(), scale).unwrap()
}
fn decimal(text: &str) -> Decimal {
    Decimal::from_str_exact(text).unwrap()
}
fn money(text: &str, code: &str, scale: u8) -> PostedMoney {
    PostedMoney::try_new(decimal(text), spec(code, scale)).unwrap()
}
fn old_money(minor: i64) -> PostedMoney {
    PostedMoney::try_new(
        Decimal::from_i128_with_scale(i128::from(minor), 2),
        spec("EUR", 2),
    )
    .unwrap()
}
fn dr(minor: i64) -> FxLine {
    FxLine {
        amount: old_money(minor),
        side: Side::Debit,
    }
}
fn cr(minor: i64) -> FxLine {
    FxLine {
        amount: old_money(minor),
        side: Side::Credit,
    }
}
fn translated(lines: &[FxLine], rate: &str, anchor: usize) -> Vec<PostedMoney> {
    translate_entry(lines, decimal(rate), spec("USD", 2), anchor).unwrap()
}
fn check(lines: &[FxLine], values: &[PostedMoney]) {
    let mut net = ExactAmount::from_decimal(Decimal::ZERO);
    for (line, value) in lines.iter().zip(values) {
        let exact = ExactAmount::from_decimal(value.amount());
        net = match line.side {
            Side::Debit => net.checked_add(&exact),
            Side::Credit => net.checked_sub(&exact),
        }
        .unwrap();
    }
    assert_eq!(net, ExactAmount::from_decimal(Decimal::ZERO));
}
fn expected(minors: &[i64]) -> Vec<PostedMoney> {
    minors
        .iter()
        .map(|v| {
            PostedMoney::try_new(
                Decimal::from_i128_with_scale(i128::from(*v), 2),
                spec("USD", 2),
            )
            .unwrap()
        })
        .collect()
}
#[test]
fn identity_rate_mirrors_transaction() {
    let lines = [dr(1000), cr(1000)];
    let func = translated(&lines, "1", 0);
    assert_eq!(func, expected(&[1000, 1000]));
    check(&lines, &func);
}
#[test]
fn scaled_rate_balances_both_columns() {
    let lines = [dr(1000), cr(1000)];
    let func = translated(&lines, "1.1", 0);
    assert_eq!(func, expected(&[1100, 1100]));
    check(&lines, &func);
}
#[test]
fn rounding_residual_is_plugged_onto_the_anchor() {
    let lines = [dr(1), dr(1), cr(2)];
    let func = translated(&lines, "1.5", 2);
    assert_eq!(func, expected(&[2, 2, 4]));
    check(&lines, &func);
}
#[test]
fn residual_plug_is_deterministic() {
    let lines = [dr(1), dr(1), cr(2)];
    assert_eq!(translated(&lines, "1.5", 2), translated(&lines, "1.5", 2));
}
#[test]
fn dr_anchor_absorbs_residual() {
    let lines = [cr(1), cr(1), dr(2)];
    let func = translated(&lines, "1.5", 2);
    assert_eq!(func, expected(&[2, 2, 4]));
    check(&lines, &func);
}
#[test]
fn non_positive_rate_is_rejected() {
    assert_eq!(
        translate_entry(&[dr(1000), cr(1000)], Decimal::ZERO, spec("USD", 2), 0),
        Err(FxTranslateError::RateNonPositive)
    );
}
#[test]
fn translate_amount_rejects_non_positive_rate() {
    for rate in [Decimal::ZERO, decimal("-1.1")] {
        assert_eq!(
            translate_amount(&old_money(10000), rate, spec("USD", 2)),
            Err(FxTranslateError::RateNonPositive)
        );
    }
    assert_eq!(
        translate_amount(&old_money(10000), decimal("1.1"), spec("USD", 2)),
        Ok(expected(&[11000]).remove(0))
    );
}
#[test]
fn anchor_out_of_bounds_is_rejected() {
    assert_eq!(
        translate_entry(&[dr(1000), cr(1000)], Decimal::ONE, spec("USD", 2), 2),
        Err(FxTranslateError::AnchorOutOfBounds)
    );
}
#[test]
fn task8_cross_scale_examples_and_signed_translation() {
    assert_eq!(
        translate_amount(&money("12.34", "EUR", 2), decimal("160"), spec("JPY", 0)).unwrap(),
        money("1974", "JPY", 0)
    );
    assert_eq!(
        translate_amount(&money("100", "JPY", 0), decimal("0.00625"), spec("EUR", 2)).unwrap(),
        money("0.62", "EUR", 2)
    );
    assert_eq!(
        translate_amount(&money("-100", "JPY", 0), decimal("0.00625"), spec("EUR", 2)).unwrap(),
        money("-0.62", "EUR", 2)
    );
}
#[test]
fn original_quote_digits_and_bounds() {
    for rate in [
        "1.123456789",
        "0.0000001",
        "0.0000000000000000000000000001",
        "9999999999999999999999999999",
    ] {
        assert_eq!(
            translate_amount(&money("1", "EUR", 0), decimal(rate), spec("USD", 28))
                .unwrap()
                .amount(),
            decimal(rate)
        );
    }
    assert_eq!(
        translate_amount(
            &money("1", "EUR", 0),
            decimal("10000000000000000000000000000"),
            spec("USD", 0)
        ),
        Err(FxTranslateError::Exact(ExactError::Money(
            MoneyError::AmountOutOfRange
        )))
    );
}
/// A 29-digit quote is out of contract even when the product fits the target:
/// `1e-28 * 1e28 = 1` would post fine at USD/0, so only the quote bound in
/// `validate_rate` can reject it (the final-amount check cannot).
#[test]
fn out_of_contract_quote_is_rejected_even_when_the_product_fits() {
    let tiny = money("0.0000000000000000000000000001", "EUR", 28);
    let quote_29_digits = decimal("10000000000000000000000000000");
    assert_eq!(
        translate_amount(&tiny, quote_29_digits, spec("USD", 0)),
        Err(FxTranslateError::Exact(ExactError::Money(
            MoneyError::AmountOutOfRange
        )))
    );
    let line = FxLine {
        amount: tiny.clone(),
        side: Side::Debit,
    };
    let other = FxLine {
        amount: tiny.clone(),
        side: Side::Credit,
    };
    assert_eq!(
        translate_entry(&[line, other], quote_29_digits, spec("USD", 0), 0),
        Err(FxTranslateError::Exact(ExactError::Money(
            MoneyError::AmountOutOfRange
        )))
    );
    // The largest in-contract quote with the same tiny source is accepted.
    assert_eq!(
        translate_amount(
            &tiny,
            decimal("9999999999999999999999999999"),
            spec("USD", 28)
        )
        .unwrap()
        .amount(),
        decimal("0.9999999999999999999999999999")
    );
}
#[test]
fn identity_requires_full_spec_and_explicit_rescale_rounds() {
    let source = money("1.25", "EUR", 2);
    assert_eq!(
        translate_amount(&source, Decimal::ONE, source.currency().clone()).unwrap(),
        source
    );
    assert_eq!(
        translate_amount(&source, Decimal::ONE, spec("EUR", 1)).unwrap(),
        money("1.2", "EUR", 1)
    );
}
#[test]
fn entry_metadata_and_positive_side_legs() {
    let mismatched = [
        FxLine {
            amount: money("1", "EUR", 2),
            side: Side::Debit,
        },
        FxLine {
            amount: money("1", "EUR", 3),
            side: Side::Credit,
        },
    ];
    assert_eq!(
        translate_entry(&mismatched, Decimal::ONE, spec("USD", 2), 0),
        Err(FxTranslateError::Exact(ExactError::Money(
            MoneyError::ScaleMismatch
        )))
    );
    assert_eq!(
        translate_entry(&[dr(0), cr(1)], Decimal::ONE, spec("USD", 2), 0),
        Err(FxTranslateError::NonPositiveLine)
    );
}
#[test]
fn final_anchor_sign_and_range_and_wide_net_cancellation() {
    assert_eq!(
        translate_entry(&[dr(1), cr(1)], decimal("0.1"), spec("USD", 2), 0),
        Err(FxTranslateError::ResidualExceedsAnchor)
    );
    let huge = money("9999999999999999999999999999", "EUR", 0);
    let lines = [
        FxLine {
            amount: huge.clone(),
            side: Side::Debit,
        },
        FxLine {
            amount: huge.clone(),
            side: Side::Debit,
        },
        FxLine {
            amount: huge.clone(),
            side: Side::Credit,
        },
        FxLine {
            amount: huge.clone(),
            side: Side::Credit,
        },
    ];
    check(
        &lines,
        &translate_entry(&lines, Decimal::ONE, spec("USD", 0), 0).unwrap(),
    );
    let overflowing = [
        FxLine {
            amount: huge.clone(),
            side: Side::Debit,
        },
        FxLine {
            amount: huge.clone(),
            side: Side::Debit,
        },
        FxLine {
            amount: huge,
            side: Side::Credit,
        },
    ];
    assert_eq!(
        translate_entry(&overflowing, Decimal::ONE, spec("USD", 0), 2),
        Err(FxTranslateError::Exact(ExactError::Money(
            MoneyError::AmountOutOfRange
        )))
    );
}
#[test]
fn anchor_is_narrowed_only_after_residual_adjustment() {
    let lines = [
        FxLine {
            amount: money("9999999999999999999999999999", "EUR", 0),
            side: Side::Debit,
        },
        FxLine {
            amount: money("1", "EUR", 0),
            side: Side::Credit,
        },
    ];
    // The initially translated anchor is above 10^28, but the final anchor is 2.
    assert_eq!(
        translate_entry(&lines, decimal("2"), spec("USD", 0), 0).unwrap(),
        vec![money("2", "USD", 0), money("2", "USD", 0)]
    );
}
