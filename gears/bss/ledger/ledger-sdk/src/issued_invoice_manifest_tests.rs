//! `GrossTotals`: sorted by currency code, one total per currency.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::*;
use crate::CurrencySpec;
use rust_decimal::Decimal;

fn money(amount: i64, code: &str, scale: u8) -> PostedMoney {
    PostedMoney::try_new(
        Decimal::new(amount, u32::from(scale)),
        CurrencySpec::try_new(code.to_owned(), scale).unwrap(),
    )
    .unwrap()
}

#[test]
fn totals_are_sorted_by_currency_code() {
    let totals = GrossTotals::try_new(vec![
        money(1050, "USD", 2),
        money(1200, "JPY", 0),
        money(325, "EUR", 2),
    ])
    .unwrap();
    let codes: Vec<_> = totals.iter().map(|t| t.currency().code()).collect();
    assert_eq!(codes, ["EUR", "JPY", "USD"]);
    assert_eq!(totals.len(), 3);
    assert_eq!(totals.get("JPY"), Some(&money(1200, "JPY", 0)));
    assert_eq!(totals.get("GBP"), None);
    assert_eq!((&totals).into_iter().count(), 3);
}

#[test]
fn a_repeated_currency_is_rejected_at_any_scale() {
    for second in [money(2, "USD", 2), money(2, "USD", 3)] {
        assert_eq!(
            GrossTotals::try_new(vec![money(1, "USD", 2), money(5, "EUR", 2), second]),
            Err(GrossTotalsError::DuplicateCurrency("USD".to_owned()))
        );
    }
}

#[test]
fn an_empty_manifest_has_no_totals() {
    let totals = GrossTotals::try_new(Vec::new()).unwrap();
    assert!(totals.is_empty());
    assert_eq!(totals, GrossTotals::default());
    assert!(totals.as_slice().is_empty());
}
