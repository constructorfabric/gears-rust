//! Queued allocation payload decoding and the settlement currency/scale gate.
#![allow(clippy::unwrap_used)]

use bss_ledger_sdk::{CurrencySpec, PostedMoney, parse_decimal};
use uuid::Uuid;

use super::{AllocateRequest, QueuedAllocationPayload, QueuedSplit, settlement_spec_check};
use crate::domain::error::DomainError;
use crate::infra::storage::money_text::StoredMoney;

fn stored(amount: &str, currency: &str, scale: u8) -> StoredMoney {
    StoredMoney {
        amount: amount.to_owned(),
        currency: currency.to_owned(),
        currency_scale: scale,
    }
}

fn money(amount: &str, currency: &str, scale: u8) -> PostedMoney {
    PostedMoney::try_new(
        parse_decimal(amount).unwrap(),
        CurrencySpec::try_new(currency.to_owned(), scale).unwrap(),
    )
    .unwrap()
}

fn payload(lump: StoredMoney, splits: Option<Vec<QueuedSplit>>) -> QueuedAllocationPayload {
    QueuedAllocationPayload {
        tenant_id: Uuid::now_v7(),
        payer_tenant_id: Uuid::now_v7(),
        payment_id: "pay-1".to_owned(),
        allocation_id: Uuid::now_v7(),
        lump,
        hint_invoice_id: None,
        caller_splits: splits,
    }
}

#[test]
fn a_valid_payload_round_trips_lump_and_splits() {
    let request = AllocateRequest::from_payload(
        payload(
            stored("12.34", "EUR", 2),
            Some(vec![QueuedSplit {
                invoice_id: "inv-1".to_owned(),
                amount: stored("12.34", "EUR", 2),
            }]),
        ),
        "alloc-1",
    )
    .unwrap();
    assert_eq!(request.lump, money("12.34", "EUR", 2));
    let splits = request.caller_splits.unwrap();
    assert_eq!(splits[0].invoice_id, "inv-1");
    assert_eq!(splits[0].amount, money("12.34", "EUR", 2));
}

#[test]
fn corrupt_queued_money_is_internal_and_names_the_row_and_field() {
    let cases = [
        (payload(stored("0.047", "EUR", 2), None), "lump"),
        (payload(stored("1e2", "EUR", 2), None), "lump"),
        (
            payload(
                stored("1", "EUR", 2),
                Some(vec![QueuedSplit {
                    invoice_id: "inv-9".to_owned(),
                    amount: stored("1", "eur", 2),
                }]),
            ),
            "split for invoice inv-9",
        ),
    ];
    for (payload, field) in cases {
        let error = AllocateRequest::from_payload(payload, "alloc-7")
            .err()
            .unwrap();
        let DomainError::Internal(detail) = error else {
            panic!("expected Internal, got {error:?}");
        };
        assert!(detail.contains("alloc-7"), "{detail}");
        assert!(detail.contains(field), "{detail}");
    }
}

#[test]
fn settlement_gate_rejects_another_currency_or_scale_as_allocation_mismatch() {
    let request = |lump: PostedMoney| AllocateRequest {
        tenant_id: Uuid::now_v7(),
        payer_tenant_id: Uuid::now_v7(),
        payment_id: "pay-1".to_owned(),
        allocation_id: Uuid::now_v7(),
        lump,
        hint_invoice_id: None,
        caller_splits: None,
    };
    let settled = money("100", "EUR", 2);
    assert!(settlement_spec_check(&request(money("10", "EUR", 2)), &settled).is_ok());
    for lump in [money("10", "USD", 2), money("10", "EUR", 3)] {
        let error = settlement_spec_check(&request(lump), &settled).unwrap_err();
        assert!(
            matches!(&error, DomainError::AllocationCurrencyMismatch(d) if d.contains("pay-1")),
            "{error:?}"
        );
    }
}
