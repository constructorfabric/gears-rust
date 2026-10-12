use super::*;

/// Preserve the original USD scale-2 economic fixtures, expressed in major units.
fn fixture(value: i64) -> PostedMoney {
    PostedMoney::try_new(
        Decimal::new(value, 2),
        bss_ledger_sdk::CurrencySpec::try_new("USD".to_owned(), 2).unwrap(),
    )
    .unwrap()
}
fn expected(value: i64) -> Decimal {
    Decimal::new(value, 2)
}

fn input(amount: i64) -> SettlementReturnInput {
    SettlementReturnInput {
        tenant_id: Uuid::now_v7(),
        payer_tenant_id: Uuid::now_v7(),
        payment_id: "PAY-1".to_owned(),
        psp_return_id: "RET-1".to_owned(),
        amount: fixture(amount),
        effective_at: None,
    }
}

/// Σ of the debit-side amounts for these small scale-2 fixtures.
fn sum_dr(entry: &PostEntry) -> Decimal {
    entry
        .lines
        .iter()
        .filter(|l| l.side == Side::Debit)
        .map(|l| l.money.amount())
        .sum()
}

/// Σ of the credit-side line amounts.
fn sum_cr(entry: &PostEntry) -> Decimal {
    entry
        .lines
        .iter()
        .filter(|l| l.side == Side::Credit)
        .map(|l| l.money.amount())
        .sum()
}

/// Find the single line of a given class (panics if absent / not unique).
fn line_of(entry: &PostEntry, class: AccountClass) -> &PostLine {
    let mut it = entry.lines.iter().filter(|l| l.account_class == class);
    let line = it.next().expect("line of class present");
    assert!(it.next().is_none(), "class {class:?} is not unique");
    line
}

#[test]
fn fee_share_zero_omits_the_fee_leg() {
    // fee_share = 0 ⇒ the pre-Model-N 2-leg shape: DR UNALLOCATED / CR
    // CASH_CLEARING, both = amount, and NO PSP_FEE_EXPENSE line (never zero).
    let inp = input(1000);
    let entry = build_settlement_return_entry(&inp, &fixture(0)).unwrap();

    assert_eq!(entry.source_doc_type, SourceDocType::SettlementReturn);
    // The business id is the PSP return id, NOT the original payment id.
    assert_eq!(entry.source_business_id, "RET-1");
    assert_eq!(entry.reverses_entry_id, None);
    assert_eq!(entry.lines.len(), 2, "no fee leg when fee_share == 0");
    assert!(
        !entry
            .lines
            .iter()
            .any(|l| l.account_class == AccountClass::PspFeeExpense),
        "PSP_FEE_EXPENSE leg must be omitted entirely"
    );

    // DR UNALLOCATED (claw back from the pool).
    let unalloc = line_of(&entry, AccountClass::Unallocated);
    assert_eq!(unalloc.side, Side::Debit);
    assert_eq!(unalloc.money.amount(), expected(1000));

    // CR CASH_CLEARING (full amount — no fee to peel off).
    let cash = line_of(&entry, AccountClass::CashClearing);
    assert_eq!(cash.side, Side::Credit);
    assert_eq!(cash.money.amount(), expected(1000));

    // Balanced: Σ DR (1000) == Σ CR (1000).
    assert_eq!(sum_dr(&entry), expected(1000));
    assert_eq!(sum_cr(&entry), expected(1000));

    // Every line carries the payer, currency, and seller; none is tied to an
    // invoice (a return touches the pool, not a receivable).
    for l in &entry.lines {
        assert_eq!(l.payer_tenant_id, inp.payer_tenant_id);
        assert_eq!(l.money.currency().code(), "USD");
        assert_eq!(l.seller_tenant_id, Some(inp.tenant_id));
        assert_eq!(l.invoice_id, None);
    }
}

#[test]
fn full_return_is_the_mirror_of_settle() {
    // Full return of a fee-bearing payment (gross 100, fee 3): the symmetric
    // reverse of settle — DR UNALLOCATED 100 · CR CASH_CLEARING 97 · CR
    // PSP_FEE_EXPENSE 3 (the mirror of `DR CASH_CLEARING 97 · DR PSP_FEE_EXPENSE
    // 3 · CR UNALLOCATED 100`).
    let inp = input(100);
    let entry = build_settlement_return_entry(&inp, &fixture(3)).unwrap();
    assert_eq!(entry.lines.len(), 3, "3-leg symmetric reverse");

    let unalloc = line_of(&entry, AccountClass::Unallocated);
    assert_eq!(
        (unalloc.side, unalloc.money.amount()),
        (Side::Debit, expected(100))
    );

    let cash = line_of(&entry, AccountClass::CashClearing);
    assert_eq!(
        (cash.side, cash.money.amount()),
        (Side::Credit, expected(97)),
        "CASH_CLEARING reversed by the NET (amount − fee_share)"
    );

    let fee = line_of(&entry, AccountClass::PspFeeExpense);
    assert_eq!(
        (fee.side, fee.money.amount()),
        (Side::Credit, expected(3)),
        "PSP_FEE_EXPENSE reversed by the fee_share"
    );

    // Balanced: Σ DR (100) == Σ CR (97 + 3 = 100).
    assert_eq!(sum_dr(&entry), expected(100));
    assert_eq!(sum_cr(&entry), expected(100));
    assert_eq!(sum_dr(&entry), sum_cr(&entry));
}

#[test]
fn partial_return_peels_a_proportional_fee_slice() {
    // A partial return with a non-trivial fee slice: amount 600, fee_share 18 ⇒
    // CASH_CLEARING reversed by 582, PSP_FEE_EXPENSE by 18; still balanced.
    let inp = input(600);
    let entry = build_settlement_return_entry(&inp, &fixture(18)).unwrap();
    assert_eq!(entry.lines.len(), 3);

    assert_eq!(
        line_of(&entry, AccountClass::Unallocated).money.amount(),
        expected(600)
    );
    assert_eq!(
        line_of(&entry, AccountClass::CashClearing).money.amount(),
        expected(582),
        "amount − fee_share"
    );
    assert_eq!(
        line_of(&entry, AccountClass::PspFeeExpense).money.amount(),
        expected(18)
    );
    assert_eq!(sum_dr(&entry), expected(600));
    assert_eq!(sum_cr(&entry), expected(600));
}

#[test]
fn zero_amount_is_rejected() {
    let err = build_settlement_return_entry(&input(0), &fixture(0)).unwrap_err();
    assert!(matches!(err, DomainError::InvalidRequest(_)));
}

#[test]
fn negative_amount_is_rejected() {
    let err = build_settlement_return_entry(&input(-1), &fixture(0)).unwrap_err();
    assert!(matches!(err, DomainError::InvalidRequest(_)));
}

#[test]
fn negative_fee_share_is_rejected() {
    // fee_share < 0 is a defensive `InvalidRequest` (the orchestrator never
    // produces it; a breach would otherwise unbalance the entry).
    let err = build_settlement_return_entry(&input(1000), &fixture(-1)).unwrap_err();
    assert!(matches!(err, DomainError::InvalidRequest(_)));
}

#[test]
fn fee_share_exceeding_amount_is_rejected() {
    // fee_share > amount can't be reversed (the CASH_CLEARING leg would go
    // negative) ⇒ `InvalidRequest`. fee_share == amount is the boundary and is
    // accepted (a degenerate all-fee return).
    let err = build_settlement_return_entry(&input(1000), &fixture(1001)).unwrap_err();
    assert!(matches!(err, DomainError::InvalidRequest(_)));

    let ok = build_settlement_return_entry(&input(1000), &fixture(1000)).unwrap();
    assert_eq!(ok.lines.len(), 3, "fee_share == amount is accepted");
    // CASH_CLEARING leg is zero-amount here, but it is a structural reverse leg
    // (not a fee leg) so it is still emitted; the entry balances (DR 1000 = CR
    // 0 + CR 1000).
    assert_eq!(sum_dr(&ok), sum_cr(&ok));
}
