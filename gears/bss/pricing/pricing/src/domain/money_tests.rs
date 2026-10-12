#![allow(clippy::expect_used, clippy::unwrap_used)]
use super::*;
use crate::domain::{price_book_entry::Model, test_support::dec};
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
        dec("16.5")
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
        dec("20")
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
        dec("15")
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
        dec("75")
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
        dec("18")
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
        dec("49.95")
    );
    assert_eq!(
        amount_for(Model::Volume, &p, dec("1000")).unwrap(),
        dec("30")
    );
}
#[test]
fn matrix_representation_json_rejects_unknown_and_wrong_shapes() {
    assert!(serde_json::from_str::<PriceData>(r#"{"rate":"1","variant":"x"}"#).is_err());
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
fn arithmetic_overflow_is_typed() {
    assert_eq!(
        amount_for(
            Model::PerUnit,
            &PriceData::PerUnit { rate: Decimal::MAX },
            dec("2")
        )
        .unwrap_err()
        .code,
        "AMOUNT_INVALID"
    );
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
            decode(model, price.clone()).unwrap_err().code,
            "AMOUNT_INVALID",
            "{price}"
        );
    }
    assert_eq!(
        decode(
            Model::PerUnit,
            serde_json::json!({"rate":"0.12345678901234567891"})
        )
        .unwrap(),
        PriceData::PerUnit {
            rate: dec("0.12345678901234567891")
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
        dec("0"),
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
        "PACKAGE_FIELDS_INVALID"
    );
}
