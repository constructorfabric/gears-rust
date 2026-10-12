//! Tests for the pure credit-note leg plan (`build_credit_note_legs`) + the
//! request shape gate (`validate_shape`): contra-vs-goodwill debit selection,
//! with/without a deferred part (per-stream `CONTRACT_LIABILITY` + `schedule_id`),
//! tax reversal, the open-AR cap, the paid-invoice wallet remainder (K-2), and the
//! balance invariant.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use bss_ledger_sdk::{AccountClass, Side};
use uuid::Uuid;

use super::*;
use bss_ledger_sdk::money::{CurrencySpec, PostedMoney};
use rust_decimal::Decimal;
/// Original USD fixtures expressed economically at their real scale of two.
fn m(cents: i64) -> PostedMoney {
    PostedMoney::try_new(
        Decimal::new(cents, 2),
        CurrencySpec::try_new("USD".to_owned(), 2).unwrap(),
    )
    .unwrap()
}

use crate::domain::adjustment::splitter::{SplitResult, StreamSplit};
use crate::domain::error::DomainError;
use crate::domain::invoice::builder::TaxBreakdown;

/// A baseline non-goodwill request: incl-tax `amount`, `tax`,
/// `requested_deferred`, one revenue stream.
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
        // A taxed note must carry a breakdown (validate_shape rejects a bare
        // tax); synthesize a single-component breakdown when tax > 0.
        tax: if tax > 0 {
            vec![tax_component(tax, "US-CA", "2026Q2")]
        } else {
            Vec::new()
        },
        requested_deferred: m(requested_deferred),
        reason_code: "CUSTOMER_GOODWILL".to_owned(),
        goodwill: false,
    }
}

/// A split result over `ex_tax`, with `recognized`/`deferred` parts placed on a
/// single stream (the common single-obligation case).
fn split_single(stream: &str, schedule_id: &str, recognized: i64, deferred: i64) -> SplitResult {
    SplitResult {
        recognized_part: m(recognized),
        deferred_part: m(deferred),
        per_stream: vec![StreamSplit {
            revenue_stream: stream.to_owned(),
            schedule_id: schedule_id.to_owned(),
            recognized_part: m(recognized),
            deferred_part: m(deferred),
        }],
        split_basis_ref: "item=item-1;po=po-1;streams=[SAAS:sch-1@v1:def=...]".to_owned(),
    }
}

/// A wholly-recognized split with no streams (a fully point-in-time line).
fn split_recognized_no_streams(recognized: i64) -> SplitResult {
    SplitResult {
        recognized_part: m(recognized),
        deferred_part: m(0),
        per_stream: vec![],
        split_basis_ref: "item=item-1;po=po-1;streams=none".to_owned(),
    }
}

fn leg(plan: &CreditNoteLegPlan, class: AccountClass, side: Side) -> Option<&PlannedLeg> {
    plan.legs
        .iter()
        .find(|l| l.account_class == class && l.side == side)
}

fn assert_balanced(plan: &CreditNoteLegPlan) {
    let dr = sum_posted(
        &plan
            .legs
            .iter()
            .filter(|l| l.side == Side::Debit)
            .map(|l| l.amount.clone())
            .collect::<Vec<_>>(),
        m(0).currency().clone(),
    )
    .unwrap();
    let cr = sum_posted(
        &plan
            .legs
            .iter()
            .filter(|l| l.side == Side::Credit)
            .map(|l| l.amount.clone())
            .collect::<Vec<_>>(),
        m(0).currency().clone(),
    )
    .unwrap();
    assert_eq!(dr, cr, "plan must balance");
    // No zero-amount legs (inherited S1 / AC #4).
    assert!(
        plan.legs.iter().all(|l| l.amount.amount() > Decimal::ZERO),
        "no zero legs"
    );
}

#[test]
fn contra_only_fully_recognized_open_ar_covers() {
    // 1000 incl 100 tax ⇒ 900 ex-tax, all recognized. Open AR covers the full 1000.
    let r = req(1000, 100, 0);
    let split = split_recognized_no_streams(900);
    let plan = build_credit_note_legs(&r, &split, m(1000)).unwrap();
    assert_balanced(&plan);

    let contra = leg(&plan, AccountClass::ContraRevenue, Side::Debit).expect("contra leg");
    assert_eq!(contra.amount, m(900));
    assert_eq!(contra.revenue_stream.as_deref(), Some("SAAS"));
    assert!(leg(&plan, AccountClass::ContractLiability, Side::Debit).is_none());

    let tax = leg(&plan, AccountClass::TaxPayable, Side::Debit).expect("tax leg");
    assert_eq!(tax.amount, m(100));

    let ar = leg(&plan, AccountClass::Ar, Side::Credit).expect("ar leg");
    assert_eq!(ar.amount, m(1000));
    assert!(leg(&plan, AccountClass::ReusableCredit, Side::Credit).is_none());
    assert_eq!(plan.wallet_remainder, m(0));
    assert_eq!(plan.recognized_part, m(900));
    assert_eq!(plan.deferred_part, m(0));
}

#[test]
fn contra_plus_deferred_carries_schedule_id_per_stream() {
    // 900 ex-tax: 400 recognized + 500 deferred on stream SAAS / schedule sch-9.
    let r = req(900, 0, 500);
    let split = split_single("SAAS", "sch-9", 400, 500);
    let plan = build_credit_note_legs(&r, &split, m(900)).unwrap();
    assert_balanced(&plan);

    let contra = leg(&plan, AccountClass::ContraRevenue, Side::Debit).expect("contra");
    assert_eq!(contra.amount, m(400));

    let cl = leg(&plan, AccountClass::ContractLiability, Side::Debit).expect("cl");
    assert_eq!(cl.amount, m(500));
    assert_eq!(cl.revenue_stream.as_deref(), Some("SAAS"));
    // The CL leg carries the owning schedule_id so the handler reduces the right
    // schedule (§4.5) — this is the load-bearing wiring for the schedule reduction.
    assert_eq!(cl.schedule_id.as_deref(), Some("sch-9"));

    assert_eq!(plan.deferred_part, m(500));
}

#[test]
fn fully_deferred_emits_no_contra_leg() {
    let r = req(700, 0, 700);
    let split = split_single("SAAS", "sch-2", 0, 700);
    let plan = build_credit_note_legs(&r, &split, m(700)).unwrap();
    assert_balanced(&plan);
    assert!(leg(&plan, AccountClass::ContraRevenue, Side::Debit).is_none());
    let cl = leg(&plan, AccountClass::ContractLiability, Side::Debit).expect("cl");
    assert_eq!(cl.amount, m(700));
}

#[test]
fn paid_invoice_remainder_goes_to_reusable_credit_wallet() {
    // Note 1000 incl 100 tax; the invoice has only 300 open AR (mostly paid). The
    // 700 remainder seeds the reusable-credit wallet (K-2).
    let r = req(1000, 100, 0);
    let split = split_recognized_no_streams(900);
    let plan = build_credit_note_legs(&r, &split, m(300)).unwrap();
    assert_balanced(&plan);

    let ar = leg(&plan, AccountClass::Ar, Side::Credit).expect("ar");
    assert_eq!(ar.amount, m(300));
    let wallet = leg(&plan, AccountClass::ReusableCredit, Side::Credit).expect("wallet");
    assert_eq!(wallet.amount, m(700));
    assert_eq!(
        wallet.credit_grant_event_type.as_deref(),
        Some(CREDIT_GRANT_EVENT_TYPE_CREDIT_NOTE)
    );
    assert_eq!(plan.wallet_remainder, m(700));
    assert_eq!(plan.ar_credit, m(300));
}

#[test]
fn fully_paid_invoice_whole_note_goes_to_wallet() {
    // Zero open AR ⇒ no AR leg; the whole note seeds the wallet.
    let r = req(500, 0, 0);
    let split = split_recognized_no_streams(500);
    let plan = build_credit_note_legs(&r, &split, m(0)).unwrap();
    assert_balanced(&plan);
    assert!(leg(&plan, AccountClass::Ar, Side::Credit).is_none());
    let wallet = leg(&plan, AccountClass::ReusableCredit, Side::Credit).expect("wallet");
    assert_eq!(wallet.amount, m(500));
}

#[test]
fn goodwill_uses_goodwill_class_not_contra_and_no_schedule() {
    // C4: goodwill ⇒ DR GOODWILL for the full ex-tax, no CONTRA_REVENUE, no CL.
    let mut r = req(400, 0, 0);
    r.goodwill = true;
    let split = split_recognized_no_streams(400);
    let plan = build_credit_note_legs(&r, &split, m(400)).unwrap();
    assert_balanced(&plan);

    let goodwill = leg(&plan, AccountClass::Goodwill, Side::Debit).expect("goodwill");
    assert_eq!(goodwill.amount, m(400));
    assert!(goodwill.revenue_stream.is_none());
    assert!(goodwill.schedule_id.is_none());
    assert!(leg(&plan, AccountClass::ContraRevenue, Side::Debit).is_none());
    assert!(leg(&plan, AccountClass::ContractLiability, Side::Debit).is_none());
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
    let r = req(100, 50, 60);
    assert!(matches!(
        validate_shape(&r),
        Err(DomainError::AmountOutOfRange(_))
    ));
}

#[test]
fn validate_shape_rejects_goodwill_with_deferred() {
    let mut r = req(400, 0, 100);
    r.goodwill = true;
    assert!(matches!(
        validate_shape(&r),
        Err(DomainError::InvalidRequest(_))
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
fn build_rejects_split_not_matching_ex_tax() {
    // Defensive invariant: a split that does not net to ex_tax is an Internal
    // breach (the handler must feed the splitter the request's ex-tax amount).
    let r = req(1000, 100, 0); // ex_tax = 900
    let split = split_recognized_no_streams(800); // wrong total
    assert!(matches!(
        build_credit_note_legs(&r, &split, m(1000)),
        Err(DomainError::Internal(_))
    ));
}

#[test]
fn build_rejects_negative_open_ar() {
    let r = req(100, 0, 0);
    let split = split_recognized_no_streams(100);
    assert!(matches!(
        build_credit_note_legs(&r, &split, m(-1)),
        Err(DomainError::Internal(_))
    ));
}

/// One tax component (the authoritative breakdown the caller carries, §4.5).
fn tax_component(amount: i64, jurisdiction: &str, filing: &str) -> TaxBreakdown {
    TaxBreakdown {
        amount: m(amount),
        tax_jurisdiction: jurisdiction.to_owned(),
        tax_filing_period: filing.to_owned(),
        tax_rate_ref: Some(format!("rate:{jurisdiction}")),
    }
}

#[test]
fn tax_breakdown_emits_per_component_tax_legs() {
    // §4.5: a credit note carrying an authoritative breakdown of two components
    // (different jurisdiction + filing) reverses ONE DR TAX_PAYABLE per component,
    // each stamped with its own dims so the projector disaggregates `tax_subbalance`
    // per (jurisdiction, filing). `tax` (the split scalar) == Σ components.
    let mut r = req(1000, 150, 0); // ex_tax = 850, tax = 150 = 100 + 50
    r.tax = vec![
        tax_component(100, "US-CA", "2026Q2"),
        tax_component(50, "US-NY", "2026Q2"),
    ];
    let split = split_recognized_no_streams(850);
    let plan = build_credit_note_legs(&r, &split, m(1000)).unwrap();
    assert_balanced(&plan);

    let tax_legs: Vec<&PlannedLeg> = plan
        .legs
        .iter()
        .filter(|l| l.account_class == AccountClass::TaxPayable && l.side == Side::Debit)
        .collect();
    assert_eq!(tax_legs.len(), 2, "one TAX_PAYABLE leg per component");
    // Σ of the per-component tax legs == tax (the plan stays balanced).
    let tax_sum = sum_posted(
        &tax_legs
            .iter()
            .map(|l| l.amount.clone())
            .collect::<Vec<_>>(),
        m(0).currency().clone(),
    )
    .unwrap();
    assert_eq!(tax_sum, m(150), "Σ tax legs == tax");

    let ca = tax_legs
        .iter()
        .find(|l| l.tax_jurisdiction.as_deref() == Some("US-CA"))
        .expect("US-CA tax leg");
    assert_eq!(ca.amount, m(100));
    assert_eq!(ca.tax_filing_period.as_deref(), Some("2026Q2"));
    assert_eq!(ca.tax_rate_ref.as_deref(), Some("rate:US-CA"));

    let ny = tax_legs
        .iter()
        .find(|l| l.tax_jurisdiction.as_deref() == Some("US-NY"))
        .expect("US-NY tax leg");
    assert_eq!(ny.amount, m(50));
    assert_eq!(ny.tax_filing_period.as_deref(), Some("2026Q2"));
}

#[test]
fn bare_tax_without_breakdown_is_rejected() {
    // A taxed note MUST carry a dimensioned breakdown: a bare `tax` with no
    // breakdown could only build a dimensionless TAX_PAYABLE leg the schema rejects
    // (chk_journal_line_tax_dims), so validate_shape blocks it up front (400).
    // The req helper auto-synthesizes a breakdown when tax > 0; clear it to
    // force the bare-tax case under test.
    let mut r = req(1000, 100, 0);
    r.tax = Vec::new();
    assert!(r.tax.is_empty());
    assert!(
        matches!(validate_shape(&r), Err(DomainError::InvalidRequest(_))),
        "a bare tax with no breakdown must be rejected"
    );
}

#[test]
fn validate_shape_rejects_tax_breakdown_not_summing_to_tax() {
    // A non-empty breakdown is bound to `tax` (the split scalar): a sum that
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
    wrong.amount = PostedMoney::try_new(
        wrong.amount.amount(),
        CurrencySpec::try_new("EUR".to_owned(), 2).unwrap(),
    )
    .unwrap();
    r.tax = vec![wrong];
    assert!(matches!(
        validate_shape(&r),
        Err(DomainError::CurrencyMismatch(_))
    ));
}

#[test]
fn fractional_tax_ar_wallet_and_caller_corrections_are_exact() {
    let r = req(1234, 34, 500);
    let split = split_single("SAAS", "sch-1", 700, 500);
    let plan = build_credit_note_legs(&r, &split, m(1001)).unwrap();
    assert_eq!(r.amount_ex_tax().unwrap(), m(1200));
    assert_eq!(plan.recognized_part, m(700));
    assert_eq!(plan.deferred_part, m(500));
    assert_eq!(plan.ar_credit, m(1001));
    assert_eq!(plan.wallet_remainder, m(233));
    assert_balanced(&plan);
    for cents in [1, 2] {
        let correction = req(cents, 0, 0);
        let plan =
            build_credit_note_legs(&correction, &split_recognized_no_streams(cents), m(cents))
                .unwrap();
        assert_eq!(plan.legs[0].amount, m(cents));
        assert_eq!(plan.legs[0].side, Side::Debit);
        assert_eq!(plan.legs[1].side, Side::Credit);
    }
}
#[test]
fn credit_zero_metadata_checked_in_every_comparand() {
    for (code, scale) in [("EUR", 2), ("USD", 3)] {
        let wrong = PostedMoney::try_new(
            Decimal::ZERO,
            CurrencySpec::try_new(code.to_owned(), scale).unwrap(),
        )
        .unwrap();
        let named = |error: DomainError| {
            if code == "EUR" {
                assert!(matches!(error, DomainError::CurrencyMismatch(_)));
            } else {
                assert!(matches!(error, DomainError::InconsistentScale(_)));
            }
        };
        let mut r = req(0, 0, 0);
        r.tax_amount = wrong.clone();
        named(r.amount_ex_tax().unwrap_err());
        r = req(0, 0, 0);
        r.requested_deferred = wrong.clone();
        named(validate_shape(&r).unwrap_err());
        r = req(0, 0, 0);
        let mut tax = tax_component(0, "US-CA", "2026Q2");
        tax.amount = wrong.clone();
        r.tax = vec![tax];
        named(validate_shape(&r).unwrap_err());
        r = req(0, 0, 0);
        let mut split = split_recognized_no_streams(0);
        split.recognized_part = wrong.clone();
        named(build_credit_note_legs(&r, &split, m(0)).unwrap_err());
        split = split_single("SAAS", "sch-1", 0, 0);
        split.per_stream[0].deferred_part = wrong.clone();
        named(build_credit_note_legs(&r, &split, m(0)).unwrap_err());
        named(build_credit_note_legs(&r, &split_recognized_no_streams(0), wrong).unwrap_err());
    }
}
#[test]
fn goodwill_fractional_over_ar_and_inconsistent_split_fail() {
    let mut r = req(1234, 34, 0);
    r.goodwill = true;
    assert!(matches!(
        build_credit_note_legs(&r, &split_recognized_no_streams(1200), m(1233)),
        Err(DomainError::InvalidRequest(_))
    ));
    let r = req(1234, 34, 500);
    let mut split = split_single("SAAS", "sch-1", 700, 500);
    split.per_stream[0].deferred_part = m(499);
    assert!(matches!(
        build_credit_note_legs(&r, &split, m(1234)),
        Err(DomainError::Internal(_))
    ));
    split = split_single("SAAS", "sch-1", 800, 400);
    assert!(matches!(
        build_credit_note_legs(&r, &split, m(1234)),
        Err(DomainError::Internal(_))
    ));
}
#[test]
fn credit_final_tax_and_split_overflow_are_named() {
    let max = PostedMoney::try_new(
        "9999999999999999999999999999".parse().unwrap(),
        m(0).currency().clone(),
    )
    .unwrap();
    let mut r = req(0, 0, 0);
    r.amount = max.clone();
    r.tax_amount = max.clone();
    let mut tax = tax_component(0, "US-CA", "2026Q2");
    tax.amount = max.clone();
    r.tax = vec![tax.clone(), tax];
    assert!(matches!(
        validate_shape(&r),
        Err(DomainError::AmountOutOfRange(_))
    ));
    r.tax_amount = m(0);
    r.tax = vec![];
    let mut split = split_recognized_no_streams(0);
    split.recognized_part = max.clone();
    split.deferred_part = max.clone();
    assert!(matches!(
        build_credit_note_legs(&r, &split, max.clone()),
        Err(DomainError::AmountOutOfRange(_))
    ));
    let mut split = split_recognized_no_streams(0);
    split.recognized_part = max.clone();
    assert_eq!(
        build_credit_note_legs(&r, &split, max.clone())
            .unwrap()
            .ar_credit,
        max
    );
}
