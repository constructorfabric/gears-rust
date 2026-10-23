#![allow(clippy::expect_used, clippy::unwrap_used)]

use rust_decimal::Decimal;

use super::*;
use crate::input::{AdjustmentKind, AdjustmentRef, CorePriceModel};
use crate::test_input::{eur, only_binding, period_line};

fn flat() -> CorePriceModel {
    CorePriceModel::Flat {
        amount: Decimal::ONE,
    }
}

#[test]
fn every_named_adjustment_fails_and_nothing_is_applied() {
    for kind in [
        AdjustmentKind::ContractOverride,
        AdjustmentKind::CommitmentPool,
        AdjustmentKind::Reservation,
        AdjustmentKind::Coupon,
        AdjustmentKind::Promotion,
    ] {
        let mut input = period_line(flat(), "1");
        input.fact.adjustments.push(AdjustmentRef {
            kind,
            reference: "WELCOME10".into(),
        });
        let binding = only_binding(&mut input).clone();
        let error = deferred_adjustments(&input.fact, &binding).unwrap_err();
        assert_eq!(error.reason_code(), "adjustment_unsupported", "{kind:?}");
    }
}

#[test]
fn another_billing_currency_is_not_converted() {
    let mut input = period_line(flat(), "1");
    input.fact.currency = "USD".into();
    let binding = only_binding(&mut input).clone();
    let error = deferred_adjustments(&input.fact, &binding).unwrap_err();
    assert_eq!(error.reason_code(), "fx_not_supported");
}

#[test]
fn a_negative_amount_is_clamped_with_the_pre_clamp_amount_kept() {
    let mut steps = Vec::new();
    let negative = ExactMoney::from_price("-0.5".parse().unwrap(), &eur());
    let guarded = amount(negative, &mut steps).unwrap();
    assert_eq!(guarded.wire.numerator, "0");
    let clamped = guarded.clamped_from.unwrap();
    assert_eq!(
        (clamped.numerator.as_str(), clamped.denominator.as_str()),
        ("-1", "2")
    );
    assert_eq!(
        steps.iter().map(|record| record.step).collect::<Vec<_>>(),
        vec![Step::NonNegative, Step::Precision]
    );
}

#[test]
fn a_non_negative_amount_passes_unchanged() {
    let mut steps = Vec::new();
    let guarded = amount(
        ExactMoney::from_price("0.25".parse().unwrap(), &eur()),
        &mut steps,
    )
    .unwrap();
    assert_eq!(guarded.clamped_from, None);
    assert_eq!(guarded.wire.denominator, "4");
}
