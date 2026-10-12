//! Pure unit tests for [`RefundHandler`](super::RefundHandler)'s in-module
//! helpers: the stage-1 reversal plan inversion (`invert_plan`), the per-pattern
//! cap-target resolution (`RefundCap::for_request`), the `unknown_final`
//! loss-clearing plan, and the PII-clean `unknown_final` secured-audit payload.
//! Extracted to a sibling file (dylint DE1101 — no inline `#[cfg(test)] mod`).

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use bss_ledger_sdk::{AccountClass, CurrencySpec, PostedMoney};
use rust_decimal::Decimal;

use super::*;
use crate::domain::adjustment::refund::build_refund_legs;

/// `cents` minor units of USD (scale 2) as validated major-unit money.
fn usd(cents: i64) -> PostedMoney {
    PostedMoney::try_new(
        Decimal::new(cents, 2),
        CurrencySpec::try_new("USD".to_owned(), 2).unwrap(),
    )
    .unwrap()
}

fn stage1_req(pattern: RefundPattern, amount: i64) -> RefundRequest {
    let invoice_id = match pattern {
        RefundPattern::BRestoreAr => Some("inv-1".to_owned()),
        RefundPattern::AUnallocated => None,
    };
    RefundRequest {
        tenant_id: Uuid::now_v7(),
        payer_tenant_id: Uuid::now_v7(),
        refund_id: "rf-1".to_owned(),
        psp_refund_id: "psp-1".to_owned(),
        phase: RefundPhase::Initiated,
        pattern,
        payment_id: "pay-1".to_owned(),
        invoice_id,
        amount: usd(amount),
        two_stage: true,
        relates_to_refund_id: None,
        direction: RefundDirection::Outbound,
    }
}

fn sum_side(plan: &RefundLegPlan, side: Side) -> Decimal {
    plan.legs
        .iter()
        .filter(|l| l.side == side)
        .map(|l| l.amount.amount())
        .sum()
}

/// The stage-1 reversal is the STRICT line-negation of the stage-1 plan: same
/// classes + amounts, every side flipped. Asserted for both patterns.
#[test]
fn invert_plan_flips_sides_and_stays_balanced() {
    for pattern in [RefundPattern::AUnallocated, RefundPattern::BRestoreAr] {
        let stage1 = build_refund_legs(&stage1_req(pattern, 500)).unwrap();
        let reversed = invert_plan(&stage1);

        // Same number of legs, same classes + amounts.
        assert_eq!(reversed.legs.len(), stage1.legs.len());
        for (orig, rev) in stage1.legs.iter().zip(reversed.legs.iter()) {
            assert_eq!(rev.account_class, orig.account_class, "class preserved");
            assert_eq!(rev.amount, orig.amount, "amount preserved");
            assert_ne!(rev.side, orig.side, "side flipped");
        }

        // Stage-1 was DR pattern.debit · CR REFUND_CLEARING; the reversal is
        // DR REFUND_CLEARING · CR pattern.debit (drains clearing, restores the
        // drawn-down UNALLOCATED(A) / AR(B)).
        let debit_class = reversed
            .legs
            .iter()
            .find(|l| l.side == Side::Debit)
            .unwrap()
            .account_class;
        let credit_class = reversed
            .legs
            .iter()
            .find(|l| l.side == Side::Credit)
            .unwrap()
            .account_class;
        assert_eq!(debit_class, AccountClass::RefundClearing);
        assert_eq!(credit_class, pattern.debit_class());

        // Balanced (Σ DR == Σ CR), and the reversal row stamps REVERSED.
        assert_eq!(
            sum_side(&reversed, Side::Debit),
            sum_side(&reversed, Side::Credit)
        );
        assert_eq!(reversed.clearing_state, CLEARING_STATE_REVERSED);
    }
}

/// The cap movement is resolved from the pattern: both bump `refunded_minor`;
/// Pattern A additionally moves `refunded_unallocated_minor`; Pattern B
/// additionally targets the per-`(payment, invoice)` counter.
#[test]
fn refund_cap_targets_match_pattern() {
    let a = RefundCap::for_request(&stage1_req(RefundPattern::AUnallocated, 100));
    assert!(
        a.is_unallocated_pattern,
        "Pattern A moves refunded_unallocated"
    );
    assert!(
        a.invoice_id.is_none(),
        "Pattern A has no per-invoice target"
    );

    let b = RefundCap::for_request(&stage1_req(RefundPattern::BRestoreAr, 100));
    assert!(
        !b.is_unallocated_pattern,
        "Pattern B does not move refunded_unallocated"
    );
    assert_eq!(
        b.invoice_id.as_deref(),
        Some("inv-1"),
        "Pattern B targets the per-(payment, invoice) counter"
    );
}

/// The `unknown_final` disposition PARKS the stuck `REFUND_CLEARING` on SUSPENSE
/// with the plan `post_unknown_final` posts (`unknown_final_park_plan`): a
/// BALANCED two-leg plan `DR REFUND_CLEARING · CR SUSPENSE` sized at the stage-1
/// open amount it is given (not a request amount), draining the guarded clearing.
#[test]
fn unknown_final_park_clearing_plan_is_balanced_and_drains_clearing() {
    // A stage-1 open amount unlike any request figure, at a non-2 scale.
    let open = PostedMoney::try_new(
        rust_decimal::Decimal::new(7_505, 3),
        bss_ledger_sdk::CurrencySpec::try_new("KWD".to_owned(), 3).unwrap(),
    )
    .unwrap();
    let plan = unknown_final_park_plan(&open);
    assert_eq!(plan.legs.len(), 2);
    let dr = plan.legs.iter().find(|l| l.side == Side::Debit).unwrap();
    let cr = plan.legs.iter().find(|l| l.side == Side::Credit).unwrap();
    // The DR DRAINS the guarded REFUND_CLEARING; the CR PARKS on SUSPENSE (not a
    // premature loss/gain); both carry exactly the stage-1 open amount.
    assert_eq!(dr.account_class, AccountClass::RefundClearing);
    assert_eq!(cr.account_class, AccountClass::Suspense);
    assert_eq!(dr.amount, open);
    assert_eq!(cr.amount, open);
    assert!(dr.revenue_stream.is_none() && cr.revenue_stream.is_none());
    // The disposition drains REFUND_CLEARING off the live account → SETTLED.
    assert_eq!(plan.clearing_state, CLEARING_STATE_SETTLED);
}

/// The `unknown_final` secured-audit before/after payload is PII-clean (ids +
/// amounts + enum codes only — no names / emails / free text) and carries the
/// park arithmetic (open → 0, parked to SUSPENSE). The reason code is the closed
/// `REFUND_UNKNOWN_FINAL` literal.
#[test]
fn unknown_final_audit_payload_is_pii_clean_and_shaped() {
    let mut req = stage1_req(RefundPattern::AUnallocated, 900);
    req.phase = RefundPhase::UnknownFinal;
    // Z5-4: the before-image now carries the LIVE stage-1 clearing_state + open
    // amount (read from the stage-1 row by the handler) rather than a hardcoded
    // PENDING / the request amount. Here the stuck stage-1 is PENDING with 900 open.
    let payload = unknown_final_audit_payload(
        &req,
        crate::domain::adjustment::refund::CLEARING_STATE_PENDING,
        &usd(900),
    );

    assert_eq!(payload["disposition"], "REFUND_UNKNOWN_FINAL");
    assert_eq!(payload["before"]["clearing_state"], "PENDING");
    // Canonical decimal text (`9.00` → `"9"`), never a JSON number.
    assert_eq!(payload["currency"], "USD");
    assert_eq!(payload["currency_scale"], 2);
    assert_eq!(payload["before"]["refund_clearing_open"], "9");
    assert_eq!(payload["after"]["refund_clearing_open"], "0");
    assert_eq!(payload["after"]["parked"], "9");
    assert_eq!(payload["after"]["park_account_class"], "SUSPENSE");
    assert_eq!(payload["after"]["clearing_state"], CLEARING_STATE_SETTLED);
    assert_eq!(REASON_REFUND_UNKNOWN_FINAL, "REFUND_UNKNOWN_FINAL");

    // No PII: the serialized payload's keys/values are ids, amounts, and enum
    // codes. Assert the top-level key set is exactly the expected ids/amounts.
    let obj = payload.as_object().unwrap();
    let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        [
            "after",
            "before",
            "currency",
            "currency_scale",
            "disposition",
            "pattern",
            "payment_id",
            "psp_refund_id",
            "refund_id",
        ]
    );
}

fn settlement(settled: PostedMoney) -> SettlementState {
    let zero = PostedMoney::try_new(Decimal::ZERO, settled.currency().clone()).unwrap();
    SettlementState {
        tenant_id: Uuid::now_v7(),
        payment_id: "pay-1".to_owned(),
        version: 1,
        settled,
        fee: zero.clone(),
        allocated: zero.clone(),
        refunded: zero.clone(),
        refunded_unallocated: zero.clone(),
        clawed_back: zero,
    }
}

/// A refund in the settlement's currency but at another stored scale is a named
/// `InconsistentScale` (checked before any cap or post), distinct from a
/// different code (`CurrencyMismatch`); the same spec passes.
#[test]
fn refund_origin_spec_check_names_scale_and_code_mismatches() {
    let req = stage1_req(RefundPattern::AUnallocated, 500);
    let usd3 = PostedMoney::try_new(
        Decimal::new(10_000, 3),
        CurrencySpec::try_new("USD".to_owned(), 3).unwrap(),
    )
    .unwrap();
    match origin_spec_check(&req, &settlement(usd3)) {
        Err(DomainError::InconsistentScale(detail)) => {
            assert!(
                detail.contains("scale 2") && detail.contains("scale 3"),
                "{detail}"
            );
        }
        other => panic!("expected InconsistentScale, got {other:?}"),
    }
    let eur = PostedMoney::try_new(
        Decimal::new(10_000, 2),
        CurrencySpec::try_new("EUR".to_owned(), 2).unwrap(),
    )
    .unwrap();
    assert!(matches!(
        origin_spec_check(&req, &settlement(eur)),
        Err(DomainError::CurrencyMismatch(_))
    ));
    assert!(origin_spec_check(&req, &settlement(usd(10_000))).is_ok());
}
