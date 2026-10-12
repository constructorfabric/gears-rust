//! The stored-amount reader agrees with the authoring door on the 28-digit
//! bound: a value the door accepts reads back, anything else is a corrupt row.
#![allow(clippy::unwrap_used)]

use rust_decimal::Decimal;
use uuid::Uuid;

use super::amount;

#[test]
fn a_28_digit_stored_amount_reads_back_exactly() {
    let price = Uuid::now_v7();
    for (stored, expected) in [
        (
            "9999999999999999999999999999",
            "9999999999999999999999999999",
        ),
        (
            "0.0000000000000000000000000001",
            "0.0000000000000000000000000001",
        ),
        ("12.50", "12.5"),
        ("0", "0"),
    ] {
        assert_eq!(
            amount(price, stored).unwrap(),
            Decimal::from_str_exact(expected).unwrap(),
            "{stored}"
        );
    }
}

#[test]
fn an_out_of_contract_or_negative_stored_amount_is_corrupt() {
    let price = Uuid::now_v7();
    for stored in [
        "12345678901234567890123456789",
        "-1",
        "1e2",
        "01",
        "",
        "abc",
    ] {
        assert!(amount(price, stored).is_err(), "{stored:?} read as valid");
    }
}
