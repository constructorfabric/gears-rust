//! Per-condition debit-note amount messages: each failure names its field and
//! the values involved.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use super::*;
use bss_ledger_sdk::money::CurrencySpec;
use uuid::Uuid;

fn m(cents: i64) -> PostedMoney {
    PostedMoney::try_new(
        Decimal::new(cents, 2),
        CurrencySpec::try_new("USD".to_owned(), 2).unwrap(),
    )
    .unwrap()
}

fn req(amount: i64, tax: i64, deferred: i64) -> DebitNoteRequest {
    DebitNoteRequest {
        tenant_id: Uuid::now_v7(),
        payer_tenant_id: Uuid::now_v7(),
        debit_note_id: "dn-1".to_owned(),
        origin_invoice_id: "inv-1".to_owned(),
        origin_invoice_item_ref: Some("item-1".to_owned()),
        revenue_stream: "SAAS".to_owned(),
        amount: m(amount),
        tax_amount: m(tax),
        tax: Vec::new(),
        deferred: m(deferred),
        reason_code: "ADDITIONAL_USAGE".to_owned(),
        recognition: None,
    }
}

fn out_of_range(result: Result<PostedMoney, DomainError>) -> String {
    match result {
        Err(DomainError::AmountOutOfRange(detail)) => detail,
        other => panic!("expected AmountOutOfRange, got {other:?}"),
    }
}

#[test]
fn each_amount_failure_names_its_field_and_values() {
    let detail = out_of_range(req(-100, 0, 0).amount_ex_tax());
    assert!(
        detail.contains("amount must be >= 0") && detail.contains("-1 USD"),
        "{detail}"
    );
    let detail = out_of_range(req(100, -10, 0).amount_ex_tax());
    assert!(
        detail.contains("tax_amount must be >= 0") && detail.contains("-0.1 USD"),
        "{detail}"
    );
    let detail = out_of_range(req(100, 200, 0).amount_ex_tax());
    assert!(
        detail.contains("tax_amount 2 USD exceeds amount 1 USD"),
        "{detail}"
    );
    let detail = out_of_range(req(100, 0, -10).recognized());
    assert!(
        detail.contains("deferred must be >= 0") && detail.contains("-0.1 USD"),
        "{detail}"
    );
    let detail = out_of_range(req(100, 10, 95).recognized());
    assert!(
        detail.contains("deferred 0.95 USD exceeds the ex-tax amount 0.9 USD"),
        "{detail}"
    );
    assert_eq!(req(100, 10, 40).recognized().unwrap(), m(50));
}
