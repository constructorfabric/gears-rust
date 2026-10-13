//! The dual-control comparand of a schedule change: the unreleased remainder
//! `total_deferred − recognized`, never the reverse or the total.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::schedule_change_governed_amount;
use crate::domain::error::DomainError;
use bss_ledger_sdk::{CurrencySpec, PostedMoney, RecognitionScheduleView};

fn money(text: &str, scale: u8) -> PostedMoney {
    PostedMoney::try_new(
        bss_ledger_sdk::parse_decimal(text).unwrap(),
        CurrencySpec::try_new("EUR".to_owned(), scale).unwrap(),
    )
    .unwrap()
}

fn view(total_deferred: PostedMoney, recognized: PostedMoney) -> RecognitionScheduleView {
    RecognitionScheduleView {
        schedule_id: "sch-1".to_owned(),
        status: "ACTIVE".to_owned(),
        version: 0,
        revenue_stream: "SAAS".to_owned(),
        total_deferred,
        recognized,
        source_invoice_id: "INV-1".to_owned(),
        source_invoice_item_ref: "item-1".to_owned(),
        po_allocation_group: None,
        subscription_ref: None,
        policy_ref: "straight-line".to_owned(),
        segments: Vec::new(),
    }
}

#[test]
fn the_comparand_is_the_unreleased_remainder() {
    assert_eq!(
        schedule_change_governed_amount(&view(money("1200", 2), money("450.5", 2))).unwrap(),
        money("749.5", 2)
    );
    assert_eq!(
        schedule_change_governed_amount(&view(money("1200", 2), money("1200", 2))).unwrap(),
        money("0", 2)
    );
}

#[test]
fn stored_totals_at_different_scales_are_a_named_error() {
    assert!(matches!(
        schedule_change_governed_amount(&view(money("1200", 2), money("450.5", 3))),
        Err(DomainError::InconsistentScale(_))
    ));
}
