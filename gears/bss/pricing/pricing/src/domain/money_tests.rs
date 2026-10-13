#![allow(clippy::expect_used, clippy::unwrap_used)]
use super::*;
use crate::domain::{price_book_entry::Model, test_support::dec};
use bss_money::exact::{ExactAmount, ExactError};
use bss_pricing_sdk::numeric::NumericError;
/// The exact value of decimal text.
fn exact(text: &str) -> ExactAmount {
    ExactAmount::from_decimal(dec(text))
}
fn tiers() -> Vec<Tier> {
    vec![
        Tier {
            up_to: Some(dec("100")),
            rate: dec("1"),
        },
        Tier {
            up_to: None,
            rate: dec("0.5"),
        },
    ]
}
#[test]
fn prototype_l53_graduated() {
    let p = PriceData::Tiers {
        tiers: vec![
            Tier {
                up_to: Some(dec("100")),
                rate: dec("0.12"),
            },
            Tier {
                up_to: None,
                rate: dec("0.09"),
            },
        ],
    };
    assert_eq!(
        amount_for(Model::Graduated, &p, dec("150")).unwrap(),
        exact("16.5")
    );
}
#[test]
fn prototype_l54_flat() {
    assert_eq!(
        amount_for(
            Model::Flat,
            &PriceData::Flat { amount: dec("20") },
            dec("7")
        )
        .unwrap(),
        exact("20")
    );
}
#[test]
fn prototype_l55_package_rounds_up() {
    assert_eq!(
        amount_for(
            Model::Package,
            &PriceData::Package {
                package_size: dec("10"),
                package_price: dec("5")
            },
            dec("25")
        )
        .unwrap(),
        exact("15")
    );
}
#[test]
fn prototype_l56_volume() {
    assert_eq!(
        amount_for(
            Model::Volume,
            &PriceData::Tiers { tiers: tiers() },
            dec("150")
        )
        .unwrap(),
        exact("75")
    );
}
#[test]
fn matrix_15_per_unit() {
    assert_eq!(
        amount_for(
            Model::PerUnit,
            &PriceData::PerUnit { rate: dec("0.12") },
            dec("150")
        )
        .unwrap(),
        exact("18")
    );
}
#[test]
fn prototype_l62_valid_tiers() {
    assert!(validate_tiers(&tiers()).is_empty());
}
#[test]
fn prototype_l63_closed_top() {
    let mut t = tiers();
    t[1].up_to = Some(dec("200"));
    assert_eq!(validate_tiers(&t)[0].code, "TIER_TOP_CLOSED");
}
#[test]
fn prototype_l64_tiers_order() {
    let mut t = tiers();
    t.insert(
        1,
        Tier {
            up_to: Some(dec("50")),
            rate: dec("0.5"),
        },
    );
    assert_eq!(validate_tiers(&t)[0].code, "TIER_BANDS_ORDER");
}
#[test]
fn matrix_17_empty_and_negative_tiers() {
    assert_eq!(validate_tiers(&[])[0].code, "TIER_BAND_EMPTY");
    let mut t = tiers();
    t[0].rate = dec("-1");
    assert_eq!(validate_tiers(&t)[0].code, "AMOUNT_INVALID");
}
#[test]
fn matrix_16_volume_half_open_cliff() {
    let p = PriceData::Tiers {
        tiers: vec![
            Tier {
                up_to: Some(dec("1000")),
                rate: dec("0.05"),
            },
            Tier {
                up_to: None,
                rate: dec("0.03"),
            },
        ],
    };
    assert_eq!(
        amount_for(Model::Volume, &p, dec("999")).unwrap(),
        exact("49.95")
    );
    assert_eq!(
        amount_for(Model::Volume, &p, dec("1000")).unwrap(),
        exact("30")
    );
}
#[test]
fn matrix_representation_json_rejects_unknown_and_wrong_shapes() {
    assert_eq!(
        decode(
            Model::PerUnit,
            serde_json::json!({"rate":"1","variant":"x"})
        )
        .unwrap_err(),
        DecodeError::Shape,
        "an unknown field is a shape error"
    );
    assert!(decode(Model::Flat, serde_json::json!({"rate":"1"})).is_err());
    assert_eq!(
        decode(
            Model::Package,
            serde_json::json!({"package_size":"10","package_price":"5"})
        )
        .unwrap(),
        PriceData::Package {
            package_size: dec("10"),
            package_price: dec("5")
        }
    );
}
#[test]
fn a_product_past_the_decimal_carrier_is_exact() {
    // The largest rate the limits take (28 digits); ten of it does not fit a `Decimal`, and the
    // exact product has 29 digits.
    assert_eq!(
        amount_for(
            Model::PerUnit,
            &PriceData::PerUnit {
                rate: dec("9999999999999999999999999999")
            },
            dec("10")
        )
        .unwrap()
        .canonical_at_scale(0)
        .unwrap(),
        "99999999999999999999999999990"
    );
}
#[test]
fn an_exact_step_that_fails_is_a_typed_rule_refusal() {
    assert_eq!(
        exact_step(Err(ExactError::ArithmeticLimit))
            .unwrap_err()
            .code,
        "AMOUNT_OUT_OF_RANGE"
    );
    assert_eq!(
        exact_step(Err(ExactError::DivisionByZero))
            .unwrap_err()
            .code,
        "AMOUNT_INVALID"
    );
    assert_eq!(exact_step(Ok(exact("1"))).unwrap(), exact("1"));
}
#[test]
fn money_sent_as_json_numbers_is_refused_never_rounded() {
    // Chains LOW-4: serde_json reads a number through f64; 0.12345678901234567891 would be
    // stored as "0.12345678901234568". Every decimal field is exact text.
    for (model, price) in [
        (
            Model::PerUnit,
            serde_json::json!({"rate": 0.123_456_789_012_345_67}),
        ),
        (Model::Flat, serde_json::json!({"amount": 10})),
        (
            Model::Package,
            serde_json::json!({"package_size": "10", "package_price": 5}),
        ),
        (
            Model::Graduated,
            serde_json::json!({"tiers":[{"up_to": 100, "rate":"1"},{"up_to":null,"rate":"0.5"}]}),
        ),
        (
            Model::Volume,
            serde_json::json!({"tiers":[{"up_to":null,"rate": 1}]}),
        ),
    ] {
        assert_eq!(
            decode(model, price.clone()).unwrap_err().code(),
            "AMOUNT_INVALID",
            "{price}"
        );
    }
    assert_eq!(
        decode(
            Model::PerUnit,
            serde_json::json!({"rate":"1234567890123456.123456789012"})
        )
        .unwrap(),
        PriceData::PerUnit {
            rate: dec("1234567890123456.123456789012")
        },
        "a string keeps every digit"
    );
}
/// `amount_for`'s two other refusals (PT-10): a negative quantity, and content its model does not
/// take, judged before any arithmetic.
#[test]
fn a_negative_quantity_and_a_shape_the_model_does_not_take_are_refused() {
    assert_eq!(
        amount_for(
            Model::PerUnit,
            &PriceData::PerUnit { rate: dec("2") },
            dec("-1")
        )
        .unwrap_err()
        .code,
        "AMOUNT_INVALID"
    );
    assert_eq!(
        amount_for(
            Model::PerUnit,
            &PriceData::PerUnit { rate: dec("2") },
            dec("0")
        )
        .unwrap(),
        exact("0"),
        "zero is a quantity"
    );
    assert_eq!(
        amount_for(
            Model::Flat,
            &PriceData::PerUnit { rate: dec("2") },
            dec("1")
        )
        .unwrap_err()
        .code,
        "PRICE_MISSING"
    );
    assert_eq!(
        amount_for(
            Model::Package,
            &PriceData::Package {
                package_size: dec("0"),
                package_price: dec("5")
            },
            dec("1")
        )
        .unwrap_err()
        .code,
        "AMOUNT_INVALID"
    );
}

/// A per-unit price.
fn per_unit_price(rate: &str) -> PriceData {
    PriceData::PerUnit { rate: dec(rate) }
}
/// A package price.
fn package_price(size: &str, price: &str) -> PriceData {
    PriceData::Package {
        package_size: dec(size),
        package_price: dec(price),
    }
}
/// A tiered price: each band is its upper bound (`None` for the open top) and its rate.
fn banded(rows: &[(Option<&str>, &str)]) -> PriceData {
    PriceData::Tiers {
        tiers: rows
            .iter()
            .map(|(up_to, rate)| Tier {
                up_to: up_to.map(dec),
                rate: dec(rate),
            })
            .collect(),
    }
}
/// What `amount_for` returns for a quantity, which must be accepted.
fn owed(model: Model, data: &PriceData, quantity: &str) -> ExactAmount {
    amount_for(model, data, dec(quantity)).unwrap()
}
/// The exact decimal text of an amount with at most 28 places.
fn text(amount: &ExactAmount) -> String {
    amount.canonical_at_scale(28).unwrap()
}
/// `10^-places`.
fn tenth_power(places: u8) -> ExactAmount {
    ExactAmount::from_coefficient(1, places)
}
#[test]
fn a_rate_times_a_tiny_quantity_keeps_every_place() {
    // 0.001 x 0.0000000000000000000000000001 has 31 places; a `Decimal` product keeps 28 and
    // returns zero.
    let amount = owed(
        Model::PerUnit,
        &per_unit_price("0.001"),
        "0.0000000000000000000000000001",
    );
    assert_eq!(amount, tenth_power(31));
    assert_eq!(
        amount,
        exact("1")
            .checked_div(&ExactAmount::from_coefficient(10_i128.pow(31), 0))
            .unwrap(),
        "one over ten to the 31"
    );
    assert_ne!(amount, exact("0"));
    assert_eq!(
        amount.canonical_at_scale(28),
        Err(ExactError::Money(
            bss_money::MoneyError::InvalidPostingIncrement
        )),
        "finer than a decimal can hold"
    );
}
#[test]
fn the_smallest_rate_and_quantity_multiply_to_forty_places() {
    // 10^-12 x 10^-28 = 10^-40, in every model that multiplies.
    let quantity = "0.0000000000000000000000000001";
    let rate = "0.000000000001";
    let one_band = banded(&[(None, rate)]);
    assert_eq!(
        owed(Model::PerUnit, &per_unit_price(rate), quantity),
        tenth_power(40)
    );
    assert_eq!(owed(Model::Volume, &one_band, quantity), tenth_power(40));
    assert_eq!(owed(Model::Graduated, &one_band, quantity), tenth_power(40));
    assert_eq!(
        owed(Model::PerUnit, &per_unit_price(rate), "1"),
        tenth_power(12),
        "one unit costs the rate"
    );
}
#[test]
fn the_largest_rate_and_quantity_multiply_to_just_below_ten_to_the_56() {
    // 28 nines each, the most a rate or a quantity can have: (10^28 - 1)^2 is 10^56 - 2 x 10^28
    // + 1, a 56-digit integer. Integer arithmetic gives the text, not the code under test.
    let rate = "9999999999999999999999999999";
    let quantity = "9999999999999999999999999999";
    let product = "99999999999999999999999999980000000000000000000000000001";
    let one_band = banded(&[(None, rate)]);
    // Per-unit, volume, graduated: one comparison, in this order.
    let owed_by_model: Vec<String> = [
        (Model::PerUnit, per_unit_price(rate)),
        (Model::Volume, one_band.clone()),
        (Model::Graduated, one_band),
    ]
    .iter()
    .map(|(model, data)| text(&owed(*model, data, quantity)))
    .collect();
    assert_eq!(owed_by_model, [product; 3]);
}
#[test]
fn a_rate_with_twelve_places_at_its_widest_multiplies_exactly() {
    // The widest 12-place rate has 28 digits: 16 whole and 12 places.
    let rate = "9999999999999999.999999999999";
    let quantity = "9999999999999999999999999999";
    // (10^28 - 1)^2 / 10^12: 44 whole digits and 12 places, far past a `Decimal`.
    assert_eq!(
        text(&owed(Model::PerUnit, &per_unit_price(rate), quantity)),
        "99999999999999999999999999980000000000000000.000000000001"
    );
    assert_eq!(
        text(&owed(Model::Volume, &banded(&[(None, rate)]), quantity)),
        "99999999999999999999999999980000000000000000.000000000001"
    );
    assert_eq!(
        owed(
            Model::Flat,
            &PriceData::Flat { amount: dec(rate) },
            quantity
        ),
        exact(rate),
        "a flat price ignores the quantity"
    );
    // The widest quantity with the most places: (10^28 - 1)^2 / 10^40, which has 40 places.
    // The expected value uses only shifts of ten and one subtraction, not the product under test.
    let nines = ExactAmount::from_coefficient(10_i128.pow(28) - 1, 0);
    let shifted = nines
        .checked_mul(&ExactAmount::from_coefficient(10_i128.pow(28), 0))
        .unwrap()
        .checked_sub(&nines)
        .unwrap();
    assert_eq!(
        owed(
            Model::PerUnit,
            &per_unit_price(rate),
            "0.9999999999999999999999999999"
        ),
        shifted.checked_mul(&tenth_power(40)).unwrap()
    );
}
#[test]
fn zero_quantity_costs_nothing_except_a_flat_price() {
    // Flat, per-unit, package, volume, graduated: one comparison, in this order.
    let cases = [
        (Model::Flat, PriceData::Flat { amount: dec("20") }, "20"),
        (Model::PerUnit, per_unit_price("0.12"), "0"),
        (Model::Package, package_price("10", "5"), "0"),
        (Model::Volume, PriceData::Tiers { tiers: tiers() }, "0"),
        (Model::Graduated, PriceData::Tiers { tiers: tiers() }, "0"),
    ];
    let owed_at_zero: Vec<ExactAmount> = cases
        .iter()
        .map(|(model, data, _)| owed(*model, data, "0"))
        .collect();
    let expected: Vec<ExactAmount> = cases.iter().map(|(_, _, amount)| exact(amount)).collect();
    assert_eq!(owed_at_zero, expected);
}
#[test]
fn a_package_is_counted_whole_below_at_and_above_each_boundary() {
    let data = package_price("10", "5");
    let cases = [
        ("0", "0"),
        ("0.0000000000000000000000000001", "5"),
        ("1", "5"),
        ("9.999999999999999999999999999", "5"),
        ("10", "5"),
        ("10.00000000000000000000000001", "10"),
        ("19.99999999999999999999999999", "10"),
        ("20", "10"),
        ("20.00000000000000000000000001", "15"),
    ];
    let owed_for_each: Vec<ExactAmount> = cases
        .iter()
        .map(|(quantity, _)| owed(Model::Package, &data, quantity))
        .collect();
    let expected: Vec<ExactAmount> = cases.iter().map(|(_, amount)| exact(amount)).collect();
    assert_eq!(owed_for_each, expected);
}
#[test]
fn a_package_quotient_that_never_ends_is_counted_exactly() {
    let data = package_price("3", "1.5");
    let cases = [
        ("1", "1.5"),
        ("2.999999999999999999999999999", "1.5"),
        ("3", "1.5"),
        ("3.000000000000000000000000001", "3"),
        ("6", "3"),
        ("7", "4.5"),
    ];
    let owed_for_each: Vec<ExactAmount> = cases
        .iter()
        .map(|(quantity, _)| owed(Model::Package, &data, quantity))
        .collect();
    let expected: Vec<ExactAmount> = cases.iter().map(|(_, amount)| exact(amount)).collect();
    assert_eq!(owed_for_each, expected);
    // 3000000000000000000000000001 / 0.3 is 10^28 + 3 + 1/3. A `Decimal` quotient cannot keep the
    // third, so its count falls one package short.
    assert_eq!(
        text(&owed(
            Model::Package,
            &package_price("0.3", "1"),
            "3000000000000000000000000001"
        )),
        "10000000000000000000000000004"
    );
    // The same quotient with the smallest package sizes: 3.000000000000000000000000001 divided by
    // 0.0000000000000000000000000003.
    assert_eq!(
        text(&owed(
            Model::Package,
            &package_price("0.0000000000000000000000000003", "1"),
            "3.000000000000000000000000001"
        )),
        "10000000000000000000000000004"
    );
}
#[test]
fn the_largest_package_count_and_amount_stay_exact() {
    let quantity = "9999999999999999999999999999";
    let size = "0.0000000000000000000000000001";
    // 10^56 - 10^28 packages, each priced 1.
    assert_eq!(
        owed(Model::Package, &package_price(size, "1"), quantity)
            .canonical_at_scale(0)
            .unwrap(),
        "99999999999999999999999999990000000000000000000000000000"
    );
    // Each priced at the widest 12-place package price: 10^72 - 2 x 10^44 + 10^16, below 10^84.
    assert_eq!(
        owed(
            Model::Package,
            &package_price(size, "9999999999999999.999999999999"),
            quantity
        )
        .canonical_at_scale(12)
        .unwrap(),
        "999999999999999999999999999800000000000000000000000000010000000000000000"
    );
}
#[test]
fn the_largest_package_price_and_count_reach_just_below_ten_to_the_84() {
    // The largest count: quantity 10^28 - 1 over size 10^-28 is (10^28 - 1) x 10^28 packages.
    // Each is priced 10^28 - 1, the largest package price. The amount is
    // (10^56 - 10^28)(10^28 - 1) = 10^84 - 2 x 10^56 + 10^28, an 84-digit integer. Integer
    // arithmetic gives the text, not the code under test.
    let amount = owed(
        Model::Package,
        &package_price(
            "0.0000000000000000000000000001",
            "9999999999999999999999999999",
        ),
        "9999999999999999999999999999",
    );
    assert_eq!(
        amount.canonical_at_scale(0).unwrap(),
        "999999999999999999999999999800000000000000000000000000010000000000000000000000000000"
    );
}
#[test]
fn a_volume_price_changes_band_exactly_at_the_bound() {
    let data = banded(&[(Some("1000"), "0.05"), (None, "0.03")]);
    let cases = [
        ("0", "0"),
        (
            "999.9999999999999999999999999",
            "49.999999999999999999999999995",
        ),
        ("1000", "30"),
        (
            "1000.000000000000000000000001",
            "30.00000000000000000000000003",
        ),
    ];
    let owed_for_each: Vec<String> = cases
        .iter()
        .map(|(quantity, _)| text(&owed(Model::Volume, &data, quantity)))
        .collect();
    let expected: Vec<&str> = cases.iter().map(|(_, amount)| *amount).collect();
    assert_eq!(owed_for_each, expected);
}
#[test]
fn a_graduated_price_splits_exactly_at_the_bound() {
    let data = banded(&[(Some("100"), "0.12"), (None, "0.09")]);
    let cases = [
        ("0", "0"),
        (
            "99.99999999999999999999999999",
            "11.9999999999999999999999999988",
        ),
        ("100", "12"),
        (
            "100.000000000000000000000001",
            "12.00000000000000000000000009",
        ),
        ("150", "16.5"),
    ];
    let owed_for_each: Vec<String> = cases
        .iter()
        .map(|(quantity, _)| text(&owed(Model::Graduated, &data, quantity)))
        .collect();
    let expected: Vec<&str> = cases.iter().map(|(_, amount)| *amount).collect();
    assert_eq!(owed_for_each, expected);
}
#[test]
fn a_graduated_band_width_is_not_rounded() {
    // The width of the open band is 99999999999.99999999999999999 - 0.0000000000000000000000000001,
    // which has 39 digits: a `Decimal` difference rounds it.
    let data = banded(&[(Some("0.0000000000000000000000000001"), "0"), (None, "1")]);
    assert_eq!(
        text(&owed(
            Model::Graduated,
            &data,
            "99999999999.99999999999999999"
        )),
        "99999999999.9999999999999999899999999999"
    );
}
#[test]
fn graduated_bands_at_the_largest_bounds_add_exactly() {
    let data = banded(&[
        (Some("9999999999999999999999999998"), "0.999999999999"),
        (None, "0.000000000001"),
    ]);
    // (10^28 - 2) x 0.999999999999 + 1 x 10^-12
    assert_eq!(
        text(&owed(
            Model::Graduated,
            &data,
            "9999999999999999999999999999"
        )),
        "9999999999989999999999999998.000000000003"
    );
}
proptest::proptest! {
    /// A per-unit amount is the product of the two coefficients, at the sum of the two scales.
    #[test]
    fn a_per_unit_amount_is_the_product_of_the_coefficients(
        rate_coefficient in 0_i64..,
        rate_scale in 0_u32..=12,
        quantity_coefficient in 0_i64..,
        quantity_scale in 0_u32..=28,
    ) {
        let data = PriceData::PerUnit { rate: Decimal::new(rate_coefficient, rate_scale) };
        let quantity = Decimal::new(quantity_coefficient, quantity_scale);
        let expected = ExactAmount::from_coefficient(
            i128::from(rate_coefficient) * i128::from(quantity_coefficient),
            u8::try_from(rate_scale + quantity_scale).unwrap(),
        );
        proptest::prop_assert_eq!(amount_for(Model::PerUnit, &data, quantity).unwrap(), expected);
    }
    /// A package amount is the whole-package count, found with integer division, times the price.
    #[test]
    fn a_package_amount_is_the_ceiling_count_times_the_price(
        size_coefficient in 1_u32..,
        size_scale in 0_u32..=18,
        quantity_coefficient in 0_u32..,
        quantity_scale in 0_u32..=18,
        price_coefficient in 0_u32..,
        price_scale in 0_u32..=12,
    ) {
        let data = PriceData::Package {
            package_size: Decimal::new(i64::from(size_coefficient), size_scale),
            package_price: Decimal::new(i64::from(price_coefficient), price_scale),
        };
        let quantity = Decimal::new(i64::from(quantity_coefficient), quantity_scale);
        // quantity / size = (qc x 10^ss) / (sc x 10^qs)
        let numerator = u128::from(quantity_coefficient) * 10_u128.pow(size_scale);
        let denominator = u128::from(size_coefficient) * 10_u128.pow(quantity_scale);
        let packages = numerator.div_ceil(denominator);
        let expected = ExactAmount::from_coefficient(
            i128::try_from(packages * u128::from(price_coefficient)).unwrap(),
            u8::try_from(price_scale).unwrap(),
        );
        proptest::prop_assert_eq!(amount_for(Model::Package, &data, quantity).unwrap(), expected);
    }
}

fn per_unit(rate: &str) -> serde_json::Value {
    serde_json::json!({ "rate": rate })
}
/// A rate with `places` fractional places: `0.00…01`.
fn smallest(places: usize) -> String {
    format!("0.{}1", "0".repeat(places - 1))
}
/// The brief's decode regression: a JSON number, an exponent, a 29th place, trailing zeros and
/// a rate finer than the currency.
#[test]
fn decode_regression_for_numbers_text_limits_and_exact_rates() {
    let number = decode(Model::PerUnit, serde_json::json!({"rate": 0.047})).unwrap_err();
    assert_eq!(number, DecodeError::JsonNumber);
    assert_eq!(number.code(), "AMOUNT_INVALID");
    assert_eq!(number.message(), DECIMALS_ARE_STRINGS);
    let exponent = decode(Model::PerUnit, per_unit("1e-3")).unwrap_err();
    assert_eq!(
        exponent,
        DecodeError::Numeric {
            field: PriceField::Rate,
            error: NumericError::InvalidText
        }
    );
    assert_eq!(exponent.code(), "AMOUNT_INVALID");
    assert_eq!(exponent.message(), PLAIN_DECIMAL_TEXT);
    let scale_29 = decode(Model::PerUnit, per_unit(&smallest(29))).unwrap_err();
    assert_eq!(
        scale_29,
        DecodeError::Numeric {
            field: PriceField::Rate,
            error: NumericError::OutOfRange
        }
    );
    assert_eq!(scale_29.code(), "AMOUNT_OUT_OF_RANGE");
    let trailing = decode(Model::PerUnit, per_unit("0.10")).unwrap();
    assert_eq!(trailing, PriceData::PerUnit { rate: dec("0.1") });
    let PriceData::PerUnit { rate } = trailing else {
        panic!("a per-unit price");
    };
    assert_eq!(rate.scale(), 1, "the value is normalized");
    assert_eq!(
        decode(Model::PerUnit, per_unit("0.047")).unwrap(),
        PriceData::PerUnit { rate: dec("0.047") }
    );
}
#[test]
fn decode_refuses_text_that_is_not_a_plain_decimal() {
    for text in [
        "1e-3", " 0.1", "0.1 ", "+1", "01", ".5", "1.", "1_000", "-0", "", "0x10",
    ] {
        let refused = decode(Model::PerUnit, per_unit(text)).unwrap_err();
        assert_eq!(
            refused,
            DecodeError::Numeric {
                field: PriceField::Rate,
                error: NumericError::InvalidText
            },
            "plain decimal text only"
        );
        assert_eq!(refused.code(), "AMOUNT_INVALID");
    }
    assert_eq!(
        decode(
            Model::Graduated,
            serde_json::json!({"tiers":[{"up_to":"1e2","rate":"1"},{"up_to":null,"rate":"1"}]})
        )
        .unwrap_err(),
        DecodeError::Numeric {
            field: PriceField::UpTo,
            error: NumericError::InvalidText
        }
    );
}
#[test]
fn decode_refuses_a_value_over_its_limit_and_names_the_limit() {
    for (model, price, field) in [
        (Model::PerUnit, per_unit(&smallest(13)), PriceField::Rate),
        (
            Model::Flat,
            serde_json::json!({"amount": "12345678901234567890123456789"}),
            PriceField::Amount,
        ),
        (
            Model::Package,
            serde_json::json!({"package_size": "10", "package_price": smallest(13)}),
            PriceField::PackagePrice,
        ),
        (
            Model::Package,
            serde_json::json!({"package_size": smallest(29), "package_price": "1"}),
            PriceField::PackageSize,
        ),
        (
            Model::Volume,
            serde_json::json!({"tiers":[{"up_to": smallest(29),"rate":"1"},{"up_to":null,"rate":"1"}]}),
            PriceField::UpTo,
        ),
        (
            Model::Volume,
            serde_json::json!({"tiers":[{"up_to":null,"rate": smallest(13)}]}),
            PriceField::Rate,
        ),
    ] {
        let refused = decode(model, price).unwrap_err();
        assert_eq!(
            refused,
            DecodeError::Numeric {
                field,
                error: NumericError::OutOfRange
            },
            "over the field's limit"
        );
        assert_eq!(refused.code(), "AMOUNT_OUT_OF_RANGE");
    }
    let rate = decode(Model::PerUnit, per_unit(&smallest(13))).unwrap_err();
    assert!(
        rate.message().contains("12 fractional places"),
        "the message names the rate limit"
    );
    let threshold = decode(
        Model::Volume,
        serde_json::json!({"tiers":[{"up_to": smallest(29),"rate":"1"},{"up_to":null,"rate":"1"}]}),
    )
    .unwrap_err();
    assert!(
        threshold.message().contains("28 fractional places"),
        "the message names the threshold limit"
    );
    // The limits themselves are accepted.
    assert!(decode(Model::PerUnit, per_unit(&smallest(12))).is_ok());
    assert!(
        decode(
            Model::Package,
            serde_json::json!({"package_size": smallest(28), "package_price": "1"})
        )
        .is_ok()
    );
    assert!(
        decode(
            Model::Graduated,
            serde_json::json!({"tiers":[{"up_to": smallest(28),"rate":"1"},{"up_to":null,"rate":"1"}]})
        )
        .is_ok()
    );
}
#[test]
fn decode_names_the_field_of_a_negative_or_zero_value() {
    // The plan's error table: a negative or non-positive value is `AMOUNT_INVALID`, and the
    // message names the field, for a package field as for any other.
    for (model, price, field, error) in [
        (
            Model::PerUnit,
            per_unit("-1"),
            PriceField::Rate,
            NumericError::Negative,
        ),
        (
            Model::Flat,
            serde_json::json!({"amount": "-0.5"}),
            PriceField::Amount,
            NumericError::Negative,
        ),
        (
            Model::Graduated,
            serde_json::json!({"tiers":[{"up_to":"-5","rate":"1"},{"up_to":null,"rate":"1"}]}),
            PriceField::UpTo,
            NumericError::Negative,
        ),
        (
            Model::Package,
            serde_json::json!({"package_size": "0", "package_price": "5"}),
            PriceField::PackageSize,
            NumericError::NotPositive,
        ),
        (
            Model::Package,
            serde_json::json!({"package_size": "-10", "package_price": "5"}),
            PriceField::PackageSize,
            NumericError::Negative,
        ),
        (
            Model::Package,
            serde_json::json!({"package_size": "10", "package_price": "-5"}),
            PriceField::PackagePrice,
            NumericError::Negative,
        ),
    ] {
        let refused = decode(model, price).unwrap_err();
        assert_eq!(
            refused,
            DecodeError::Numeric { field, error },
            "the sign rule"
        );
        assert_eq!(refused.code(), "AMOUNT_INVALID");
        assert!(
            refused.message().starts_with(field.name()),
            "the message names the field"
        );
    }
    assert_eq!(
        decode(
            Model::Package,
            serde_json::json!({"package_size": "0", "package_price": "5"})
        )
        .unwrap_err()
        .message(),
        "package_size must be above zero"
    );
}
#[test]
fn decode_normalizes_every_decimal_of_every_shape() {
    // `Decimal`'s `==` ignores the scale (`10.00 == 10`), so each field is compared as text.
    let PriceData::Package {
        package_size,
        package_price,
    } = decode(
        Model::Package,
        serde_json::json!({"package_size": "10.00", "package_price": "5.50"}),
    )
    .unwrap()
    else {
        panic!("a package price");
    };
    assert_eq!(
        (package_size.to_string(), package_price.to_string()),
        ("10".to_owned(), "5.5".to_owned()),
        "both package fields are normalized"
    );
    let PriceData::Tiers { tiers } = decode(
        Model::Graduated,
        serde_json::json!({"tiers":[{"up_to":"100.0","rate":"0.120"},{"up_to":null,"rate":"0.090"}]}),
    )
    .unwrap() else {
        panic!("a tiered price");
    };
    let texts: Vec<(Option<String>, String)> = tiers
        .iter()
        .map(|t| (t.up_to.map(|v| v.to_string()), t.rate.to_string()))
        .collect();
    assert_eq!(
        texts,
        [
            (Some("100".to_owned()), "0.12".to_owned()),
            (None, "0.09".to_owned())
        ],
        "every tier field is normalized"
    );
    // A shape the model does not take stays PRICE_MISSING.
    let shape = decode(Model::Flat, per_unit("1")).unwrap_err();
    assert_eq!(shape, DecodeError::Shape);
    assert_eq!(shape.code(), "PRICE_MISSING");
    assert_eq!(
        decode(
            Model::PerUnit,
            serde_json::json!({"rate": "1", "extra": "1"})
        )
        .unwrap_err(),
        DecodeError::Shape
    );
}
#[test]
fn a_stored_decimal_must_be_its_canonical_text() {
    assert_eq!(
        decode_stored(Model::PerUnit, per_unit("0.047")).unwrap(),
        PriceData::PerUnit { rate: dec("0.047") }
    );
    assert_eq!(
        decode_stored(Model::PerUnit, per_unit("0.10")).unwrap_err(),
        DecodeError::NotCanonical {
            field: PriceField::Rate
        }
    );
    assert_eq!(
        decode_stored(
            Model::Volume,
            serde_json::json!({"tiers":[{"up_to":"100.0","rate":"1"},{"up_to":null,"rate":"1"}]})
        )
        .unwrap_err(),
        DecodeError::NotCanonical {
            field: PriceField::UpTo
        }
    );
    assert_eq!(
        decode_stored(Model::PerUnit, serde_json::json!({"rate": 0.1})).unwrap_err(),
        DecodeError::JsonNumber
    );
    assert_eq!(
        decode_stored(Model::PerUnit, per_unit("-1")).unwrap_err(),
        DecodeError::Numeric {
            field: PriceField::Rate,
            error: NumericError::Negative
        }
    );
    assert_eq!(
        decode_stored(Model::Flat, per_unit("1")).unwrap_err(),
        DecodeError::Shape
    );
}
#[test]
fn a_stored_fee_must_be_canonical_and_not_negative() {
    for (text, fee) in [
        ("30", "30"),
        ("0.1", "0.1"),
        ("0", "0"),
        ("12.5", "12.5"),
        // The 28-digit bound the authoring door takes reads back exactly.
        (
            "9999999999999999999999999999",
            "9999999999999999999999999999",
        ),
        (
            "0.0000000000000000000000000001",
            "0.0000000000000000000000000001",
        ),
    ] {
        let read = stored_fee(text).expect("canonical text reads back");
        assert_eq!(read, dec(fee), "canonical text reads back");
        assert_eq!(
            bss_money::canonical_decimal(read),
            text,
            "it reads back with its own spelling"
        );
    }
    for text in [
        "30.00",
        "0.10",
        "12.50",
        "1.0",
        "-1",
        " 3",
        "1e2",
        "01",
        "",
        "abc",
        "12345678901234567890123456789",
    ] {
        assert_eq!(stored_fee(text), None, "anything else is corrupt");
    }
}
#[test]
fn price_decimals_serialize_as_canonical_text() {
    assert_eq!(
        serde_json::to_value(PriceData::PerUnit { rate: dec("0.10") }).unwrap(),
        serde_json::json!({"rate":"0.1"})
    );
    assert_eq!(
        serde_json::to_value(PriceData::Tiers {
            tiers: vec![
                Tier {
                    up_to: Some(dec("100.00")),
                    rate: dec("1.50")
                },
                Tier {
                    up_to: None,
                    rate: dec("0")
                },
            ]
        })
        .unwrap(),
        serde_json::json!({"tiers":[{"up_to":"100","rate":"1.5"},{"up_to":null,"rate":"0"}]})
    );
    assert_eq!(
        serde_json::to_value(PriceData::Package {
            package_size: dec("10.0"),
            package_price: dec("5.000")
        })
        .unwrap(),
        serde_json::json!({"package_size":"10","package_price":"5"})
    );
}
#[test]
fn validate_applies_the_numeric_limits_to_values_already_held() {
    // One place finer than a rate takes.
    let too_fine = dec(&smallest(13));
    for (model, data, code) in [
        (
            Model::PerUnit,
            PriceData::PerUnit { rate: too_fine },
            "AMOUNT_OUT_OF_RANGE",
        ),
        (
            Model::Flat,
            PriceData::Flat {
                amount: Decimal::MAX,
            },
            "AMOUNT_OUT_OF_RANGE",
        ),
        (
            Model::Package,
            PriceData::Package {
                package_size: dec("10"),
                package_price: too_fine,
            },
            "AMOUNT_OUT_OF_RANGE",
        ),
        (
            Model::Package,
            PriceData::Package {
                package_size: dec("-1"),
                package_price: dec("1"),
            },
            "AMOUNT_INVALID",
        ),
        (
            Model::Package,
            PriceData::Package {
                package_size: dec("0"),
                package_price: dec("1"),
            },
            "AMOUNT_INVALID",
        ),
        (
            Model::Package,
            PriceData::Package {
                package_size: dec("10"),
                package_price: dec("-1"),
            },
            "AMOUNT_INVALID",
        ),
        (
            Model::Volume,
            PriceData::Tiers {
                tiers: vec![Tier {
                    up_to: None,
                    rate: too_fine,
                }],
            },
            "AMOUNT_OUT_OF_RANGE",
        ),
        (
            Model::Graduated,
            PriceData::Tiers {
                tiers: vec![
                    Tier {
                        up_to: Some(dec("-5")),
                        rate: dec("1"),
                    },
                    Tier {
                        up_to: None,
                        rate: dec("1"),
                    },
                ],
            },
            "AMOUNT_INVALID",
        ),
    ] {
        assert_eq!(validate(model, &data)[0].code, code, "the field's limit");
    }
    assert!(
        validate(
            Model::PerUnit,
            &PriceData::PerUnit {
                rate: dec(&smallest(12))
            }
        )
        .is_empty()
    );
}

/// Decimal text within `places` fractional places and the 28-digit coefficient, written with up
/// to two trailing zeros that are not places (`5.50`), so the authoring decode has to normalize it.
fn decimal_text(places: usize) -> impl proptest::strategy::Strategy<Value = String> {
    use proptest::prelude::*;
    (0..=places)
        .prop_flat_map(|fraction| (1..=28_usize.saturating_sub(fraction).max(1), Just(fraction)))
        .prop_flat_map(|(whole, fraction)| {
            (
                proptest::collection::vec(0_u8..10, whole),
                proptest::collection::vec(0_u8..10, fraction),
                0_usize..3,
            )
        })
        .prop_map(|(whole, fraction, zeros)| {
            let text =
                |digits: &[u8]| -> String { digits.iter().map(|d| char::from(b'0' + d)).collect() };
            // A 28-place fraction leaves no digit for the whole part.
            let whole = if fraction.len() >= 28 {
                "0".to_owned()
            } else {
                let whole = text(&whole);
                let trimmed = whole.trim_start_matches('0');
                if trimmed.is_empty() {
                    "0".to_owned()
                } else {
                    trimmed.to_owned()
                }
            };
            if fraction.is_empty() {
                whole
            } else {
                format!("{whole}.{}{}", text(&fraction), "0".repeat(zeros))
            }
        })
}
/// A price body of every shape, each decimal within its field's limit.
fn price_body() -> impl proptest::strategy::Strategy<Value = (Model, serde_json::Value)> {
    use proptest::prelude::*;
    let rate = || decimal_text(12);
    let positive = decimal_text(28).prop_filter("a package size is above zero", |text| {
        text.bytes().any(|b| (b'1'..=b'9').contains(&b))
    });
    let tier = (proptest::option::of(decimal_text(28)), rate())
        .prop_map(|(up_to, rate)| serde_json::json!({"up_to": up_to, "rate": rate}));
    prop_oneof![
        rate().prop_map(|amount| (Model::Flat, serde_json::json!({ "amount": amount }))),
        rate().prop_map(|rate| (Model::PerUnit, serde_json::json!({ "rate": rate }))),
        (positive, rate()).prop_map(|(size, price)| (
            Model::Package,
            serde_json::json!({"package_size": size, "package_price": price})
        )),
        (
            prop_oneof![Just(Model::Graduated), Just(Model::Volume)],
            proptest::collection::vec(tier, 1..4)
        )
            .prop_map(|(model, tiers)| (model, serde_json::json!({ "tiers": tiers }))),
    ]
}
/// Every decimal of a price, in order.
fn decimals(data: &PriceData) -> Vec<Decimal> {
    match data {
        PriceData::Flat { amount } => vec![*amount],
        PriceData::PerUnit { rate } => vec![*rate],
        PriceData::Package {
            package_size,
            package_price,
        } => vec![*package_size, *package_price],
        PriceData::Tiers { tiers } => tiers
            .iter()
            .flat_map(|t| t.up_to.into_iter().chain([t.rate]))
            .collect(),
    }
}
proptest::proptest! {
    /// Every price write rests on this: whatever the authoring decode accepts, serialized as
    /// storage writes it, the stored decode reads back as the same value and the same text.
    /// Each decoded decimal already has its canonical scale, trailing zeros or not.
    #[test]
    fn what_decode_accepts_reads_back_from_storage_unchanged((model, body) in price_body()) {
        let decoded = decode(model, body).expect("a body within its limits decodes");
        for value in decimals(&decoded) {
            proptest::prop_assert_eq!(value.to_string(), bss_money::canonical_decimal(value));
        }
        let stored = serde_json::to_value(&decoded).expect("a decoded price serializes");
        let read = decode_stored(model, stored.clone()).expect("the stored text reads back");
        proptest::prop_assert_eq!(&read, &decoded);
        proptest::prop_assert_eq!(serde_json::to_value(&read).expect("it serializes"), stored);
    }
    /// Neither decode panics on any text in any decimal field.
    #[test]
    fn neither_decode_panics_on_arbitrary_text(text in "\\PC{0,80}") {
        for (model, body) in [
            (Model::Flat, serde_json::json!({ "amount": text })),
            (Model::PerUnit, serde_json::json!({ "rate": text })),
            (
                Model::Package,
                serde_json::json!({"package_size": text, "package_price": text}),
            ),
            (
                Model::Volume,
                serde_json::json!({"tiers": [{"up_to": text, "rate": text}]}),
            ),
        ] {
            // Whatever text it accepts reads back from storage; the stored decode only answers.
            if let Ok(data) = decode(model, body.clone()) {
                let stored = serde_json::to_value(&data).expect("a decoded price serializes");
                let read = decode_stored(model, stored).expect("the stored text reads back");
                proptest::prop_assert_eq!(read, data);
            }
            match decode_stored(model, body) {
                Ok(_) | Err(_) => {}
            }
        }
    }
}
