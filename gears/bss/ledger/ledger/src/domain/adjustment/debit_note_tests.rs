//! Tests for the pure debit-note direct-split leg plan (`build_debit_note_legs`)
//! and the request-shape gate (`validate_shape`): the S1-mirror DR AR / CR REVENUE /
//! CR `CONTRACT_LIABILITY` / CR `TAX_PAYABLE` split, fully-recognized (no CL line),
//! with-deferred (CL line), fully-deferred (no REVENUE line), the no-zero-
//! placeholder rule, tax routing, and the balance invariant.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use bss_ledger_sdk::{AccountClass, Side};
use uuid::Uuid;

use super::*;
use crate::domain::error::DomainError;
use crate::domain::invoice::builder::TaxBreakdown;
use crate::domain::recognition::input::{RecognitionInput, RecognitionTiming};

/// A straight-line recognition spec (the deferred-note D4 shape).
fn straight_line() -> RecognitionInput {
    RecognitionInput {
        policy_ref: "policy.sl.v1".to_owned(),
        timing: RecognitionTiming::StraightLine {
            periods: 12,
            first_period_id: None,
        },
        po_allocation_group: Some("grp-1".to_owned()),
        multi_po: false,
        ssp_snapshot_ref: None,
        subscription_ref: Some("sub-1".to_owned()),
        vc_estimate_ref: None,
        vc_method_ref: None,
        immaterial_one_shot_sku: false,
    }
}

/// A baseline debit-note request: incl-tax `amount_minor`, `tax_minor`,
/// `deferred_minor`, one revenue stream. Carries a recognition spec iff it defers.
fn req(amount_minor: i64, tax_minor: i64, deferred_minor: i64) -> DebitNoteRequest {
    DebitNoteRequest {
        tenant_id: Uuid::now_v7(),
        payer_tenant_id: Uuid::now_v7(),
        debit_note_id: "dn-1".to_owned(),
        origin_invoice_id: "inv-1".to_owned(),
        origin_invoice_item_ref: Some("item-1".to_owned()),
        revenue_stream: "SAAS".to_owned(),
        currency: "USD".to_owned(),
        amount_minor,
        tax_minor,
        // A taxed note must carry a breakdown (validate_shape rejects a bare
        // tax_minor); synthesize a single-component breakdown when tax_minor > 0.
        tax: if tax_minor > 0 {
            vec![tax_component(tax_minor, "US-CA", "2026Q2")]
        } else {
            Vec::new()
        },
        deferred_minor,
        reason_code: "ADDITIONAL_USAGE".to_owned(),
        recognition: if deferred_minor > 0 {
            Some(straight_line())
        } else {
            None
        },
    }
}

fn leg(plan: &DebitNoteLegPlan, class: AccountClass, side: Side) -> Option<&PlannedLeg> {
    plan.legs
        .iter()
        .find(|l| l.account_class == class && l.side == side)
}

fn assert_balanced(plan: &DebitNoteLegPlan) {
    let dr: i64 = plan
        .legs
        .iter()
        .filter(|l| l.side == Side::Debit)
        .map(|l| l.amount_minor)
        .sum();
    let cr: i64 = plan
        .legs
        .iter()
        .filter(|l| l.side == Side::Credit)
        .map(|l| l.amount_minor)
        .sum();
    assert_eq!(dr, cr, "plan must balance: {plan:?}");
    // No zero-amount legs (inherited S1 / AC #4).
    assert!(plan.legs.iter().all(|l| l.amount_minor > 0), "no zero legs");
}

#[test]
fn fully_recognized_emits_no_contract_liability_line() {
    // 1000 incl 100 tax ⇒ 900 ex-tax, all recognized (deferred 0). DR AR 1000 / CR
    // REVENUE 900 / CR TAX 100; NO CONTRACT_LIABILITY line.
    let r = req(1000, 100, 0);
    let plan = build_debit_note_legs(&r).unwrap();
    assert_balanced(&plan);

    let ar = leg(&plan, AccountClass::Ar, Side::Debit).expect("ar leg");
    assert_eq!(ar.amount_minor, 1000);
    assert!(ar.revenue_stream.is_none());

    let rev = leg(&plan, AccountClass::Revenue, Side::Credit).expect("revenue leg");
    assert_eq!(rev.amount_minor, 900);
    assert_eq!(rev.revenue_stream.as_deref(), Some("SAAS"));

    let tax = leg(&plan, AccountClass::TaxPayable, Side::Credit).expect("tax leg");
    assert_eq!(tax.amount_minor, 100);

    assert!(
        leg(&plan, AccountClass::ContractLiability, Side::Credit).is_none(),
        "fully-recognized ⇒ no CL line (no zero placeholder)"
    );
    assert_eq!(plan.recognized_part_minor, 900);
    assert_eq!(plan.deferred_part_minor, 0);
}

#[test]
fn with_deferred_splits_revenue_and_contract_liability() {
    // 1200 ex-tax (no tax), 500 deferred ⇒ DR AR 1200 / CR REVENUE 700 / CR CL 500.
    let r = req(1200, 0, 500);
    let plan = build_debit_note_legs(&r).unwrap();
    assert_balanced(&plan);

    let ar = leg(&plan, AccountClass::Ar, Side::Debit).expect("ar");
    assert_eq!(ar.amount_minor, 1200);

    let rev = leg(&plan, AccountClass::Revenue, Side::Credit).expect("revenue");
    assert_eq!(rev.amount_minor, 700, "recognized-now = ex_tax - deferred");
    assert_eq!(rev.revenue_stream.as_deref(), Some("SAAS"));

    let cl = leg(&plan, AccountClass::ContractLiability, Side::Credit).expect("cl");
    assert_eq!(cl.amount_minor, 500, "deferred per PO");
    assert_eq!(cl.revenue_stream.as_deref(), Some("SAAS"));

    assert_eq!(plan.recognized_part_minor, 700);
    assert_eq!(plan.deferred_part_minor, 500);
}

#[test]
fn with_deferred_and_tax_all_four_legs_balance() {
    // 1100 incl 100 tax ⇒ 1000 ex-tax; 400 deferred ⇒ 600 recognized. Four legs:
    // DR AR 1100 / CR REVENUE 600 / CR CL 400 / CR TAX 100.
    let r = req(1100, 100, 400);
    let plan = build_debit_note_legs(&r).unwrap();
    assert_balanced(&plan);
    assert_eq!(plan.legs.len(), 4, "DR AR + CR REVENUE + CR CL + CR TAX");

    assert_eq!(
        leg(&plan, AccountClass::Ar, Side::Debit)
            .unwrap()
            .amount_minor,
        1100
    );
    assert_eq!(
        leg(&plan, AccountClass::Revenue, Side::Credit)
            .unwrap()
            .amount_minor,
        600
    );
    assert_eq!(
        leg(&plan, AccountClass::ContractLiability, Side::Credit)
            .unwrap()
            .amount_minor,
        400
    );
    assert_eq!(
        leg(&plan, AccountClass::TaxPayable, Side::Credit)
            .unwrap()
            .amount_minor,
        100
    );
}

#[test]
fn fully_deferred_emits_no_revenue_line() {
    // 700 ex-tax, all 700 deferred ⇒ DR AR 700 / CR CL 700; NO REVENUE line.
    let r = req(700, 0, 700);
    let plan = build_debit_note_legs(&r).unwrap();
    assert_balanced(&plan);
    assert!(
        leg(&plan, AccountClass::Revenue, Side::Credit).is_none(),
        "no recognized-now part ⇒ no REVENUE line"
    );
    let cl = leg(&plan, AccountClass::ContractLiability, Side::Credit).expect("cl");
    assert_eq!(cl.amount_minor, 700);
    assert_eq!(plan.recognized_part_minor, 0);
    assert_eq!(plan.deferred_part_minor, 700);
}

#[test]
fn zero_deferred_never_emits_a_placeholder_cl_line() {
    // A run of fully-recognized notes (with + without tax) never carries a CL leg.
    for (amount, tax) in [(500_i64, 0_i64), (500, 50), (1, 0)] {
        let plan = build_debit_note_legs(&req(amount, tax, 0)).unwrap();
        assert!(
            leg(&plan, AccountClass::ContractLiability, Side::Credit).is_none(),
            "deferred 0 must never emit a CONTRACT_LIABILITY placeholder ({amount}/{tax})"
        );
        assert_eq!(plan.deferred_part_minor, 0);
    }
}

#[test]
fn no_tax_emits_no_tax_payable_line() {
    let r = req(900, 0, 0);
    let plan = build_debit_note_legs(&r).unwrap();
    assert_balanced(&plan);
    assert!(
        leg(&plan, AccountClass::TaxPayable, Side::Credit).is_none(),
        "zero tax ⇒ no TAX_PAYABLE line"
    );
}

#[test]
fn validate_shape_rejects_tax_over_amount() {
    let r = req(100, 200, 0);
    assert!(matches!(
        validate_shape(&r),
        Err(DomainError::AmountOutOfRange(_))
    ));
}

#[test]
fn validate_shape_rejects_deferred_over_ex_tax() {
    // 100 incl 50 tax ⇒ 50 ex-tax; a 60 deferred request is out of range.
    let mut r = req(100, 50, 0);
    r.deferred_minor = 60;
    r.recognition = Some(straight_line());
    assert!(matches!(
        validate_shape(&r),
        Err(DomainError::AmountOutOfRange(_))
    ));
}

#[test]
fn validate_shape_rejects_empty_reason_code() {
    let mut r = req(100, 0, 0);
    r.reason_code = "  ".to_owned();
    assert!(matches!(
        validate_shape(&r),
        Err(DomainError::InvalidRequest(_))
    ));
}

#[test]
fn validate_shape_rejects_deferred_without_recognition_spec() {
    // A deferring note MUST carry the recognition spec the schedule build needs (D4).
    let mut r = req(1000, 0, 400);
    r.recognition = None;
    assert!(matches!(
        validate_shape(&r),
        Err(DomainError::InvalidRequest(_))
    ));
}

#[test]
fn validate_shape_accepts_fully_recognized_without_recognition_spec() {
    // A fully-recognized note needs no spec.
    let r = req(1000, 100, 0);
    assert!(validate_shape(&r).is_ok());
}

#[test]
fn amount_helpers_compute_ex_tax_and_recognized() {
    let r = req(1100, 100, 400);
    assert_eq!(r.amount_minor_ex_tax(), 1000);
    assert_eq!(r.recognized_minor(), 600);
}

/// One tax component (the authoritative breakdown the caller carries, §4.5).
fn tax_component(amount_minor: i64, jurisdiction: &str, filing: &str) -> TaxBreakdown {
    TaxBreakdown {
        amount_minor,
        currency: "USD".to_owned(),
        tax_jurisdiction: jurisdiction.to_owned(),
        tax_filing_period: filing.to_owned(),
        tax_rate_ref: Some(format!("rate:{jurisdiction}")),
    }
}

#[test]
fn tax_breakdown_emits_per_component_tax_legs() {
    // §4.5: a debit note carrying an authoritative breakdown of two components
    // (different jurisdiction + filing) posts ONE CR TAX_PAYABLE per component, each
    // stamped with its own dims so the projector disaggregates `tax_subbalance` per
    // (jurisdiction, filing). `tax_minor` (the split scalar) == Σ components.
    let mut r = req(1000, 150, 0); // ex_tax = 850, tax = 150 = 100 + 50
    r.tax = vec![
        tax_component(100, "US-CA", "2026Q2"),
        tax_component(50, "US-NY", "2026Q3"),
    ];
    let plan = build_debit_note_legs(&r).unwrap();
    assert_balanced(&plan);

    let tax_legs: Vec<&PlannedLeg> = plan
        .legs
        .iter()
        .filter(|l| l.account_class == AccountClass::TaxPayable && l.side == Side::Credit)
        .collect();
    assert_eq!(tax_legs.len(), 2, "one TAX_PAYABLE leg per component");
    let tax_sum: i64 = tax_legs.iter().map(|l| l.amount_minor).sum();
    assert_eq!(tax_sum, 150, "Σ tax legs == tax_minor");

    let ca = tax_legs
        .iter()
        .find(|l| l.tax_jurisdiction.as_deref() == Some("US-CA"))
        .expect("US-CA tax leg");
    assert_eq!(ca.amount_minor, 100);
    assert_eq!(ca.tax_filing_period.as_deref(), Some("2026Q2"));
    assert_eq!(ca.tax_rate_ref.as_deref(), Some("rate:US-CA"));

    let ny = tax_legs
        .iter()
        .find(|l| l.tax_jurisdiction.as_deref() == Some("US-NY"))
        .expect("US-NY tax leg");
    assert_eq!(ny.amount_minor, 50);
    assert_eq!(ny.tax_filing_period.as_deref(), Some("2026Q3"));
}

#[test]
fn bare_tax_minor_without_breakdown_is_rejected() {
    // A taxed note MUST carry a dimensioned breakdown: a bare `tax_minor` with no
    // breakdown could only build a dimensionless TAX_PAYABLE leg the schema rejects
    // (chk_journal_line_tax_dims), so validate_shape blocks it up front (400).
    // The req helper auto-synthesizes a breakdown when tax_minor > 0; clear it to
    // force the bare-tax_minor case under test.
    let mut r = req(1000, 100, 0);
    r.tax = Vec::new();
    assert!(r.tax.is_empty());
    assert!(
        matches!(validate_shape(&r), Err(DomainError::InvalidRequest(_))),
        "a bare tax_minor with no breakdown must be rejected"
    );
}

#[test]
fn validate_shape_rejects_tax_breakdown_not_summing_to_tax_minor() {
    // A non-empty breakdown is bound to `tax_minor` (the split scalar): a sum that
    // disagrees is a 400 (AMOUNT_OUT_OF_RANGE), never a silent unbalanced post.
    let mut r = req(1000, 150, 0);
    r.tax = vec![tax_component(100, "US-CA", "2026Q2")]; // sums to 100, not 150
    assert!(matches!(
        validate_shape(&r),
        Err(DomainError::AmountOutOfRange(_))
    ));
}

#[test]
fn validate_shape_rejects_tax_breakdown_currency_mismatch() {
    // Every leg posts in the note currency; a component in another currency is a 400.
    let mut r = req(1000, 100, 0);
    let mut wrong = tax_component(100, "US-CA", "2026Q2");
    wrong.currency = "EUR".to_owned();
    r.tax = vec![wrong];
    assert!(matches!(
        validate_shape(&r),
        Err(DomainError::AmountOutOfRange(_))
    ));
}
