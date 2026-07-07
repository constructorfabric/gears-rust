use super::*;

// ── helpers ──────────────────────────────────────────────────────────────

fn subgrain(event_type: &str, available: i64) -> CreditSubgrain {
    CreditSubgrain {
        credit_grant_event_type: event_type.to_owned(),
        available_minor: available,
    }
}

fn debit(event_type: &str, amount: i64) -> CreditDebit {
    CreditDebit {
        credit_grant_event_type: event_type.to_owned(),
        amount_minor: amount,
    }
}

fn target(id: &str, amount: i64) -> Allocated {
    Allocated {
        invoice_id: id.to_owned(),
        amount_minor: amount,
    }
}

/// An open candidate with the given remaining balance. `original_posted_at` is
/// irrelevant to credit-target validation (no ordering), so it stays `None`.
fn candidate(id: &str, open: i64) -> Candidate {
    Candidate {
        invoice_id: id.to_owned(),
        open_minor: open,
        original_posted_at: None,
    }
}

fn grant_input(amount: i64, event_type: &str) -> GrantInput {
    GrantInput {
        tenant_id: Uuid::now_v7(),
        payer_tenant_id: Uuid::now_v7(),
        credit_application_id: "CREDIT-1".to_owned(),
        currency: "USD".to_owned(),
        amount_minor: amount,
        credit_grant_event_type: event_type.to_owned(),
        effective_at: None,
    }
}

fn apply_input(debits: Vec<CreditDebit>, targets: Vec<Allocated>) -> ApplyInput {
    ApplyInput {
        tenant_id: Uuid::now_v7(),
        payer_tenant_id: Uuid::now_v7(),
        credit_application_id: "CREDIT-1".to_owned(),
        currency: "USD".to_owned(),
        debits,
        targets,
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

// ── build_grant_entry ────────────────────────────────────────────────────

#[test]
fn grant_balances_dr_unallocated_cr_reusable_credit() {
    let inp = grant_input(500, "promo");
    let entry = build_grant_entry(&inp).unwrap();

    assert_eq!(entry.source_doc_type, SourceDocType::CreditApply);
    assert_eq!(entry.source_business_id, "CREDIT-1");
    assert_eq!(entry.lines.len(), 2);

    // DR UNALLOCATED (amount), no event-type, no invoice.
    let unalloc = &entry.lines[0];
    assert_eq!(unalloc.account_class, AccountClass::Unallocated);
    assert_eq!(unalloc.side, Side::Debit);
    assert_eq!(unalloc.amount_minor, 500);
    assert_eq!(unalloc.credit_grant_event_type, None);
    assert_eq!(unalloc.invoice_id, None);

    // CR REUSABLE_CREDIT (amount), carrying the sub-grain bucket.
    let credit = &entry.lines[1];
    assert_eq!(credit.account_class, AccountClass::ReusableCredit);
    assert_eq!(credit.side, Side::Credit);
    assert_eq!(credit.amount_minor, 500);
    assert_eq!(
        credit.credit_grant_event_type,
        Some("promo".to_owned()),
        "the REUSABLE_CREDIT line carries the event-type"
    );
    assert_eq!(credit.invoice_id, None);

    // Balanced: Σ DR (500) == Σ CR (500) == amount.
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
fn grant_non_positive_amount_is_rejected() {
    let err = build_grant_entry(&grant_input(0, "promo")).unwrap_err();
    assert!(matches!(err, DomainError::InvalidRequest(_)), "{err:?}");

    let err = build_grant_entry(&grant_input(-5, "promo")).unwrap_err();
    assert!(matches!(err, DomainError::InvalidRequest(_)), "{err:?}");
}

#[test]
fn grant_empty_event_type_is_rejected() {
    let err = build_grant_entry(&grant_input(500, "")).unwrap_err();
    assert!(matches!(err, DomainError::InvalidRequest(_)), "{err:?}");
}

// ── plan_wallet_debit ─────────────────────────────────────────────────────

#[test]
fn plan_fills_oldest_first_across_two_subgrains() {
    // 700 across [old:500, new:400] in order ⇒ 500 from old, 200 from new.
    let subgrains = vec![subgrain("old", 500), subgrain("new", 400)];
    let out = plan_wallet_debit(&subgrains, 700).unwrap();
    assert_eq!(out, vec![debit("old", 500), debit("new", 200)]);
}

#[test]
fn plan_exact_fit_single_grain() {
    // Amount exactly matches the first sub-grain's availability ⇒ one debit, the
    // rest untouched.
    let subgrains = vec![subgrain("old", 300), subgrain("new", 400)];
    let out = plan_wallet_debit(&subgrains, 300).unwrap();
    assert_eq!(out, vec![debit("old", 300)]);
}

#[test]
fn plan_spans_grains_in_order() {
    // 250 across three grains ⇒ 100 + 100 + 50, in fill order, stopping at 0.
    let subgrains = vec![subgrain("a", 100), subgrain("b", 100), subgrain("c", 100)];
    let out = plan_wallet_debit(&subgrains, 250).unwrap();
    assert_eq!(out, vec![debit("a", 100), debit("b", 100), debit("c", 50)]);
}

#[test]
fn plan_overdraw_is_credit_exceeds_wallet() {
    // Σ available (300) < amount (301) ⇒ wallet cannot cover the spend.
    let subgrains = vec![subgrain("a", 100), subgrain("b", 200)];
    let err = plan_wallet_debit(&subgrains, 301).unwrap_err();
    assert!(
        matches!(err, DomainError::CreditExceedsWallet(_)),
        "{err:?}"
    );
}

#[test]
fn plan_skips_zero_and_negative_availabilities() {
    // Zero/negative sub-grains contribute nothing and are skipped in the fill;
    // the 300 is drawn entirely from the two positive grains.
    let subgrains = vec![
        subgrain("zero", 0),
        subgrain("a", 200),
        subgrain("neg", -50),
        subgrain("b", 200),
    ];
    let out = plan_wallet_debit(&subgrains, 300).unwrap();
    assert_eq!(out, vec![debit("a", 200), debit("b", 100)]);
}

#[test]
fn plan_non_positive_amount_is_rejected() {
    let subgrains = vec![subgrain("a", 500)];
    let err = plan_wallet_debit(&subgrains, 0).unwrap_err();
    assert!(matches!(err, DomainError::InvalidRequest(_)), "{err:?}");

    let err = plan_wallet_debit(&subgrains, -10).unwrap_err();
    assert!(matches!(err, DomainError::InvalidRequest(_)), "{err:?}");
}

#[test]
fn plan_returns_positive_amounts_in_fill_order() {
    // Every emitted debit is positive and they appear in the input order.
    let subgrains = vec![subgrain("first", 50), subgrain("second", 50)];
    let out = plan_wallet_debit(&subgrains, 100).unwrap();
    assert_eq!(out.len(), 2);
    assert!(out.iter().all(|d| d.amount_minor > 0));
    assert_eq!(out[0].credit_grant_event_type, "first");
    assert_eq!(out[1].credit_grant_event_type, "second");
}

// ── validate_credit_targets ────────────────────────────────────────────────

#[test]
fn targets_happy_preserves_order_and_amounts() {
    let candidates = vec![candidate("A", 300), candidate("B", 800)];
    // Caller order (B before A) is preserved verbatim — never reordered to the
    // candidate order; under-paying a candidate is allowed.
    let targets = vec![target("B", 200), target("A", 250)];
    let out = validate_credit_targets(&candidates, &targets).unwrap();
    assert_eq!(out, targets, "validated targets returned in caller order");
}

#[test]
fn targets_full_open_is_allowed() {
    // Exactly the open balance is at the boundary (`<=`), so it passes. There is
    // no lump cap here (the wallet cap is plan_wallet_debit).
    let candidates = vec![candidate("A", 300), candidate("B", 200)];
    let targets = vec![target("A", 300), target("B", 200)];
    assert!(validate_credit_targets(&candidates, &targets).is_ok());
}

#[test]
fn targets_over_open_is_credit_exceeds_open_ar() {
    let candidates = vec![candidate("A", 300)];
    // 301 > A's open 300.
    let err = validate_credit_targets(&candidates, &[target("A", 301)]).unwrap_err();
    assert!(
        matches!(err, DomainError::CreditExceedsOpenAr(_)),
        "{err:?}"
    );
}

#[test]
fn targets_unknown_invoice_is_credit_exceeds_open_ar() {
    let candidates = vec![candidate("A", 300)];
    let err = validate_credit_targets(&candidates, &[target("Z", 100)]).unwrap_err();
    assert!(
        matches!(err, DomainError::CreditExceedsOpenAr(_)),
        "{err:?}"
    );
}

#[test]
fn targets_closed_candidate_is_credit_exceeds_open_ar() {
    // A present but fully-paid (open 0) candidate is not a valid target.
    let candidates = vec![candidate("A", 0)];
    let err = validate_credit_targets(&candidates, &[target("A", 1)]).unwrap_err();
    assert!(
        matches!(err, DomainError::CreditExceedsOpenAr(_)),
        "{err:?}"
    );
}

#[test]
fn targets_duplicate_invoice_is_credit_exceeds_open_ar() {
    let candidates = vec![candidate("A", 300)];
    let err =
        validate_credit_targets(&candidates, &[target("A", 100), target("A", 50)]).unwrap_err();
    assert!(
        matches!(err, DomainError::CreditExceedsOpenAr(_)),
        "{err:?}"
    );
}

#[test]
fn targets_non_positive_amount_is_credit_exceeds_open_ar() {
    let candidates = vec![candidate("A", 300), candidate("B", 300)];
    let err = validate_credit_targets(&candidates, &[target("A", 0)]).unwrap_err();
    assert!(
        matches!(err, DomainError::CreditExceedsOpenAr(_)),
        "{err:?}"
    );

    let err = validate_credit_targets(&candidates, &[target("B", -5)]).unwrap_err();
    assert!(
        matches!(err, DomainError::CreditExceedsOpenAr(_)),
        "{err:?}"
    );
}

// ── build_apply_entry ──────────────────────────────────────────────────────

#[test]
fn apply_balances_dr_reusable_credit_cr_ar() {
    // Two wallet draw-downs (200 + 100 = 300) paying two receivables (250 + 50 =
    // 300).
    let inp = apply_input(
        vec![debit("old", 200), debit("new", 100)],
        vec![target("INV-1", 250), target("INV-2", 50)],
    );
    let entry = build_apply_entry(&inp).unwrap();

    assert_eq!(entry.source_doc_type, SourceDocType::CreditApply);
    assert_eq!(entry.source_business_id, "CREDIT-1");
    assert_eq!(entry.lines.len(), 4);

    // N×DR REUSABLE_CREDIT FIRST, in debits order, each carrying its event-type,
    // no invoice.
    let dr_old = &entry.lines[0];
    assert_eq!(dr_old.account_class, AccountClass::ReusableCredit);
    assert_eq!(dr_old.side, Side::Debit);
    assert_eq!(dr_old.amount_minor, 200);
    assert_eq!(dr_old.credit_grant_event_type, Some("old".to_owned()));
    assert_eq!(dr_old.invoice_id, None);

    let dr_new = &entry.lines[1];
    assert_eq!(dr_new.account_class, AccountClass::ReusableCredit);
    assert_eq!(dr_new.side, Side::Debit);
    assert_eq!(dr_new.amount_minor, 100);
    assert_eq!(dr_new.credit_grant_event_type, Some("new".to_owned()));
    assert_eq!(dr_new.invoice_id, None);

    // M×CR AR next, in targets order, each carrying its invoice_id, no event-type.
    let cr_1 = &entry.lines[2];
    assert_eq!(cr_1.account_class, AccountClass::Ar);
    assert_eq!(cr_1.side, Side::Credit);
    assert_eq!(cr_1.amount_minor, 250);
    assert_eq!(cr_1.invoice_id, Some("INV-1".to_owned()));
    assert_eq!(cr_1.credit_grant_event_type, None);

    let cr_2 = &entry.lines[3];
    assert_eq!(cr_2.account_class, AccountClass::Ar);
    assert_eq!(cr_2.side, Side::Credit);
    assert_eq!(cr_2.amount_minor, 50);
    assert_eq!(cr_2.invoice_id, Some("INV-2".to_owned()));
    assert_eq!(cr_2.credit_grant_event_type, None);

    // Balanced: Σ DR (300) == Σ CR (300).
    assert_eq!(sum_dr(&entry), 300);
    assert_eq!(sum_cr(&entry), 300);
    assert_eq!(sum_dr(&entry), sum_cr(&entry));

    // Every line carries the payer, currency, and seller.
    for l in &entry.lines {
        assert_eq!(l.payer_tenant_id, inp.payer_tenant_id);
        assert_eq!(l.currency, "USD");
        assert_eq!(l.seller_tenant_id, Some(inp.tenant_id));
    }
}

#[test]
fn apply_unbalanced_sides_is_rejected() {
    // Σ debits (300) != Σ targets (250) ⇒ the balance backstop rejects it.
    let inp = apply_input(vec![debit("old", 300)], vec![target("INV-1", 250)]);
    let err = build_apply_entry(&inp).unwrap_err();
    assert!(matches!(err, DomainError::InvalidRequest(_)), "{err:?}");
}

#[test]
fn apply_empty_debits_is_rejected() {
    let inp = apply_input(vec![], vec![target("INV-1", 100)]);
    let err = build_apply_entry(&inp).unwrap_err();
    assert!(matches!(err, DomainError::InvalidRequest(_)), "{err:?}");
}

#[test]
fn apply_empty_targets_is_rejected() {
    let inp = apply_input(vec![debit("old", 100)], vec![]);
    let err = build_apply_entry(&inp).unwrap_err();
    assert!(matches!(err, DomainError::InvalidRequest(_)), "{err:?}");
}

#[test]
fn apply_non_positive_amount_is_rejected() {
    // A non-positive debit.
    let inp = apply_input(vec![debit("old", 0)], vec![target("INV-1", 0)]);
    let err = build_apply_entry(&inp).unwrap_err();
    assert!(matches!(err, DomainError::InvalidRequest(_)), "{err:?}");

    // A non-positive target (debits sum positive so the empty/positive guards
    // pass first; the target guard catches it).
    let inp = apply_input(vec![debit("old", 100)], vec![target("INV-1", -5)]);
    let err = build_apply_entry(&inp).unwrap_err();
    assert!(matches!(err, DomainError::InvalidRequest(_)), "{err:?}");
}
