#![allow(clippy::expect_used, clippy::unwrap_used)]

use rust_decimal::Decimal;

use super::{ArithmeticBudget, CorePriceModel, ModelKind, Placement, price, price_slice};
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

fn tiers() -> Vec<CoreTier> {
    vec![
        CoreTier {
            up_to: Some(decimal("10")),
            rate: decimal("0.02"),
        },
        CoreTier {
            up_to: None,
            rate: decimal("0.015"),
        },
    ]
}

fn amount(model: &CorePriceModel, q: &str) -> (String, String) {
    let wire = price(model, &eur(), &quantity(q), ArithmeticBudget::GEN_1)
        .unwrap()
        .amount
        .to_wire("t")
        .unwrap();
    (wire.numerator, wire.denominator)
}

fn pair(numerator: &str, denominator: &str) -> (String, String) {
    (numerator.to_owned(), denominator.to_owned())
}

#[test]
fn f24_volume_and_graduated_bands_of_one_hour() {
    let volume = CorePriceModel::Volume { tiers: tiers() };
    let graduated = CorePriceModel::Graduated { tiers: tiers() };
    // q = 10 is in the upper band (bands are half-open, V42).
    assert_eq!(amount(&volume, "10"), pair("3", "20"));
    assert_eq!(amount(&volume, "12"), pair("9", "50"));
    assert_eq!(amount(&graduated, "10"), pair("1", "5"));
    assert_eq!(amount(&graduated, "12"), pair("23", "100"));
    assert_eq!(amount(&graduated, "8"), pair("4", "25"));
}

#[test]
fn v42_package_blocks_round_the_quantity_up_and_never_the_money() {
    let package = CorePriceModel::Package {
        package_size: decimal("10"),
        package_price: decimal("1.00"),
    };
    let priced = |q: &str| price(&package, &eur(), &quantity(q), ArithmeticBudget::GEN_1).unwrap();
    assert_eq!(priced("10").billable_quantity, Some(quantity("1")));
    assert_eq!(priced("10.0000001").billable_quantity, Some(quantity("2")));
    assert_eq!(amount(&package, "10.0000001"), pair("2", "1"));
    // Feature 02 §6: 3.0000000000000000000000000001 units of 3 are 2 blocks.
    let of_three = CorePriceModel::Package {
        package_size: decimal("3"),
        package_price: decimal("1"),
    };
    assert_eq!(
        amount(&of_three, "3.0000000000000000000000000001"),
        pair("2", "1")
    );
}

#[test]
fn a_slice_is_placed_after_the_earlier_slices_on_the_band_axis() {
    let worked = vec![
        CoreTier {
            up_to: Some(decimal("100")),
            rate: decimal("0.90"),
        },
        CoreTier {
            up_to: None,
            rate: decimal("0.70"),
        },
    ];
    let placed = |model: &CorePriceModel, offset: &str, q: &str, total: &str| {
        price_slice(
            model,
            &eur(),
            Placement {
                quantity: &quantity(q),
                offset: &quantity(offset),
                selecting_total: &quantity(total),
            },
            ArithmeticBudget::GEN_1,
        )
        .unwrap()
    };
    // DESIGN 03 §4.3 slice B: 20 × 0.90 + 30 × 0.70 = 39.
    let graduated = placed(
        &CorePriceModel::Graduated {
            tiers: worked.clone(),
        },
        "80",
        "50",
        "130",
    );
    assert_eq!(graduated.amount.to_wire("t").unwrap().numerator, "39");
    let segments: Vec<(u32, &str)> = graduated
        .placement
        .as_ref()
        .unwrap()
        .segments
        .iter()
        .map(|segment| (segment.band, segment.quantity.as_str()))
        .collect();
    assert_eq!(segments, [(0, "20"), (1, "30")]);
    // Volume prices the slice at the band of the total.
    let volume = placed(&CorePriceModel::Volume { tiers: worked }, "80", "50", "130");
    assert_eq!(volume.amount.to_wire("t").unwrap().numerator, "35");
    // The straddling block belongs to the slice that opened it: 8 + 5 opens one more block.
    let package = CorePriceModel::Package {
        package_size: decimal("10"),
        package_price: decimal("1"),
    };
    assert_eq!(
        placed(&package, "8", "5", "13").billable_quantity,
        Some(quantity("1"))
    );
    assert_eq!(
        placed(&package, "10", "5", "15").billable_quantity,
        Some(quantity("1"))
    );
    assert_eq!(
        placed(&package, "5", "3", "8").billable_quantity,
        Some(quantity("0"))
    );
}

#[test]
fn a_price_pricing_would_refuse_fails_closed() {
    let open_band = CoreTier {
        up_to: None,
        rate: decimal("1"),
    };
    let closed = |up_to: &str| CoreTier {
        up_to: Some(decimal(up_to)),
        rate: decimal("1"),
    };
    for tiers in [
        vec![],
        vec![closed("10")],
        vec![open_band.clone(), closed("10")],
        vec![closed("10"), closed("10"), open_band.clone()],
        vec![closed("0"), open_band],
    ] {
        let model = CorePriceModel::Graduated { tiers };
        let error = price(&model, &eur(), &quantity("1"), ArithmeticBudget::GEN_1).unwrap_err();
        assert_eq!(error.reason_code(), "unsupported_primitive", "{model:?}");
    }
    let empty_package = CorePriceModel::Package {
        package_size: Decimal::ZERO,
        package_price: decimal("1"),
    };
    let error = price(
        &empty_package,
        &eur(),
        &quantity("1"),
        ArithmeticBudget::GEN_1,
    )
    .unwrap_err();
    assert_eq!(error.reason_code(), "unsupported_primitive");
}
