//! The grant cap's absent-pool, metadata and boundary branches.
#![allow(clippy::unwrap_used)]

use bss_ledger_sdk::{CurrencySpec, PostedMoney, parse_decimal};
use uuid::Uuid;

use super::check_grant_cap;
use crate::domain::error::DomainError;

fn money(text: &str, code: &str, scale: u8) -> PostedMoney {
    PostedMoney::try_new(
        parse_decimal(text).unwrap(),
        CurrencySpec::try_new(code.into(), scale).unwrap(),
    )
    .unwrap()
}

#[test]
fn an_unfunded_payer_cannot_be_granted_credit() {
    assert!(matches!(
        check_grant_cap(&money("0.01", "EUR", 2), None, Uuid::now_v7()),
        Err(DomainError::GrantExceedsUnallocated(_))
    ));
}

#[test]
fn a_pool_stored_at_another_scale_or_currency_is_named() {
    let payer = Uuid::now_v7();
    assert!(matches!(
        check_grant_cap(&money("1", "EUR", 2), Some(money("5", "EUR", 3)), payer),
        Err(DomainError::InconsistentScale(_))
    ));
    assert!(matches!(
        check_grant_cap(&money("1", "EUR", 2), Some(money("5", "USD", 2)), payer),
        Err(DomainError::CurrencyMismatch(_))
    ));
}

#[test]
fn the_whole_pool_can_be_granted_and_one_increment_more_cannot() {
    let payer = Uuid::now_v7();
    let pool = || Some(money("5", "EUR", 2));
    assert!(check_grant_cap(&money("5", "EUR", 2), pool(), payer).is_ok());
    assert!(matches!(
        check_grant_cap(&money("5.01", "EUR", 2), pool(), payer),
        Err(DomainError::GrantExceedsUnallocated(_))
    ));
}
