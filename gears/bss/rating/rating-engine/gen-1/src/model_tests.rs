#![allow(clippy::expect_used, clippy::unwrap_used)]

use rust_decimal::Decimal;

use super::*;
use crate::input::CoreTier;
use crate::test_input::{eur, quantity};

fn decimal(text: &str) -> Decimal {
    text.parse().unwrap()
}

#[test]
fn flat_prices_the_amount_and_no_quantity() {
    let priced = price(
        &CorePriceModel::Flat {
            amount: decimal("30"),
        },
        &eur(),
        &quantity("7"),
        ArithmeticBudget::GEN_1,
    )
    .unwrap();
    assert_eq!(priced.kind, ModelKind::Flat);
    assert_eq!(priced.quantity, None);
    assert_eq!(priced.amount.to_wire("t").unwrap().numerator, "30");
}

#[test]
fn per_unit_prices_rate_times_quantity() {
    let priced = price(
        &CorePriceModel::PerUnit {
            unit_amount: decimal("0.047"),
        },
        &eur(),
        &quantity("10"),
        ArithmeticBudget::GEN_1,
    )
    .unwrap();
    assert_eq!(priced.kind, ModelKind::PerUnit);
    assert_eq!(priced.quantity, Some(quantity("10")));
    let wire = priced.amount.to_wire("t").unwrap();
    assert_eq!(
        (wire.numerator.as_str(), wire.denominator.as_str()),
        ("47", "100")
    );
}

#[test]
fn v39_the_model_function_takes_a_quantity_evaluate_never_receives() {
    let priced = price(
        &CorePriceModel::PerUnit {
            unit_amount: decimal("0.0000000000000000000000000001"),
        },
        &eur(),
        &quantity("0.0000000000000000000000000001"),
        ArithmeticBudget::GEN_1,
    )
    .unwrap();
    let wire = priced.amount.to_wire("t").unwrap();
    assert_eq!(wire.numerator, "1");
    assert_eq!(wire.denominator, format!("1{}", "0".repeat(56)));
}

#[test]
fn tiered_and_package_models_fail_closed_until_they_are_priced() {
    let tiers = vec![
        CoreTier {
            up_to: Some(decimal("10")),
            rate: decimal("0.02"),
        },
        CoreTier {
            up_to: None,
            rate: decimal("0.015"),
        },
    ];
    for model in [
        CorePriceModel::Volume {
            tiers: tiers.clone(),
        },
        CorePriceModel::Graduated { tiers },
        CorePriceModel::Package {
            package_size: decimal("3"),
            package_price: decimal("1"),
        },
    ] {
        let error = price(&model, &eur(), &quantity("12"), ArithmeticBudget::GEN_1).unwrap_err();
        assert_eq!(error.reason_code(), "unsupported_primitive", "{model:?}");
    }
}
