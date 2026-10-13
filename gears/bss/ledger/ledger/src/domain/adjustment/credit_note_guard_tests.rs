//! Credit-note guards with no prior coverage: the per-condition amount messages
//! and the split invariants `build_credit_note_legs` refuses as `Internal`
//! (negative split parts, per-stream recognized parts that do not sum to the
//! split's recognized total).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use super::*;
use crate::domain::adjustment::splitter::{SplitResult, StreamSplit};
use bss_ledger_sdk::money::CurrencySpec;
use uuid::Uuid;

fn m(cents: i64) -> PostedMoney {
    PostedMoney::try_new(
        Decimal::new(cents, 2),
        CurrencySpec::try_new("USD".to_owned(), 2).unwrap(),
    )
    .unwrap()
}

/// An untaxed request (no breakdown needed) with `requested_deferred`.
fn req(amount: i64, tax: i64, requested_deferred: i64) -> CreditNoteRequest {
    CreditNoteRequest {
        tenant_id: Uuid::now_v7(),
        payer_tenant_id: Uuid::now_v7(),
        credit_note_id: "cn-1".to_owned(),
        origin_invoice_id: "inv-1".to_owned(),
        origin_invoice_item_ref: Some("item-1".to_owned()),
        po_allocation_group: Some("po-1".to_owned()),
        revenue_stream: "SAAS".to_owned(),
        amount: m(amount),
        tax_amount: m(tax),
        tax: Vec::new(),
        requested_deferred: m(requested_deferred),
        reason_code: "CUSTOMER_GOODWILL".to_owned(),
        goodwill: false,
    }
}

fn split(recognized: i64, deferred: i64, stream: (i64, i64)) -> SplitResult {
    SplitResult {
        recognized_part: m(recognized),
        deferred_part: m(deferred),
        per_stream: vec![StreamSplit {
            revenue_stream: "SAAS".to_owned(),
            schedule_id: "sch-1".to_owned(),
            recognized_part: m(stream.0),
            deferred_part: m(stream.1),
        }],
        split_basis_ref: "basis".to_owned(),
    }
}

fn internal(result: Result<CreditNoteLegPlan, DomainError>) -> String {
    match result {
        Err(DomainError::Internal(detail)) => detail,
        other => panic!(
            "expected Internal, got {}",
            if other.is_ok() { "Ok" } else { "another error" }
        ),
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
    assert_eq!(req(100, 10, 0).amount_ex_tax().unwrap(), m(90));
}

#[test]
fn per_stream_recognized_parts_must_sum_to_the_split_total() {
    // 1000 ex-tax: 700 recognized + 300 deferred overall, but the only stream
    // claims 600 recognized.
    let detail = internal(build_credit_note_legs(
        &req(1000, 0, 300),
        &split(700, 300, (600, 300)),
        m(1000),
    ));
    assert!(detail.contains("stream recognized"), "{detail}");
}

#[test]
fn negative_split_parts_are_refused() {
    let detail = internal(build_credit_note_legs(
        &req(1000, 0, 0),
        &split(-100, 1100, (-100, 1100)),
        m(1000),
    ));
    assert!(detail.contains("negative split part"), "{detail}");
    let detail = internal(build_credit_note_legs(
        &req(1000, 0, 300),
        &split(700, 300, (700, -300)),
        m(1000),
    ));
    assert!(detail.contains("negative stream split part"), "{detail}");
}

/// The one headroom rule: exact `original + debit − credit`, a named metadata
/// mismatch, and a negative (CHECK-violating) row as an invariant failure.
#[test]
fn remaining_headroom_is_exact_and_refuses_corrupt_rows() {
    assert_eq!(
        remaining_headroom(&m(1000), &m(250), &m(300)).unwrap(),
        m(950)
    );
    assert_eq!(remaining_headroom(&m(1000), &m(0), &m(1000)).unwrap(), m(0));
    match remaining_headroom(&m(1000), &m(0), &m(1001)) {
        Err(DomainError::Internal(detail)) => assert!(
            detail.contains("negative"),
            "the headroom error names the negative result"
        ),
        other => panic!(
            "expected Internal, got {}",
            if other.is_ok() { "Ok" } else { "another error" }
        ),
    }
    let other_scale = PostedMoney::try_new(
        Decimal::new(100, 3),
        CurrencySpec::try_new("USD".to_owned(), 3).unwrap(),
    )
    .unwrap();
    assert!(matches!(
        remaining_headroom(&m(1000), &other_scale, &m(0)),
        Err(DomainError::InconsistentScale(_))
    ));
}
