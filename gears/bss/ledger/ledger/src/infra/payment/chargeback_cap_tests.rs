//! The exact money-out cap of a lost chargeback on a refunded payment, and the
//! service-side dispute transition guard.
#![allow(clippy::unwrap_used)]

use bss_ledger_sdk::{CurrencySpec, PostedMoney, parse_decimal};
use uuid::Uuid;

use super::clawback_fits;
use crate::domain::error::DomainError;
use crate::infra::storage::repo::payment_repo::SettlementState;

fn eur(text: &str) -> PostedMoney {
    PostedMoney::try_new(
        parse_decimal(text).unwrap(),
        CurrencySpec::try_new("EUR".into(), 2).unwrap(),
    )
    .unwrap()
}

fn settlement(settled: &str, refunded: &str, clawed_back: &str) -> SettlementState {
    SettlementState {
        tenant_id: Uuid::now_v7(),
        payment_id: "pay-1".into(),
        version: 3,
        settled: eur(settled),
        fee: eur("0"),
        allocated: eur("0"),
        refunded: eur(refunded),
        refunded_unallocated: eur("0"),
        clawed_back: eur(clawed_back),
    }
}

#[test]
fn a_clawback_that_reaches_the_cap_exactly_fits_and_one_increment_more_does_not() {
    // refunded 6.00 + stored clawback 1.00 + this 3.00 == settled 10.00.
    let state = settlement("10", "6", "1");
    assert!(clawback_fits(&state, &eur("3")).unwrap());
    assert!(!clawback_fits(&state, &eur("3.01")).unwrap());
    // Without a refund the same arithmetic applies.
    let unrefunded = settlement("10", "0", "0");
    assert!(clawback_fits(&unrefunded, &eur("10")).unwrap());
    assert!(!clawback_fits(&unrefunded, &eur("10.01")).unwrap());
}

#[test]
fn a_clawback_in_another_currency_or_scale_is_refused() {
    let state = settlement("10", "6", "1");
    let usd = PostedMoney::try_new(
        parse_decimal("1").unwrap(),
        CurrencySpec::try_new("USD".into(), 2).unwrap(),
    )
    .unwrap();
    assert!(matches!(
        clawback_fits(&state, &usd),
        Err(DomainError::CurrencyMismatch(_))
    ));
    let eur3 = PostedMoney::try_new(
        parse_decimal("1").unwrap(),
        CurrencySpec::try_new("EUR".into(), 3).unwrap(),
    )
    .unwrap();
    assert!(matches!(
        clawback_fits(&state, &eur3),
        Err(DomainError::InconsistentScale(_))
    ));
}

/// The out-of-txn guard runs the same state machine as the in-txn dispute write:
/// a stale-cycle outcome or a reopen for another payment is refused before any
/// posting, as `InvalidDisputeTransition`.
#[test]
fn the_service_guard_applies_the_cycle_and_payment_rules() {
    use super::{ChargebackRequest, guard_transition};
    use crate::domain::payment::chargeback::FundsAtOpen;
    use crate::domain::payment::dispute_state::{DisputePhase, DisputeVariant};
    use crate::infra::storage::repo::dispute_repo::DisputeState;

    let row = |phase, cycle| DisputeState {
        tenant_id: Uuid::nil(),
        dispute_id: "dsp-1".into(),
        payment_id: "pay-1".into(),
        variant: DisputeVariant::CashHold,
        last_phase: phase,
        cycle,
        disputed_amount: eur("5"),
        cash_hold: eur("5"),
        version: 1,
    };
    let req = |payment: &str, phase, cycle| ChargebackRequest {
        tenant_id: Uuid::nil(),
        payer_tenant_id: Uuid::nil(),
        payment_id: payment.into(),
        dispute_id: "dsp-1".into(),
        invoice_id: None,
        cycle,
        phase,
        funds_at_open: FundsAtOpen::Withheld,
        disputed_amount: eur("5"),
        effective_at: None,
    };
    let open2 = row(DisputePhase::Opened, 2);
    let won1 = row(DisputePhase::Won, 1);
    assert!(guard_transition(Some(&open2), &req("pay-1", DisputePhase::Lost, 2)).is_ok());
    assert!(guard_transition(None, &req("pay-1", DisputePhase::Opened, 1)).is_ok());
    assert!(guard_transition(Some(&won1), &req("pay-1", DisputePhase::Opened, 2)).is_ok());
    for (existing, request) in [
        (Some(&open2), req("pay-1", DisputePhase::Lost, 1)),
        (Some(&won1), req("pay-2", DisputePhase::Opened, 2)),
        (Some(&won1), req("pay-1", DisputePhase::Opened, 3)),
        (Some(&open2), req("pay-1", DisputePhase::Opened, 3)),
        (None, req("pay-1", DisputePhase::Opened, 2)),
        (None, req("pay-1", DisputePhase::Won, 1)),
    ] {
        assert!(
            matches!(
                guard_transition(existing, &request),
                Err(DomainError::InvalidDisputeTransition(_))
            ),
            "{:?} at cycle {} over {:?}",
            request.phase,
            request.cycle,
            existing.map(|r| (r.last_phase, r.cycle))
        );
    }
}
