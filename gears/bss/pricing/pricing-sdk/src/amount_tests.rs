//! The reference amount function (D-549): each price model, the band edges, and each refusal.
#![allow(clippy::unwrap_used)]

use bss_money::exact::ExactAmount;
use rust_decimal::Decimal;

use super::{AmountError, PriceViolation, amount_for, price_violations, tier_violations};
use crate::numeric::NumericError;
use crate::read::{PriceModel, Tier};

fn dec(text: &str) -> Decimal {
    Decimal::from_str_exact(text).unwrap()
}

/// The exact value of decimal text.
fn exact(text: &str) -> ExactAmount {
    ExactAmount::from_decimal(dec(text))
}

/// What `amount_for` returns for a quantity, which must be accepted.
fn owed(price: &PriceModel, quantity: &str) -> ExactAmount {
    amount_for(price, dec(quantity)).unwrap()
}

/// Bands from `(up_to, rate)` rows.
fn bands(rows: &[(Option<&str>, &str)]) -> Vec<Tier> {
    rows.iter()
        .map(|(up_to, rate)| Tier {
            up_to: up_to.map(dec),
            rate: dec(rate),
        })
        .collect()
}

/// `[0, 100)` at 0.12, then `[100, …)` at 0.09.
fn two_bands() -> Vec<Tier> {
    bands(&[(Some("100"), "0.12"), (None, "0.09")])
}

#[test]
fn a_flat_price_is_its_amount_at_any_quantity() {
    let price = PriceModel::Flat { amount: dec("20") };
    for quantity in ["0", "1", "7", "123456789.5"] {
        assert_eq!(owed(&price, quantity), exact("20"), "{quantity}");
    }
}

#[test]
fn a_per_unit_price_is_the_quantity_times_the_unit_amount() {
    let price = PriceModel::PerUnit {
        unit_amount: dec("0.12"),
    };
    assert_eq!(owed(&price, "150"), exact("18"));
    assert_eq!(owed(&price, "0"), exact("0"), "zero is a quantity");
}

#[test]
fn a_per_unit_product_keeps_every_place_and_every_digit() {
    // 12 places of rate times 28 places of quantity: 40 places, none rounded.
    let tiny = PriceModel::PerUnit {
        unit_amount: dec("0.000000000001"),
    };
    assert_eq!(
        owed(&tiny, "0.0000000000000000000000000001"),
        ExactAmount::from_coefficient(1, 40)
    );
    // The largest rate the limits take; ten of it does not fit a `Decimal`.
    let large = PriceModel::PerUnit {
        unit_amount: dec("9999999999999999999999999999"),
    };
    assert_eq!(
        owed(&large, "10").canonical_at_scale(0).unwrap(),
        "99999999999999999999999999990"
    );
}

#[test]
fn a_package_price_counts_whole_packages() {
    let price = PriceModel::Package {
        package_size: dec("10"),
        package_price: dec("5"),
    };
    assert_eq!(owed(&price, "0"), exact("0"));
    assert_eq!(owed(&price, "0.0000000000000000000000000001"), exact("5"));
    assert_eq!(owed(&price, "20"), exact("10"), "at a boundary");
    assert_eq!(owed(&price, "20.1"), exact("15"), "just above it");
    assert_eq!(owed(&price, "25"), exact("15"));
    // 10 / 3 never ends; it is counted exactly as four packages.
    let thirds = PriceModel::Package {
        package_size: dec("3"),
        package_price: dec("2"),
    };
    assert_eq!(owed(&thirds, "10"), exact("8"));
    assert_eq!(owed(&thirds, "9"), exact("6"));
}

#[test]
fn a_volume_price_changes_band_exactly_at_the_bound() {
    let price = PriceModel::Volume {
        tiers: bands(&[(Some("1000"), "0.1"), (None, "0.08")]),
    };
    assert_eq!(owed(&price, "999"), exact("99.9"));
    assert_eq!(
        owed(&price, "1000"),
        exact("80"),
        "the bound opens the next band"
    );
    assert_eq!(owed(&price, "0"), exact("0"));
    assert_eq!(owed(&price, "5000"), exact("400"), "the open top band");
}

#[test]
fn a_graduated_price_prices_each_band_on_its_own_part() {
    let price = PriceModel::Graduated { tiers: two_bands() };
    assert_eq!(owed(&price, "150"), exact("16.5"));
    assert_eq!(
        owed(&price, "100"),
        exact("12"),
        "the bound ends the first band"
    );
    assert_eq!(owed(&price, "50"), exact("6"));
    assert_eq!(owed(&price, "0"), exact("0"));
    let three = PriceModel::Graduated {
        tiers: bands(&[(Some("10"), "1"), (Some("20"), "0.5"), (None, "0.25")]),
    };
    assert_eq!(owed(&three, "25"), exact("16.25"));
}

#[test]
fn a_negative_quantity_is_refused_after_the_price() {
    let price = PriceModel::PerUnit {
        unit_amount: dec("2"),
    };
    assert_eq!(
        amount_for(&price, Decimal::NEGATIVE_ONE),
        Err(AmountError::NegativeQuantity)
    );
    // The price is judged before the quantity.
    let broken = PriceModel::PerUnit {
        unit_amount: Decimal::NEGATIVE_ONE,
    };
    assert_eq!(
        amount_for(&broken, Decimal::NEGATIVE_ONE),
        Err(AmountError::Price(PriceViolation::Numeric(
            NumericError::Negative
        )))
    );
}

#[test]
fn each_model_refuses_a_decimal_off_its_limit() {
    let refused = |price: PriceModel, expected: NumericError| {
        assert_eq!(
            amount_for(&price, Decimal::ONE),
            Err(AmountError::Price(PriceViolation::Numeric(expected))),
            "{price:?}"
        );
    };
    refused(
        PriceModel::Flat {
            amount: Decimal::NEGATIVE_ONE,
        },
        NumericError::Negative,
    );
    refused(
        PriceModel::PerUnit {
            unit_amount: dec("0.0000000000001"),
        },
        NumericError::OutOfRange,
    );
    refused(
        PriceModel::Package {
            package_size: Decimal::ZERO,
            package_price: dec("5"),
        },
        NumericError::NotPositive,
    );
    refused(
        PriceModel::Package {
            package_size: dec("10"),
            package_price: Decimal::NEGATIVE_ONE,
        },
        NumericError::Negative,
    );
    refused(
        PriceModel::Volume {
            tiers: bands(&[(Some("-1"), "1"), (None, "1")]),
        },
        NumericError::Negative,
    );
}

#[test]
fn a_package_reports_its_size_before_its_price_and_only_one() {
    let price = PriceModel::Package {
        package_size: Decimal::ZERO,
        package_price: Decimal::NEGATIVE_ONE,
    };
    assert_eq!(
        price_violations(&price),
        vec![PriceViolation::Numeric(NumericError::NotPositive)]
    );
}

#[test]
fn tiered_prices_refuse_no_band_a_closed_top_and_bands_out_of_order() {
    for price in [
        PriceModel::Volume { tiers: vec![] },
        PriceModel::Graduated { tiers: vec![] },
    ] {
        assert_eq!(
            amount_for(&price, Decimal::ONE),
            Err(AmountError::Price(PriceViolation::NoBands)),
            "{price:?}"
        );
    }
    let closed = bands(&[(Some("100"), "1"), (Some("200"), "0.5")]);
    let falling = bands(&[(Some("100"), "1"), (Some("50"), "0.5"), (None, "0.25")]);
    let open_middle = bands(&[(None, "1"), (None, "0.5")]);
    let zero_first = bands(&[(Some("0"), "1"), (None, "0.5")]);
    for (tiers, expected) in [
        (closed, PriceViolation::TopBandClosed),
        (falling, PriceViolation::BandsOutOfOrder),
        (open_middle, PriceViolation::BandsOutOfOrder),
        (zero_first, PriceViolation::BandsOutOfOrder),
    ] {
        for price in [
            PriceModel::Volume {
                tiers: tiers.clone(),
            },
            PriceModel::Graduated {
                tiers: tiers.clone(),
            },
        ] {
            assert_eq!(
                amount_for(&price, dec("150")),
                Err(AmountError::Price(expected)),
                "{price:?}"
            );
        }
    }
}

#[test]
fn the_band_rules_report_every_violation_in_band_order() {
    let tiers = bands(&[
        (Some("100"), "-1"),
        (Some("50"), "0.5"),
        (Some("200"), "0.0000000000001"),
    ]);
    assert_eq!(
        tier_violations(&tiers),
        vec![
            PriceViolation::Numeric(NumericError::Negative),
            PriceViolation::BandsOutOfOrder,
            PriceViolation::Numeric(NumericError::OutOfRange),
            PriceViolation::TopBandClosed,
        ]
    );
    assert!(tier_violations(&two_bands()).is_empty());
    assert_eq!(tier_violations(&[]), vec![PriceViolation::NoBands]);
}
