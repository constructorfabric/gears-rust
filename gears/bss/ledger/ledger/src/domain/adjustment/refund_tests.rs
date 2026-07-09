//! Tests for the pure refund leg plan (`build_refund_legs`) + the request shape
//! gate (`validate_shape`): the A/B × stage-1/stage-2 × two-stage/single-step
//! routing matrix, the balanced invariant, the never-DR-CONTRACT_LIABILITY
//! guarantee (design §4.4), and the Pattern-B-without-invoice / Pattern-A-with-
//! invoice / single-step-confirmed rejects.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use bss_ledger_sdk::{AccountClass, Side};
use uuid::Uuid;

use super::*;
use crate::domain::error::DomainError;

/// A baseline request for `(pattern, phase, two_stage)` over `amount`. Pattern B
/// carries `inv-1`; Pattern A carries none (the shape `validate_shape` enforces).
fn req(pattern: RefundPattern, phase: RefundPhase, two_stage: bool, amount: i64) -> RefundRequest {
    let invoice_id = match pattern {
        RefundPattern::BRestoreAr => Some("inv-1".to_owned()),
        RefundPattern::AUnallocated => None,
    };
    RefundRequest {
        tenant_id: Uuid::now_v7(),
        payer_tenant_id: Uuid::now_v7(),
        refund_id: "rf-1".to_owned(),
        psp_refund_id: "psp-1".to_owned(),
        phase,
        pattern,
        payment_id: "pay-1".to_owned(),
        invoice_id,
        currency: "USD".to_owned(),
        amount_minor: amount,
        two_stage,
        // Default to a first-order OUTBOUND refund; the refund-of-refund tests
        // override `direction` + `relates_to_refund_id` explicitly.
        relates_to_refund_id: None,
        direction: RefundDirection::Outbound,
    }
}

fn assert_balanced(plan: &RefundLegPlan) {
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
    assert!(plan.legs.iter().all(|l| l.amount_minor > 0), "no zero legs");
    // The §4.4 invariant: a refund NEVER debits CONTRACT_LIABILITY.
    assert!(
        plan.legs
            .iter()
            .all(|l| l.account_class != AccountClass::ContractLiability),
        "refund must never touch CONTRACT_LIABILITY: {plan:?}"
    );
}

/// The (single) debit leg's class.
fn debit_class(plan: &RefundLegPlan) -> AccountClass {
    plan.legs
        .iter()
        .find(|l| l.side == Side::Debit)
        .expect("a debit leg")
        .account_class
}

/// The (single) credit leg's class.
fn credit_class(plan: &RefundLegPlan) -> AccountClass {
    plan.legs
        .iter()
        .find(|l| l.side == Side::Credit)
        .expect("a credit leg")
        .account_class
}

// --- Pattern A (A_UNALLOCATED) ---

#[test]
fn pattern_a_stage1_two_stage_unallocated_to_clearing() {
    let r = req(
        RefundPattern::AUnallocated,
        RefundPhase::Initiated,
        true,
        500,
    );
    let plan = build_refund_legs(&r).unwrap();
    assert_balanced(&plan);
    assert_eq!(debit_class(&plan), AccountClass::Unallocated);
    assert_eq!(credit_class(&plan), AccountClass::RefundClearing);
    assert_eq!(plan.clearing_state, CLEARING_STATE_PENDING);
    assert_eq!(plan.legs.len(), 2);
}

#[test]
fn pattern_a_stage2_clearing_to_cash() {
    let r = req(
        RefundPattern::AUnallocated,
        RefundPhase::Confirmed,
        true,
        500,
    );
    let plan = build_refund_legs(&r).unwrap();
    assert_balanced(&plan);
    assert_eq!(debit_class(&plan), AccountClass::RefundClearing);
    assert_eq!(credit_class(&plan), AccountClass::CashClearing);
    assert_eq!(plan.clearing_state, CLEARING_STATE_SETTLED);
}

#[test]
fn pattern_a_single_step_unallocated_to_cash() {
    let r = req(
        RefundPattern::AUnallocated,
        RefundPhase::Initiated,
        false,
        500,
    );
    let plan = build_refund_legs(&r).unwrap();
    assert_balanced(&plan);
    // Single-step skips REFUND_CLEARING entirely (D1).
    assert_eq!(debit_class(&plan), AccountClass::Unallocated);
    assert_eq!(credit_class(&plan), AccountClass::CashClearing);
    assert_eq!(plan.clearing_state, CLEARING_STATE_SETTLED);
    assert!(
        plan.legs
            .iter()
            .all(|l| l.account_class != AccountClass::RefundClearing),
        "single-step has no REFUND_CLEARING leg"
    );
}

// --- Pattern B (B_RESTORE_AR) ---

#[test]
fn pattern_b_stage1_two_stage_ar_to_clearing() {
    let r = req(RefundPattern::BRestoreAr, RefundPhase::Initiated, true, 800);
    let plan = build_refund_legs(&r).unwrap();
    assert_balanced(&plan);
    // Pattern B restores AR (re-opens the receivable).
    assert_eq!(debit_class(&plan), AccountClass::Ar);
    assert_eq!(credit_class(&plan), AccountClass::RefundClearing);
    assert_eq!(plan.clearing_state, CLEARING_STATE_PENDING);
}

#[test]
fn pattern_b_stage2_clearing_to_cash() {
    let r = req(RefundPattern::BRestoreAr, RefundPhase::Confirmed, true, 800);
    let plan = build_refund_legs(&r).unwrap();
    assert_balanced(&plan);
    assert_eq!(debit_class(&plan), AccountClass::RefundClearing);
    assert_eq!(credit_class(&plan), AccountClass::CashClearing);
    assert_eq!(plan.clearing_state, CLEARING_STATE_SETTLED);
}

#[test]
fn pattern_b_single_step_ar_to_cash() {
    let r = req(
        RefundPattern::BRestoreAr,
        RefundPhase::Initiated,
        false,
        800,
    );
    let plan = build_refund_legs(&r).unwrap();
    assert_balanced(&plan);
    assert_eq!(debit_class(&plan), AccountClass::Ar);
    assert_eq!(credit_class(&plan), AccountClass::CashClearing);
    assert_eq!(plan.clearing_state, CLEARING_STATE_SETTLED);
}

// --- The two-stage clearing round-trip drains to zero (the integration check,
//     proven here on the pure legs: stage-1 CR REFUND_CLEARING == stage-2 DR
//     REFUND_CLEARING, so the clearing nets to 0 across both stages). ---

#[test]
fn two_stage_clearing_credit_then_debit_nets_to_zero() {
    let amount = 1234;
    let s1 = build_refund_legs(&req(
        RefundPattern::AUnallocated,
        RefundPhase::Initiated,
        true,
        amount,
    ))
    .unwrap();
    let s2 = build_refund_legs(&req(
        RefundPattern::AUnallocated,
        RefundPhase::Confirmed,
        true,
        amount,
    ))
    .unwrap();
    let clearing_delta = |plan: &RefundLegPlan| -> i64 {
        plan.legs
            .iter()
            .filter(|l| l.account_class == AccountClass::RefundClearing)
            .map(|l| match l.side {
                // CR a credit-normal clearing liability raises it; DR drains it.
                Side::Credit => l.amount_minor,
                Side::Debit => -l.amount_minor,
            })
            .sum()
    };
    assert_eq!(clearing_delta(&s1), amount, "stage-1 opens the clearing");
    assert_eq!(clearing_delta(&s2), -amount, "stage-2 drains the clearing");
    assert_eq!(
        clearing_delta(&s1) + clearing_delta(&s2),
        0,
        "REFUND_CLEARING nets to zero across both stages"
    );
}

// --- validate_shape ---

#[test]
fn validate_shape_rejects_pattern_b_without_invoice() {
    let mut r = req(RefundPattern::BRestoreAr, RefundPhase::Initiated, true, 100);
    r.invoice_id = None;
    assert!(matches!(
        validate_shape(&r),
        Err(DomainError::InvalidRequest(_))
    ));
}

#[test]
fn validate_shape_rejects_pattern_b_with_blank_invoice() {
    let mut r = req(RefundPattern::BRestoreAr, RefundPhase::Initiated, true, 100);
    r.invoice_id = Some("  ".to_owned());
    assert!(matches!(
        validate_shape(&r),
        Err(DomainError::InvalidRequest(_))
    ));
}

#[test]
fn validate_shape_rejects_pattern_a_with_invoice() {
    let mut r = req(
        RefundPattern::AUnallocated,
        RefundPhase::Initiated,
        true,
        100,
    );
    r.invoice_id = Some("inv-x".to_owned());
    assert!(matches!(
        validate_shape(&r),
        Err(DomainError::InvalidRequest(_))
    ));
}

#[test]
fn validate_shape_rejects_negative_amount() {
    let r = req(
        RefundPattern::AUnallocated,
        RefundPhase::Initiated,
        true,
        -1,
    );
    assert!(matches!(
        validate_shape(&r),
        Err(DomainError::AmountOutOfRange(_))
    ));
}

#[test]
fn validate_shape_rejects_single_step_confirmed() {
    // A single-step refund has no separate confirmed stage.
    let r = req(
        RefundPattern::AUnallocated,
        RefundPhase::Confirmed,
        false,
        100,
    );
    assert!(matches!(
        validate_shape(&r),
        Err(DomainError::InvalidRequest(_))
    ));
}

#[test]
fn validate_shape_accepts_zero_amount() {
    // A zero-amount refund passes the shape gate (amount >= 0); the HANDLER rejects
    // it up-front before the empty-entry engine check — not validate_shape's job.
    let r = req(RefundPattern::AUnallocated, RefundPhase::Initiated, true, 0);
    assert!(validate_shape(&r).is_ok());
}

// --- build_refund_legs rejects terminal / invalid combos ---

#[test]
fn build_rejects_single_step_confirmed() {
    let r = req(
        RefundPattern::AUnallocated,
        RefundPhase::Confirmed,
        false,
        100,
    );
    assert!(matches!(
        build_refund_legs(&r),
        Err(DomainError::InvalidRequest(_))
    ));
}

#[test]
fn build_rejects_terminal_phases() {
    for phase in [
        RefundPhase::Rejected,
        RefundPhase::Voided,
        RefundPhase::UnknownFinal,
    ] {
        let r = req(RefundPattern::AUnallocated, phase, true, 100);
        assert!(
            matches!(build_refund_legs(&r), Err(DomainError::InvalidRequest(_))),
            "phase {phase:?} has no Group-B posting shape"
        );
    }
}

// --- wire mapping (as_str) round-trips the CHECK literals ---

#[test]
fn phase_and_pattern_as_str_match_check_literals() {
    assert_eq!(RefundPhase::Initiated.as_str(), "initiated");
    assert_eq!(RefundPhase::Confirmed.as_str(), "confirmed");
    assert_eq!(RefundPhase::Rejected.as_str(), "rejected");
    assert_eq!(RefundPhase::Voided.as_str(), "voided");
    assert_eq!(RefundPhase::UnknownFinal.as_str(), "unknown_final");
    assert_eq!(RefundPattern::AUnallocated.as_str(), "A_UNALLOCATED");
    assert_eq!(RefundPattern::BRestoreAr.as_str(), "B_RESTORE_AR");
}

// --- refund-of-refund: claw-back vs outbound legs + direction shape (Group E) ---

/// Turn an outbound request into a refund-of-refund CLAW-BACK (sets the prior-refund
/// link + the `Clawback` direction).
fn clawback(mut r: RefundRequest) -> RefundRequest {
    r.relates_to_refund_id = Some("rf-origin".to_owned());
    r.direction = RefundDirection::Clawback;
    r
}

#[test]
fn direction_default_is_clawback() {
    // D8: the canonical default for a refund-of-refund is claw-back/decrement.
    assert_eq!(RefundDirection::default(), RefundDirection::Clawback);
}

#[test]
fn direction_as_str_round_trips() {
    for d in [RefundDirection::Outbound, RefundDirection::Clawback] {
        assert_eq!(RefundDirection::parse(d.as_str()), Some(d));
    }
    assert_eq!(RefundDirection::Outbound.as_str(), "OUTBOUND");
    assert_eq!(RefundDirection::Clawback.as_str(), "CLAWBACK");
    assert_eq!(RefundDirection::parse("NOPE"), None);
}

#[test]
fn is_clawback_requires_link_and_direction() {
    // Clawback direction WITHOUT a link is not a claw-back (and is rejected by
    // validate_shape) — both are required.
    let mut r = req(
        RefundPattern::AUnallocated,
        RefundPhase::Initiated,
        true,
        100,
    );
    assert!(!r.is_clawback(), "outbound first-order is not a claw-back");
    r.direction = RefundDirection::Clawback;
    assert!(
        !r.is_clawback(),
        "clawback direction with no link is not a claw-back"
    );
    r.relates_to_refund_id = Some("rf-origin".to_owned());
    assert!(r.is_clawback(), "clawback direction + link IS a claw-back");
}

#[test]
fn validate_shape_requires_link_for_clawback() {
    let mut r = req(
        RefundPattern::AUnallocated,
        RefundPhase::Initiated,
        true,
        100,
    );
    r.direction = RefundDirection::Clawback; // no relates_to_refund_id
    assert!(matches!(
        validate_shape(&r),
        Err(DomainError::InvalidRequest(_))
    ));
    // With the link it validates.
    r.relates_to_refund_id = Some("rf-origin".to_owned());
    assert!(validate_shape(&r).is_ok());
}

#[test]
fn validate_shape_outbound_refund_of_refund_needs_no_link() {
    // An OUTBOUND refund-of-refund (cash out again) may carry the link but does not
    // require it — only a CLAWBACK does.
    let mut r = req(
        RefundPattern::AUnallocated,
        RefundPhase::Initiated,
        true,
        100,
    );
    r.direction = RefundDirection::Outbound;
    r.relates_to_refund_id = Some("rf-origin".to_owned());
    assert!(validate_shape(&r).is_ok());
}

/// Stage-1 two-stage claw-back is the side-flip of the outbound stage-1: same two
/// accounts, DR/CR swapped (`REFUND_CLEARING` drains the other way; the pattern debit
/// is RESTORED via the credit leg). Asserted for both patterns.
#[test]
fn clawback_stage1_inverts_outbound_legs() {
    for pattern in [RefundPattern::AUnallocated, RefundPattern::BRestoreAr] {
        let out = build_refund_legs(&req(pattern, RefundPhase::Initiated, true, 500)).unwrap();
        let cb =
            build_refund_legs(&clawback(req(pattern, RefundPhase::Initiated, true, 500))).unwrap();
        assert_balanced(&cb);
        // Outbound stage-1: DR pattern.debit · CR REFUND_CLEARING.
        assert_eq!(debit_class(&out), pattern.debit_class());
        assert_eq!(credit_class(&out), AccountClass::RefundClearing);
        // Claw-back stage-1: DR REFUND_CLEARING · CR pattern.debit (restores it).
        assert_eq!(debit_class(&cb), AccountClass::RefundClearing);
        assert_eq!(credit_class(&cb), pattern.debit_class());
        // Same clearing_state (PENDING) — only the sides differ.
        assert_eq!(cb.clearing_state, CLEARING_STATE_PENDING);
    }
}

#[test]
fn clawback_stage2_pulls_cash_back_in() {
    // Outbound stage-2 drains REFUND_CLEARING → CASH_CLEARING (cash out); the
    // claw-back stage-2 is the inverse: DR CASH_CLEARING · CR REFUND_CLEARING.
    let cb = build_refund_legs(&clawback(req(
        RefundPattern::AUnallocated,
        RefundPhase::Confirmed,
        true,
        500,
    )))
    .unwrap();
    assert_balanced(&cb);
    assert_eq!(debit_class(&cb), AccountClass::CashClearing);
    assert_eq!(credit_class(&cb), AccountClass::RefundClearing);
    assert_eq!(cb.clearing_state, CLEARING_STATE_SETTLED);
}

#[test]
fn clawback_single_step_restores_pattern_debit_from_cash() {
    // Single-step outbound: DR pattern.debit · CR CASH_CLEARING. Single-step
    // claw-back: DR CASH_CLEARING · CR pattern.debit (no REFUND_CLEARING leg).
    let cb = build_refund_legs(&clawback(req(
        RefundPattern::BRestoreAr,
        RefundPhase::Initiated,
        false,
        800,
    )))
    .unwrap();
    assert_balanced(&cb);
    assert_eq!(debit_class(&cb), AccountClass::CashClearing);
    assert_eq!(credit_class(&cb), AccountClass::Ar);
    assert!(
        cb.legs
            .iter()
            .all(|l| l.account_class != AccountClass::RefundClearing),
        "single-step claw-back has no REFUND_CLEARING leg"
    );
}
