//! `SettledAmounts`: the gross and fee of a settlement share one currency and scale.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::*;
use rust_decimal::Decimal;

fn money(amount: i64, code: &str, scale: u8) -> PostedMoney {
    PostedMoney::try_new(
        Decimal::new(amount, u32::from(scale)),
        CurrencySpec::try_new(code.to_owned(), scale).unwrap(),
    )
    .unwrap()
}

#[test]
fn a_gross_and_fee_in_one_currency_and_scale_pair_up() {
    let amounts = SettledAmounts::try_new(money(1000, "USD", 2), money(30, "USD", 2)).unwrap();
    assert_eq!(amounts.gross(), &money(1000, "USD", 2));
    assert_eq!(amounts.fee(), &money(30, "USD", 2));
    assert_eq!(amounts.currency().code(), "USD");
    assert_eq!(
        amounts.into_parts(),
        (money(1000, "USD", 2), money(30, "USD", 2))
    );
}

#[test]
fn a_fee_in_another_currency_or_scale_is_refused() {
    assert_eq!(
        SettledAmounts::try_new(money(1000, "EUR", 2), money(30, "USD", 2)),
        Err(MoneyError::CurrencyMismatch)
    );
    assert_eq!(
        SettledAmounts::try_new(money(1000, "USD", 2), money(30, "USD", 3)),
        Err(MoneyError::ScaleMismatch)
    );
}
