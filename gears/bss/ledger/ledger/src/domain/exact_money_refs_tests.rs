//! Spec-level comparison and the by-reference exact sum.
#![allow(clippy::unwrap_used)]

use super::*;
use crate::domain::error::DomainError;

fn money(text: &str, code: &str, scale: u8) -> PostedMoney {
    PostedMoney::try_new(
        bss_ledger_sdk::parse_decimal(text).unwrap(),
        CurrencySpec::try_new(code.to_owned(), scale).unwrap(),
    )
    .unwrap()
}

fn spec(code: &str, scale: u8) -> CurrencySpec {
    CurrencySpec::try_new(code.to_owned(), scale).unwrap()
}

#[test]
fn matching_currency_names_code_and_scale_mismatches() {
    assert!(matching_currency(&spec("USD", 2), &spec("USD", 2)).is_ok());
    assert!(matches!(
        matching_currency(&spec("USD", 2), &spec("EUR", 2)),
        Err(DomainError::CurrencyMismatch(_))
    ));
    assert!(matches!(
        matching_currency(&spec("USD", 2), &spec("USD", 3)),
        Err(DomainError::InconsistentScale(_))
    ));
}

#[test]
fn sum_posted_is_exact_and_checks_every_term() {
    let values = [
        money("0.1", "USD", 2),
        money("0.2", "USD", 2),
        money("-0.05", "USD", 2),
    ];
    let total = sum_posted(values.iter(), &spec("USD", 2)).unwrap();
    assert_eq!(total, money("0.25", "USD", 2));
    // Agrees with the owning form.
    assert_eq!(total, sum_posted(&values, spec("USD", 2)).unwrap());
    // Empty is the explicit-spec zero.
    assert_eq!(
        sum_posted(std::iter::empty(), &spec("JPY", 0)).unwrap(),
        money("0", "JPY", 0)
    );
    let mixed = [money("1", "USD", 2), money("1", "USD", 3)];
    assert_eq!(
        sum_posted(mixed.iter(), &spec("USD", 2)),
        Err(ExactError::Money(MoneyError::ScaleMismatch))
    );
    let other = [money("1", "EUR", 2)];
    assert_eq!(
        sum_posted(other.iter(), &spec("USD", 2)),
        Err(ExactError::Money(MoneyError::CurrencyMismatch))
    );
}
