//! Tests for the pure governed manual-adjustment gate ([`govern`]) + the
//! [`ManualAdjustmentAction`] token round-trip: the code-owned allow-list, the
//! mandatory-reason / shape / balance checks, the global `REVENUE`/`CONTRACT_LIABILITY`
//! ban, and the write-off structural guard (a bare `CONTRA_REVENUE` leg ⇒
//! `AttemptedWriteOff`, design §4.6).

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use bss_ledger_sdk::{AccountClass, Side};
use uuid::Uuid;

use super::*;

/// A balanced manual-adjustment request from an explicit leg list + action.
fn req(action: ManualAdjustmentAction, legs: Vec<ManualLeg>) -> ManualAdjustmentRequest {
    ManualAdjustmentRequest {
        tenant_id: Uuid::now_v7(),
        payer_tenant_id: Some(Uuid::now_v7()),
        adjustment_id: "adj-1".to_owned(),
        action,
        currency: "USD".to_owned(),
        legs,
        reason_code: "ROUNDING_RESIDUE".to_owned(),
        preparer_actor_id: Uuid::now_v7(),
        approver_actor_id: None,
        tax: Vec::new(),
    }
}

/// A leg helper — stream-less unless overridden via [`leg_stream`].
fn leg(account_class: AccountClass, side: Side, amount_minor: i64) -> ManualLeg {
    ManualLeg {
        account_class,
        side,
        amount_minor,
        revenue_stream: None,
    }
}

/// A per-stream leg helper.
fn leg_stream(
    account_class: AccountClass,
    side: Side,
    amount_minor: i64,
    stream: &str,
) -> ManualLeg {
    ManualLeg {
        account_class,
        side,
        amount_minor,
        revenue_stream: Some(stream.to_owned()),
    }
}

#[test]
fn rounding_correction_within_allow_list_is_ok() {
    // A balanced Suspense ⇄ CashClearing move — both in the RoundingCorrection
    // allow-list.
    let r = req(
        ManualAdjustmentAction::RoundingCorrection,
        vec![
            leg(AccountClass::Suspense, Side::Debit, 1),
            leg(AccountClass::CashClearing, Side::Credit, 1),
        ],
    );
    assert_eq!(govern(&r), Ok(()));
}

#[test]
fn class_outside_allow_list_is_not_allowed() {
    // TAX_PAYABLE is in no RoundingCorrection allow-list — and it is neither
    // REVENUE/CONTRACT_LIABILITY (step 4) nor CONTRA_REVENUE (step 5), so it falls
    // through to the allow-list check (step 6).
    let r = req(
        ManualAdjustmentAction::RoundingCorrection,
        vec![
            leg(AccountClass::TaxPayable, Side::Debit, 1),
            leg(AccountClass::Suspense, Side::Credit, 1),
        ],
    );
    match govern(&r) {
        Err(ManualAdjustmentReject::NotAllowed(m)) => assert!(m.contains("allow-list"), "{m}"),
        other => panic!("expected NotAllowed(allow-list), got {other:?}"),
    }
}

#[test]
fn unbalanced_legs_rejected() {
    let r = req(
        ManualAdjustmentAction::RoundingCorrection,
        vec![
            leg(AccountClass::Suspense, Side::Debit, 5),
            leg(AccountClass::CashClearing, Side::Credit, 4),
        ],
    );
    match govern(&r) {
        Err(ManualAdjustmentReject::NotAllowed(m)) => {
            assert!(m.contains("net to zero"), "{m}");
        }
        other => panic!("expected NotAllowed(net to zero), got {other:?}"),
    }
}

#[test]
fn empty_reason_code_rejected() {
    let mut r = req(
        ManualAdjustmentAction::RoundingCorrection,
        vec![
            leg(AccountClass::Suspense, Side::Debit, 1),
            leg(AccountClass::CashClearing, Side::Credit, 1),
        ],
    );
    r.reason_code = "   ".to_owned();
    match govern(&r) {
        Err(ManualAdjustmentReject::NotAllowed(m)) => assert!(m.contains("reason_code"), "{m}"),
        other => panic!("expected NotAllowed(reason_code), got {other:?}"),
    }
}

#[test]
fn direct_revenue_leg_forbidden() {
    // A balanced pair that touches REVENUE directly — banned globally (step 4),
    // regardless of the action's allow-list.
    let r = req(
        ManualAdjustmentAction::SuspenseClear,
        vec![
            leg_stream(AccountClass::Revenue, Side::Debit, 10, "SAAS"),
            leg(AccountClass::Suspense, Side::Credit, 10),
        ],
    );
    match govern(&r) {
        Err(ManualAdjustmentReject::NotAllowed(m)) => assert!(m.contains("REVENUE"), "{m}"),
        other => panic!("expected NotAllowed(REVENUE), got {other:?}"),
    }
}

#[test]
fn contract_liability_leg_forbidden() {
    let r = req(
        ManualAdjustmentAction::SuspenseClear,
        vec![
            leg_stream(AccountClass::ContractLiability, Side::Debit, 10, "SAAS"),
            leg(AccountClass::Suspense, Side::Credit, 10),
        ],
    );
    match govern(&r) {
        Err(ManualAdjustmentReject::NotAllowed(m)) => {
            assert!(m.contains("CONTRACT_LIABILITY"), "{m}");
        }
        other => panic!("expected NotAllowed(CONTRACT_LIABILITY), got {other:?}"),
    }
}

#[test]
fn contra_revenue_without_paired_reduction_is_attempted_write_off() {
    // DR CONTRA_REVENUE / CR AR — a balanced pair, but the contra-revenue leg has
    // no paired same-stream REVENUE reduction (it cannot, since REVENUE is banned),
    // so it is the disguised-write-off shape ⇒ AttemptedWriteOff.
    let r = req(
        ManualAdjustmentAction::SuspenseClear,
        vec![
            leg_stream(AccountClass::ContraRevenue, Side::Debit, 10, "SAAS"),
            leg(AccountClass::Ar, Side::Credit, 10),
        ],
    );
    match govern(&r) {
        Err(ManualAdjustmentReject::AttemptedWriteOff(m)) => {
            assert!(m.contains("write-off"), "{m}");
        }
        other => panic!("expected AttemptedWriteOff, got {other:?}"),
    }
}

#[test]
fn zero_amount_leg_rejected() {
    let r = req(
        ManualAdjustmentAction::RoundingCorrection,
        vec![
            leg(AccountClass::Suspense, Side::Debit, 0),
            leg(AccountClass::CashClearing, Side::Credit, 0),
        ],
    );
    match govern(&r) {
        Err(ManualAdjustmentReject::NotAllowed(m)) => assert!(m.contains("> 0"), "{m}"),
        other => panic!("expected NotAllowed(> 0), got {other:?}"),
    }
}

#[test]
fn action_str_round_trips() {
    for a in [
        ManualAdjustmentAction::RoundingCorrection,
        ManualAdjustmentAction::SuspenseClear,
    ] {
        assert_eq!(ManualAdjustmentAction::parse(a.as_str()), Some(a));
    }
    assert_eq!(ManualAdjustmentAction::parse("NOPE"), None);
}
