use super::*;

fn input(gross: i64, fee: i64) -> SettlementInput {
    SettlementInput {
        tenant_id: Uuid::now_v7(),
        payer_tenant_id: Uuid::now_v7(),
        payment_id: "PAY-1".to_owned(),
        gross_minor: gross,
        fee_minor: fee,
        currency: "USD".to_owned(),
        effective_at: None,
    }
}

/// Σ of the debit-side line amounts (widened, mirroring the builder's i128).
fn sum_dr(entry: &PostEntry) -> i128 {
    entry
        .lines
        .iter()
        .filter(|l| l.side == Side::Debit)
        .map(|l| i128::from(l.amount_minor))
        .sum()
}

/// Σ of the credit-side line amounts.
fn sum_cr(entry: &PostEntry) -> i128 {
    entry
        .lines
        .iter()
        .filter(|l| l.side == Side::Credit)
        .map(|l| i128::from(l.amount_minor))
        .sum()
}

#[test]
fn three_lines_with_fee() {
    let inp = input(1000, 30);
    let entry = build_settlement_entry(&inp).unwrap();

    assert_eq!(entry.source_doc_type, SourceDocType::PaymentSettle);
    assert_eq!(entry.source_business_id, "PAY-1");
    assert_eq!(entry.lines.len(), 3);

    // DR CASH_CLEARING net (970).
    let cash = &entry.lines[0];
    assert_eq!(cash.account_class, AccountClass::CashClearing);
    assert_eq!(cash.side, Side::Debit);
    assert_eq!(cash.amount_minor, 970);

    // DR PSP_FEE_EXPENSE fee (30).
    let fee = &entry.lines[1];
    assert_eq!(fee.account_class, AccountClass::PspFeeExpense);
    assert_eq!(fee.side, Side::Debit);
    assert_eq!(fee.amount_minor, 30);

    // CR UNALLOCATED gross (1000).
    let unalloc = &entry.lines[2];
    assert_eq!(unalloc.account_class, AccountClass::Unallocated);
    assert_eq!(unalloc.side, Side::Credit);
    assert_eq!(unalloc.amount_minor, 1000);

    // Balanced: Σ DR (1000) == Σ CR (1000).
    assert_eq!(sum_dr(&entry), 1000);
    assert_eq!(sum_cr(&entry), 1000);
    assert_eq!(sum_dr(&entry), sum_cr(&entry));

    // Every line carries the payer, currency, and seller.
    for l in &entry.lines {
        assert_eq!(l.payer_tenant_id, inp.payer_tenant_id);
        assert_eq!(l.currency, "USD");
        assert_eq!(l.seller_tenant_id, Some(inp.tenant_id));
        assert_eq!(l.invoice_id, None);
    }
}

#[test]
fn two_lines_when_fee_zero() {
    let inp = input(1000, 0);
    let entry = build_settlement_entry(&inp).unwrap();

    // No PSP_FEE line is emitted for a zero fee.
    assert_eq!(entry.lines.len(), 2);
    assert!(
        entry
            .lines
            .iter()
            .all(|l| l.account_class != AccountClass::PspFeeExpense)
    );

    let cash = &entry.lines[0];
    assert_eq!(cash.account_class, AccountClass::CashClearing);
    assert_eq!(cash.side, Side::Debit);
    assert_eq!(cash.amount_minor, 1000);

    let unalloc = &entry.lines[1];
    assert_eq!(unalloc.account_class, AccountClass::Unallocated);
    assert_eq!(unalloc.side, Side::Credit);
    assert_eq!(unalloc.amount_minor, 1000);

    assert_eq!(sum_dr(&entry), sum_cr(&entry));
}

#[test]
fn negative_fee_is_rejected() {
    let err = build_settlement_entry(&input(1000, -1)).unwrap_err();
    assert!(matches!(err, DomainError::InvalidRequest(_)));
}

#[test]
fn fee_above_gross_is_rejected() {
    let err = build_settlement_entry(&input(1000, 1001)).unwrap_err();
    assert!(matches!(err, DomainError::InvalidRequest(_)));
}

#[test]
fn fee_equal_to_gross_is_rejected() {
    // A 100%-fee settlement (net = 0) parks nothing in the pool — rejected at the
    // boundary with a clear InvalidRequest, not a misleading AMOUNT_OUT_OF_RANGE
    // from a zero `DR CASH_CLEARING` line deep in the engine.
    let err = build_settlement_entry(&input(1000, 1000)).unwrap_err();
    assert!(matches!(err, DomainError::InvalidRequest(_)));
}
