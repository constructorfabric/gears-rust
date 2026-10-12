//! Regression tests for immutable stored money metadata and canonical text.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::{decode_currency, decode_money, decode_optional_money, decode_rate, encode_amount};
use crate::domain::model::RepoError;

#[test]
fn historical_scale_and_large_amount_round_trip_without_registry() {
    let text = "99999999999999999999999999.99";
    let money = decode_money(text, "EUR", 2).unwrap();
    assert_eq!(encode_amount(&money), text);
    assert_eq!(money.currency().scale(), 2);
    let historical = decode_money("1.001", "EUR", 3).unwrap();
    assert_eq!(historical.currency().scale(), 3);
}

#[test]
fn rejects_noncanonical_and_invalid_stored_amounts() {
    for text in [
        "1.00",
        "01",
        "-0",
        "1e2",
        "1.001",
        "10000000000000000000000000000",
    ] {
        assert!(
            matches!(
                decode_money(text, "EUR", 2),
                Err(RepoError::InvalidStoredMoney(_))
            ),
            "{text}"
        );
    }
}

#[test]
fn validates_every_stored_scale_and_currency() {
    for scale in [0, 28] {
        assert!(decode_currency("TOKEN9", scale).is_ok());
    }
    for scale in [-1, 29, 256] {
        assert!(matches!(
            decode_currency("EUR", scale),
            Err(RepoError::InvalidStoredMoney(_))
        ));
    }
    for code in ["", "eur", "TOO_LONG_CURRENCY1", "EUR "] {
        assert!(matches!(
            decode_currency(code, 2),
            Err(RepoError::InvalidStoredMoney(_))
        ));
    }
}

#[test]
fn optional_money_requires_complete_triple() {
    for mask in 0..8 {
        let result = decode_optional_money(
            (mask & 1 != 0).then_some("0"),
            (mask & 2 != 0).then_some("JPY"),
            (mask & 4 != 0).then_some(0),
        );
        match mask {
            0 => assert_eq!(result.unwrap(), None),
            7 => assert!(result.unwrap().is_some()),
            _ => assert!(matches!(result, Err(RepoError::InvalidStoredMoney(_)))),
        }
    }
}

#[test]
fn rates_require_positive_canonical_exact_text() {
    let rate = "0.0000000000000000000000000001";
    assert_eq!(decode_rate(rate).unwrap().to_string(), rate);
    for rate in ["0", "-1", "1.00", "1e-3"] {
        assert!(matches!(
            decode_rate(rate),
            Err(RepoError::InvalidStoredMoney(_))
        ));
    }
}
