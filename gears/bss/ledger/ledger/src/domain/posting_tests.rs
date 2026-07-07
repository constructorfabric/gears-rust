//! Tests for the pure posting invariants ([`super::validate_balanced_entry`]).

use super::*;

fn line(side: Side, amount: i64, payer: Uuid) -> LineFacts {
    LineFacts {
        side,
        amount_minor: amount,
        currency: "USD".to_owned(),
        currency_scale: 2,
        payer_tenant_id: payer,
        functional_amount_minor: None,
    }
}

/// A line carrying an explicit functional amount (Slice 5 dual-column tests).
fn line_f(side: Side, amount: i64, payer: Uuid, functional: Option<i64>) -> LineFacts {
    LineFacts {
        side,
        amount_minor: amount,
        currency: "USD".to_owned(),
        currency_scale: 2,
        payer_tenant_id: payer,
        functional_amount_minor: functional,
    }
}

#[test]
fn balanced_entry_is_ok() {
    let p = Uuid::now_v7();
    let lines = vec![line(Side::Debit, 1000, p), line(Side::Credit, 1000, p)];
    assert!(validate_balanced_entry("USD", &lines).is_ok());
}

#[test]
fn empty_is_rejected() {
    assert_eq!(
        validate_balanced_entry("USD", &[]),
        Err(PostingViolation::Empty)
    );
}

#[test]
fn unbalanced_is_rejected() {
    let p = Uuid::now_v7();
    let lines = vec![line(Side::Debit, 1000, p), line(Side::Credit, 700, p)];
    assert_eq!(
        validate_balanced_entry("USD", &lines),
        Err(PostingViolation::Unbalanced)
    );
}

#[test]
fn mixed_payer_is_rejected() {
    let lines = vec![
        line(Side::Debit, 1000, Uuid::now_v7()),
        line(Side::Credit, 1000, Uuid::now_v7()),
    ];
    assert_eq!(
        validate_balanced_entry("USD", &lines),
        Err(PostingViolation::MixedPayer)
    );
}

#[test]
fn foreign_currency_line_is_rejected() {
    let p = Uuid::now_v7();
    let mut eur = line(Side::Credit, 1000, p);
    eur.currency = "EUR".to_owned();
    let lines = vec![line(Side::Debit, 1000, p), eur];
    assert_eq!(
        validate_balanced_entry("USD", &lines),
        Err(PostingViolation::CurrencyMismatch)
    );
}

#[test]
fn inconsistent_scale_same_currency_is_rejected() {
    let p = Uuid::now_v7();
    // Two USD lines that net to zero but carry different scales: a wrong
    // per-line scale must be rejected, not silently posted at the wrong
    // implied magnitude.
    let mut hi = line(Side::Credit, 1000, p);
    hi.currency_scale = 3;
    let lines = vec![line(Side::Debit, 1000, p), hi];
    assert_eq!(
        validate_balanced_entry("USD", &lines),
        Err(PostingViolation::InconsistentScale)
    );
}

#[test]
fn each_violation_maps_to_its_domain_error() {
    assert!(matches!(
        DomainError::from(PostingViolation::Empty),
        DomainError::Empty(_)
    ));
    assert!(matches!(
        DomainError::from(PostingViolation::MixedPayer),
        DomainError::MixedPayer(_)
    ));
    // A wrong per-line scale surfaces as InconsistentScale (wire
    // `AMOUNT_OUT_OF_RANGE`), not a balance fault.
    assert!(matches!(
        DomainError::from(PostingViolation::InconsistentScale),
        DomainError::InconsistentScale(_)
    ));
    assert!(matches!(
        DomainError::from(PostingViolation::Unbalanced),
        DomainError::Unbalanced(_)
    ));
    // CurrencyMismatch has no dedicated variant — surfaces as unbalanced.
    assert!(matches!(
        DomainError::from(PostingViolation::CurrencyMismatch),
        DomainError::Unbalanced(_)
    ));
}

#[test]
fn negative_amount_lines_are_rejected_even_when_balanced() {
    let p = Uuid::now_v7();
    // Two negative-amount lines net to zero, but a negative amount violates
    // chk_journal_line_amount (amount > 0, or 0 with a functional amount) —
    // it must be rejected before COMMIT, not surface as a DB constraint fault.
    let lines = vec![line(Side::Debit, -100, p), line(Side::Credit, -100, p)];
    assert_eq!(
        validate_balanced_entry("USD", &lines),
        Err(PostingViolation::AmountOutOfRange)
    );
}

#[test]
fn zero_amount_without_functional_is_rejected() {
    let p = Uuid::now_v7();
    let lines = vec![line(Side::Debit, 0, p), line(Side::Credit, 0, p)];
    assert_eq!(
        validate_balanced_entry("USD", &lines),
        Err(PostingViolation::AmountOutOfRange)
    );
}

#[test]
fn functional_only_zero_amount_lines_are_allowed() {
    let p = Uuid::now_v7();
    // Functional-only lines (amount 0 WITH a positive functional amount) are valid
    // and must balance in the functional column — a DR/CR pair nets to zero there.
    let dr = line_f(Side::Debit, 0, p, Some(500));
    let cr = line_f(Side::Credit, 0, p, Some(500));
    assert!(validate_balanced_entry("USD", &[dr, cr]).is_ok());
}

#[test]
fn zero_amount_with_zero_functional_is_rejected() {
    let p = Uuid::now_v7();
    // Tightened chk_journal_line_amount: a functional-only line must carry a
    // POSITIVE functional amount (the side carries the sign), so functional 0 fails.
    let lines = vec![
        line_f(Side::Debit, 0, p, Some(0)),
        line_f(Side::Credit, 0, p, Some(0)),
    ];
    assert_eq!(
        validate_balanced_entry("USD", &lines),
        Err(PostingViolation::AmountOutOfRange)
    );
}

#[test]
fn single_currency_entry_skips_functional_check() {
    let p = Uuid::now_v7();
    // f = 0: no functional amounts → the functional check is skipped, so existing
    // single-currency posts are byte-unaffected.
    let lines = vec![line(Side::Debit, 1000, p), line(Side::Credit, 1000, p)];
    assert!(validate_balanced_entry("USD", &lines).is_ok());
}

#[test]
fn cross_currency_functional_balanced_is_ok() {
    let p = Uuid::now_v7();
    // Every line carries a functional amount; BOTH the transaction column
    // (1000 = 1000) and the functional column (1100 = 1100) balance.
    let lines = vec![
        line_f(Side::Debit, 1000, p, Some(1100)),
        line_f(Side::Credit, 1000, p, Some(1100)),
    ];
    assert!(validate_balanced_entry("USD", &lines).is_ok());
}

#[test]
fn functional_unbalanced_is_rejected() {
    let p = Uuid::now_v7();
    // Transaction balances (1000 = 1000) but the functional column does not
    // (1100 != 1090) — the dual-column invariant rejects it.
    let lines = vec![
        line_f(Side::Debit, 1000, p, Some(1100)),
        line_f(Side::Credit, 1000, p, Some(1090)),
    ];
    assert_eq!(
        validate_balanced_entry("USD", &lines),
        Err(PostingViolation::FunctionalUnbalanced)
    );
}

#[test]
fn partial_functional_entry_is_rejected() {
    let p = Uuid::now_v7();
    // 0 < f < len: one line carries functional, the other does not — a posting bug,
    // fail loud rather than silently imbalance the functional column.
    let lines = vec![
        line_f(Side::Debit, 1000, p, Some(1100)),
        line_f(Side::Credit, 1000, p, None),
    ];
    assert_eq!(
        validate_balanced_entry("USD", &lines),
        Err(PostingViolation::FunctionalPartial)
    );
}

#[test]
fn amount_out_of_range_maps_to_its_domain_error() {
    assert!(matches!(
        DomainError::from(PostingViolation::AmountOutOfRange),
        DomainError::AmountOutOfRange(_)
    ));
}

#[test]
fn functional_violations_map_to_unbalanced() {
    assert!(matches!(
        DomainError::from(PostingViolation::FunctionalPartial),
        DomainError::Unbalanced(_)
    ));
    assert!(matches!(
        DomainError::from(PostingViolation::FunctionalUnbalanced),
        DomainError::Unbalanced(_)
    ));
}
