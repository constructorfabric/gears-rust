//! The one policy for amounts in the parked `v1` payloads.
#![allow(clippy::unwrap_used)]

use super::v1_minor_units_for;
use bss_ledger_sdk::{CurrencySpec, PostedMoney, parse_decimal};

fn money(text: &str, code: &str, scale: u8) -> PostedMoney {
    PostedMoney::try_new(
        parse_decimal(text).unwrap(),
        CurrencySpec::try_new(code.to_owned(), scale).unwrap(),
    )
    .unwrap()
}

#[test]
fn fitting_amounts_convert_exactly() {
    assert_eq!(
        v1_minor_units_for(&money("12.34", "EUR", 2), "t", &"entity"),
        1234
    );
    assert_eq!(
        v1_minor_units_for(&money("-0.05", "EUR", 2), "t", &"entity"),
        -5
    );
    assert_eq!(
        v1_minor_units_for(&money("1500", "JPY", 0), "t", &"entity"),
        1500
    );
    assert_eq!(v1_minor_units_for(&money("0", "EUR", 2), "t", &"entity"), 0);
    // 9.223372036854775807 at scale 18 is exactly i64::MAX minor units.
    assert_eq!(
        v1_minor_units_for(&money("9.223372036854775807", "ETH", 18), "t", &"entity"),
        i64::MAX
    );
}

#[test]
fn amounts_beyond_i64_saturate_by_sign_instead_of_failing() {
    // Ten units at scale 18 is 10^19 minor units, above i64::MAX.
    assert_eq!(
        v1_minor_units_for(&money("10", "ETH", 18), "t", &"entity"),
        i64::MAX
    );
    assert_eq!(
        v1_minor_units_for(&money("-10", "ETH", 18), "t", &"entity"),
        i64::MIN
    );
    // The largest posted coefficient at scale 28.
    assert_eq!(
        v1_minor_units_for(
            &money("999999999999999999999999.9999", "XTNY", 28),
            "t",
            &"entity"
        ),
        i64::MAX
    );
}

#[test]
fn the_identity_variant_applies_the_same_policy() {
    let entity = "credit_note 7 tenant 9";
    assert_eq!(
        v1_minor_units_for(&money("12.34", "EUR", 2), "amount_minor", &entity),
        1234
    );
    assert_eq!(
        v1_minor_units_for(&money("10", "ETH", 18), "amount_minor", &entity),
        i64::MAX
    );
    assert_eq!(
        v1_minor_units_for(&money("-10", "ETH", 18), "amount_minor", &entity),
        i64::MIN
    );
}
