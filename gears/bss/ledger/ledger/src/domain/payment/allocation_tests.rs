use super::*;

fn split(id: &str, amount: i64) -> Allocated {
    Allocated {
        invoice_id: id.to_owned(),
        amount_minor: amount,
    }
}

fn input(splits: Vec<Allocated>) -> AllocationInput {
    AllocationInput {
        tenant_id: Uuid::now_v7(),
        payer_tenant_id: Uuid::now_v7(),
        payment_id: "PAY-1".to_owned(),
        allocation_id: Uuid::now_v7(),
        currency: "USD".to_owned(),
        splits,
        effective_at: None,
    }
}

fn sum_dr(entry: &PostEntry) -> i128 {
    entry
        .lines
        .iter()
        .filter(|l| l.side == Side::Debit)
        .map(|l| i128::from(l.amount_minor))
        .sum()
}

fn sum_cr(entry: &PostEntry) -> i128 {
    entry
        .lines
        .iter()
        .filter(|l| l.side == Side::Credit)
        .map(|l| i128::from(l.amount_minor))
        .sum()
}

#[test]
fn two_splits_dr_unallocated_plus_cr_ar_per_invoice() {
    let inp = input(vec![split("A", 300), split("B", 200)]);
    let entry = build_allocation_entry(&inp).unwrap();

    assert_eq!(entry.source_doc_type, SourceDocType::PaymentAllocate);
    assert_eq!(entry.source_business_id, inp.allocation_id.to_string());
    assert_eq!(entry.lines.len(), 3);

    // DR UNALLOCATED for the sum (500) FIRST, no invoice_id.
    let unalloc = &entry.lines[0];
    assert_eq!(unalloc.account_class, AccountClass::Unallocated);
    assert_eq!(unalloc.side, Side::Debit);
    assert_eq!(unalloc.amount_minor, 500);
    assert_eq!(unalloc.invoice_id, None);

    // CR AR per split, in splits order, each carrying its invoice_id.
    let ar_a = &entry.lines[1];
    assert_eq!(ar_a.account_class, AccountClass::Ar);
    assert_eq!(ar_a.side, Side::Credit);
    assert_eq!(ar_a.amount_minor, 300);
    assert_eq!(ar_a.invoice_id, Some("A".to_owned()));

    let ar_b = &entry.lines[2];
    assert_eq!(ar_b.account_class, AccountClass::Ar);
    assert_eq!(ar_b.side, Side::Credit);
    assert_eq!(ar_b.amount_minor, 200);
    assert_eq!(ar_b.invoice_id, Some("B".to_owned()));

    // Balanced: Σ DR (500) == Σ CR (500).
    assert_eq!(sum_dr(&entry), 500);
    assert_eq!(sum_cr(&entry), 500);
    assert_eq!(sum_dr(&entry), sum_cr(&entry));

    // Every line carries the payer, currency, and seller.
    for l in &entry.lines {
        assert_eq!(l.payer_tenant_id, inp.payer_tenant_id);
        assert_eq!(l.currency, "USD");
        assert_eq!(l.seller_tenant_id, Some(inp.tenant_id));
    }
}

#[test]
fn empty_splits_is_rejected() {
    let err = build_allocation_entry(&input(vec![])).unwrap_err();
    assert!(matches!(err, DomainError::InvalidRequest(_)));
}

#[test]
fn non_positive_split_amount_is_rejected() {
    let err = build_allocation_entry(&input(vec![split("A", 0)])).unwrap_err();
    assert!(matches!(err, DomainError::InvalidRequest(_)));

    let err = build_allocation_entry(&input(vec![split("A", 300), split("B", -5)])).unwrap_err();
    assert!(matches!(err, DomainError::InvalidRequest(_)));
}

// ── validate_caller_split (Mode B, §4.4 F-5) ─────────────────────────────

/// An open candidate with the given remaining balance. `original_posted_at`
/// is irrelevant to caller-split validation (no ordering), so it stays `None`.
fn candidate(id: &str, open: i64) -> Candidate {
    Candidate {
        invoice_id: id.to_owned(),
        open_minor: open,
        original_posted_at: None,
    }
}

#[test]
fn caller_split_happy_preserves_order_and_amounts() {
    let candidates = vec![candidate("A", 300), candidate("B", 800)];
    // Caller order (B before A) is preserved verbatim — never reordered to
    // the candidate order; under-allocating the lump is allowed.
    let caller = vec![split("B", 200), split("A", 250)];
    let out = validate_caller_split(&candidates, &caller, 500).unwrap();
    assert_eq!(out, caller, "validated splits returned in caller order");
}

#[test]
fn caller_split_full_open_and_full_lump_is_allowed() {
    // Exactly the open balance and exactly the lump are both at the boundary
    // (`<=`), so they pass.
    let candidates = vec![candidate("A", 300), candidate("B", 200)];
    let caller = vec![split("A", 300), split("B", 200)];
    assert!(validate_caller_split(&candidates, &caller, 500).is_ok());
}

#[test]
fn caller_split_over_open_is_rejected() {
    let candidates = vec![candidate("A", 300)];
    // 301 > A's open 300.
    let err = validate_caller_split(&candidates, &[split("A", 301)], 1000).unwrap_err();
    assert!(
        matches!(err, DomainError::AllocationSplitInvalid(_)),
        "{err:?}"
    );
}

#[test]
fn caller_split_over_lump_is_rejected() {
    let candidates = vec![candidate("A", 300), candidate("B", 300)];
    // Each share is within its open balance, but 300 + 300 > lump 500.
    let err =
        validate_caller_split(&candidates, &[split("A", 300), split("B", 300)], 500).unwrap_err();
    assert!(
        matches!(err, DomainError::AllocationSplitInvalid(_)),
        "{err:?}"
    );
}

#[test]
fn caller_split_unknown_invoice_is_rejected() {
    let candidates = vec![candidate("A", 300)];
    let err = validate_caller_split(&candidates, &[split("Z", 100)], 1000).unwrap_err();
    assert!(
        matches!(err, DomainError::AllocationSplitInvalid(_)),
        "{err:?}"
    );
}

#[test]
fn caller_split_closed_candidate_is_rejected() {
    // A present but fully-paid (open 0) candidate is not a valid target.
    let candidates = vec![candidate("A", 0)];
    let err = validate_caller_split(&candidates, &[split("A", 1)], 1000).unwrap_err();
    assert!(
        matches!(err, DomainError::AllocationSplitInvalid(_)),
        "{err:?}"
    );
}

#[test]
fn caller_split_duplicate_invoice_is_rejected() {
    let candidates = vec![candidate("A", 300)];
    let err =
        validate_caller_split(&candidates, &[split("A", 100), split("A", 50)], 1000).unwrap_err();
    assert!(
        matches!(err, DomainError::AllocationSplitInvalid(_)),
        "{err:?}"
    );
}

#[test]
fn caller_split_non_positive_amount_is_rejected() {
    let candidates = vec![candidate("A", 300), candidate("B", 300)];
    let err = validate_caller_split(&candidates, &[split("A", 0)], 1000).unwrap_err();
    assert!(
        matches!(err, DomainError::AllocationSplitInvalid(_)),
        "{err:?}"
    );

    let err = validate_caller_split(&candidates, &[split("B", -5)], 1000).unwrap_err();
    assert!(
        matches!(err, DomainError::AllocationSplitInvalid(_)),
        "{err:?}"
    );
}

#[test]
fn caller_split_empty_is_ok_but_builds_nothing() {
    // An empty caller split validates (vacuously) — the orchestrator's
    // separate empty-splits guard is what rejects a no-op allocation, exactly
    // as it does for an empty precedence result.
    let candidates = vec![candidate("A", 300)];
    assert_eq!(
        validate_caller_split(&candidates, &[], 500).unwrap(),
        vec![]
    );
}
