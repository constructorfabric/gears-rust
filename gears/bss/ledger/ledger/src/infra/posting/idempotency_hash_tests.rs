//! `payload_hash` properties replays depend on: line order does not matter, and
//! the framing does not drift (a golden digest over a fixed entry).
#![allow(clippy::unwrap_used)]

use bss_ledger_sdk::{
    AccountClass, CurrencySpec, MappingStatus, PostedMoney, Side, SourceDocType, parse_decimal,
};
use chrono::NaiveDate;
use serde_json::json;
use time::OffsetDateTime;
use uuid::Uuid;

use super::IdempotencyGate;
use crate::domain::model::{NewEntry, NewLine};

fn money(text: &str, code: &str, scale: u8) -> PostedMoney {
    PostedMoney::try_new(
        parse_decimal(text).unwrap(),
        CurrencySpec::try_new(code.to_owned(), scale).unwrap(),
    )
    .unwrap()
}

fn entry() -> NewEntry {
    NewEntry {
        entry_id: Uuid::from_u128(1),
        tenant_id: Uuid::from_u128(2),
        legal_entity_id: Uuid::from_u128(3),
        period_id: "202606".to_owned(),
        entry_currency: "EUR".to_owned(),
        source_doc_type: SourceDocType::InvoicePost,
        source_business_id: "inv-golden".to_owned(),
        reverses_entry_id: None,
        reverses_period_id: None,
        posted_at_utc: OffsetDateTime::UNIX_EPOCH,
        effective_at: NaiveDate::from_ymd_opt(2026, 6, 1).unwrap(),
        origin: "SYSTEM".to_owned(),
        posted_by_actor_id: Uuid::from_u128(4),
        correlation_id: Uuid::from_u128(5),
        rounding_evidence: json!({}),
        rate_snapshot_ref: None,
    }
}

fn line(account: u128, class: AccountClass, side: Side, amount: &str) -> NewLine {
    NewLine {
        line_id: Uuid::from_u128(100 + account),
        payer_tenant_id: Uuid::from_u128(2),
        seller_tenant_id: None,
        resource_tenant_id: None,
        account_id: Uuid::from_u128(account),
        account_class: class,
        gl_code: None,
        side,
        money: money(amount, "EUR", 2),
        invoice_id: Some("inv-golden".to_owned()),
        due_date: NaiveDate::from_ymd_opt(2026, 7, 1),
        revenue_stream: None,
        mapping_status: MappingStatus::Resolved,
        functional_money: None,
        tax_jurisdiction: None,
        tax_filing_period: None,
        tax_rate_ref: None,
        legal_entity_id: None,
        invoice_item_ref: None,
        sku_or_plan_ref: None,
        price_id: None,
        pricing_snapshot_ref: None,
        po_allocation_group: None,
        credit_grant_event_type: None,
        ar_status: None,
    }
}

fn lines() -> [NewLine; 3] {
    let mut revenue = line(21, AccountClass::Revenue, Side::Credit, "10");
    revenue.invoice_id = None;
    revenue.due_date = None;
    revenue.revenue_stream = Some("subscription".to_owned());
    let mut tax = line(22, AccountClass::TaxPayable, Side::Credit, "2.34");
    tax.invoice_id = None;
    tax.due_date = None;
    tax.tax_jurisdiction = Some("DE".to_owned());
    tax.tax_filing_period = Some("2026Q2".to_owned());
    [
        line(20, AccountClass::Ar, Side::Debit, "12.34"),
        revenue,
        tax,
    ]
}

#[test]
fn line_order_does_not_change_the_hash() {
    let [a, b, c] = lines();
    let reference = IdempotencyGate::payload_hash(&entry(), &[a.clone(), b.clone(), c.clone()]);
    for order in [
        [a.clone(), c.clone(), b.clone()],
        [b.clone(), a.clone(), c.clone()],
        [b.clone(), c.clone(), a.clone()],
        [c.clone(), a.clone(), b.clone()],
        [c, b, a],
    ] {
        assert_eq!(IdempotencyGate::payload_hash(&entry(), &order), reference);
    }
}

#[test]
fn envelope_fields_do_not_change_the_hash_but_money_scale_does() {
    let reference = IdempotencyGate::payload_hash(&entry(), &lines());
    let mut envelope = entry();
    envelope.entry_id = Uuid::from_u128(99);
    envelope.posted_at_utc = OffsetDateTime::now_utc();
    envelope.correlation_id = Uuid::from_u128(98);
    envelope.posted_by_actor_id = Uuid::from_u128(97);
    let mut relabelled = lines();
    for line in &mut relabelled {
        line.line_id = Uuid::now_v7();
    }
    assert_eq!(
        IdempotencyGate::payload_hash(&envelope, &relabelled),
        reference
    );
    // The same digits at another stored scale are another payload.
    let mut rescaled = lines();
    rescaled[0].money = money("12.34", "EUR", 3);
    assert_ne!(
        IdempotencyGate::payload_hash(&entry(), &rescaled),
        reference
    );
}

/// The digest was derived independently of the Rust encoder (a byte-level
/// re-implementation of the framing: tag, 0x01/0x00 presence bytes, u32
/// big-endian lengths, sorted line keys, u64 key count), so a change to the
/// framing or to a stored literal fails here before it splits replays.
#[test]
fn golden_digest_pins_the_canonical_framing() {
    assert_eq!(
        IdempotencyGate::payload_hash(&entry(), &lines()),
        "88447e37d95298b8865a8be438df97685497a16d03af3cdd3c98e7547908108f"
    );
}
