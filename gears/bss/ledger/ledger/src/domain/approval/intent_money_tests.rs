//! Money metadata of approval intents: a manual adjustment's tax survives the
//! durable round trip, and every nested amount of each multi-amount kind must
//! share the intent's currency code and scale (checked by `amount()`, by the
//! client decoder and, as corruption, by the stored decoder).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use bss_ledger_sdk::{AccountClass, CurrencySpec, PostedMoney, Side};
use uuid::Uuid;

use super::{
    ApprovalIntent, BackdatedTaxBreakdown, CreditNoteIntent, DebitNoteIntent,
    ManualAdjustmentIntent, ManualLegIntent, RecognitionChangeSegment,
    RecognitionScheduleChangeIntent, RefundIntent, RefundWithCreditNoteIntent,
};
use crate::domain::adjustment::manual::{
    ManualAdjustmentAction, ManualAdjustmentRequest, ManualLeg,
};
use crate::domain::adjustment::refund::{RefundDirection, RefundPattern, RefundPhase};
use crate::domain::error::DomainError;
use crate::domain::invoice::builder::TaxBreakdown;
use crate::infra::approval::stored_intent::{
    StoredManualAdjustmentIntent, decode_client_intent, decode_intent, encode_intent,
};

fn money(text: &str, code: &str, scale: u8) -> PostedMoney {
    PostedMoney::try_new(
        bss_ledger_sdk::parse_decimal(text).unwrap(),
        CurrencySpec::try_new(code.into(), scale).unwrap(),
    )
    .unwrap()
}

fn usd(text: &str) -> PostedMoney {
    money(text, "USD", 2)
}

fn tax(amount: PostedMoney) -> BackdatedTaxBreakdown {
    BackdatedTaxBreakdown {
        amount,
        tax_jurisdiction: "US-CA".to_owned(),
        tax_filing_period: "2026Q4".to_owned(),
        tax_rate_ref: Some("rate-1".to_owned()),
    }
}

#[test]
fn manual_adjustment_tax_survives_the_durable_round_trip() {
    let req = ManualAdjustmentRequest {
        tenant_id: Uuid::now_v7(),
        payer_tenant_id: Some(Uuid::now_v7()),
        adjustment_id: "adj-tax".to_owned(),
        action: ManualAdjustmentAction::RoundingCorrection,
        currency: CurrencySpec::try_new("USD".into(), 2).unwrap(),
        legs: vec![
            ManualLeg {
                account_class: AccountClass::Suspense,
                side: Side::Debit,
                amount: usd("1.10"),
                revenue_stream: None,
            },
            ManualLeg {
                account_class: AccountClass::CashClearing,
                side: Side::Credit,
                amount: usd("1.10"),
                revenue_stream: None,
            },
        ],
        reason_code: "ROUNDING".to_owned(),
        preparer_actor_id: Uuid::now_v7(),
        approver_actor_id: None,
        tax: vec![
            TaxBreakdown {
                amount: usd("0.07"),
                tax_jurisdiction: "US-CA".to_owned(),
                tax_filing_period: "2026Q4".to_owned(),
                tax_rate_ref: Some("rate-ca".to_owned()),
            },
            TaxBreakdown {
                amount: usd("0.03"),
                tax_jurisdiction: "US-NY".to_owned(),
                tax_filing_period: "2026Q4".to_owned(),
                tax_rate_ref: None,
            },
        ],
    };
    let snap = ManualAdjustmentIntent::from(&req);
    assert_eq!(snap.tax.len(), 2);
    let dto: StoredManualAdjustmentIntent = serde_json::from_value(
        serde_json::to_value(StoredManualAdjustmentIntent::from(&snap)).unwrap(),
    )
    .unwrap();
    let back = ManualAdjustmentIntent::try_from(dto).unwrap();
    assert_eq!(back, snap);
    let rebuilt = ManualAdjustmentRequest::try_from(&back).unwrap();
    assert_eq!(rebuilt.tax, req.tax);
    assert_eq!(rebuilt, req);
    // The whole-intent codec carries it too.
    let intent = ApprovalIntent::ManualAdjustment(snap);
    assert_eq!(
        decode_intent(encode_intent(&intent).unwrap()).unwrap(),
        intent
    );
}

fn manual() -> ManualAdjustmentIntent {
    ManualAdjustmentIntent {
        tenant_id: Uuid::now_v7(),
        payer_tenant_id: None,
        adjustment_id: "adj-1".to_owned(),
        action: ManualAdjustmentAction::RoundingCorrection
            .as_str()
            .to_owned(),
        currency: CurrencySpec::try_new("USD".into(), 2).unwrap(),
        legs: vec![
            ManualLegIntent {
                account_class: AccountClass::Suspense.as_str().to_owned(),
                side: Side::Debit.as_str().to_owned(),
                amount: usd("5"),
                revenue_stream: None,
            },
            ManualLegIntent {
                account_class: AccountClass::CashClearing.as_str().to_owned(),
                side: Side::Credit.as_str().to_owned(),
                amount: usd("5"),
                revenue_stream: None,
            },
        ],
        tax: vec![tax(usd("0"))],
        reason_code: "ROUNDING".to_owned(),
        preparer_actor_id: Uuid::now_v7(),
        approver_actor_id: None,
    }
}

fn credit_note() -> CreditNoteIntent {
    CreditNoteIntent {
        tenant_id: Uuid::now_v7(),
        payer_tenant_id: Uuid::now_v7(),
        credit_note_id: "cn-1".to_owned(),
        origin_invoice_id: "inv-1".to_owned(),
        origin_invoice_item_ref: None,
        po_allocation_group: None,
        revenue_stream: "SAAS".to_owned(),
        amount: usd("100"),
        tax_amount: usd("10"),
        tax: vec![tax(usd("10"))],
        requested_deferred: usd("0"),
        reason_code: "GOODWILL".to_owned(),
        goodwill: false,
    }
}

fn debit_note() -> DebitNoteIntent {
    DebitNoteIntent {
        tenant_id: Uuid::now_v7(),
        payer_tenant_id: Uuid::now_v7(),
        debit_note_id: "dn-1".to_owned(),
        origin_invoice_id: "inv-1".to_owned(),
        origin_invoice_item_ref: None,
        revenue_stream: "SAAS".to_owned(),
        amount: usd("100"),
        tax_amount: usd("10"),
        tax: vec![tax(usd("10"))],
        deferred: usd("0"),
        reason_code: "USAGE".to_owned(),
        recognition: None,
    }
}

fn schedule_change() -> RecognitionScheduleChangeIntent {
    RecognitionScheduleChangeIntent {
        tenant_id: Uuid::now_v7(),
        schedule_id: "sch-1".to_owned(),
        change_id: "chg-1".to_owned(),
        action: "replace".to_owned(),
        treatment: "PROSPECTIVE".to_owned(),
        new_segments: Some(vec![
            RecognitionChangeSegment {
                period_id: "202611".to_owned(),
                amount: usd("40"),
            },
            RecognitionChangeSegment {
                period_id: "202612".to_owned(),
                amount: usd("60"),
            },
        ]),
    }
}

fn refund() -> RefundIntent {
    RefundIntent {
        tenant_id: Uuid::now_v7(),
        payer_tenant_id: Uuid::now_v7(),
        refund_id: "rf-1".to_owned(),
        psp_refund_id: "psp-1".to_owned(),
        phase: RefundPhase::Initiated.as_str().to_owned(),
        pattern: RefundPattern::BRestoreAr.as_str().to_owned(),
        payment_id: "pay-1".to_owned(),
        invoice_id: Some("inv-1".to_owned()),
        amount: usd("50"),
        two_stage: false,
        relates_to_refund_id: None,
        direction: RefundDirection::Outbound.as_str().to_owned(),
    }
}

/// Each case swaps exactly one nested amount of an otherwise valid intent.
fn mutated(swap: &dyn Fn(&PostedMoney) -> PostedMoney) -> Vec<(&'static str, ApprovalIntent)> {
    let mut out = Vec::new();
    let mut i = manual();
    i.legs[1].amount = swap(&i.legs[1].amount);
    out.push(("manual leg", ApprovalIntent::ManualAdjustment(i)));
    let mut i = manual();
    i.tax[0].amount = swap(&i.tax[0].amount);
    out.push(("manual tax", ApprovalIntent::ManualAdjustment(i)));
    for field in ["tax_amount", "requested_deferred", "tax"] {
        let mut i = credit_note();
        match field {
            "tax_amount" => i.tax_amount = swap(&i.tax_amount),
            "requested_deferred" => i.requested_deferred = swap(&i.requested_deferred),
            _ => i.tax[0].amount = swap(&i.tax[0].amount),
        }
        out.push(("credit note", ApprovalIntent::CreditNote(i)));
    }
    for field in ["tax_amount", "deferred", "tax"] {
        let mut i = debit_note();
        match field {
            "tax_amount" => i.tax_amount = swap(&i.tax_amount),
            "deferred" => i.deferred = swap(&i.deferred),
            _ => i.tax[0].amount = swap(&i.tax[0].amount),
        }
        out.push(("debit note", ApprovalIntent::DebitNote(i)));
    }
    let mut i = schedule_change();
    if let Some(segments) = i.new_segments.as_mut() {
        segments[1].amount = swap(&segments[1].amount);
    }
    out.push((
        "schedule change segment",
        ApprovalIntent::RecognitionScheduleChange(i),
    ));
    let mut composite = RefundWithCreditNoteIntent {
        refund: refund(),
        credit_note: credit_note(),
    };
    composite.credit_note.amount = swap(&composite.credit_note.amount);
    composite.credit_note.tax_amount = swap(&composite.credit_note.tax_amount);
    composite.credit_note.requested_deferred = swap(&composite.credit_note.requested_deferred);
    composite.credit_note.tax[0].amount = swap(&composite.credit_note.tax[0].amount);
    out.push((
        "refund vs credit note",
        ApprovalIntent::RefundWithCreditNote(composite),
    ));
    let mut composite = RefundWithCreditNoteIntent {
        refund: refund(),
        credit_note: credit_note(),
    };
    composite.credit_note.tax[0].amount = swap(&composite.credit_note.tax[0].amount);
    out.push((
        "composite credit-note tax",
        ApprovalIntent::RefundWithCreditNote(composite),
    ));
    out
}

#[test]
fn the_unmutated_fixtures_are_valid() {
    for (fixture, intent) in [
        ApprovalIntent::ManualAdjustment(manual()),
        ApprovalIntent::CreditNote(credit_note()),
        ApprovalIntent::DebitNote(debit_note()),
        ApprovalIntent::RecognitionScheduleChange(schedule_change()),
        ApprovalIntent::RefundWithCreditNote(RefundWithCreditNoteIntent {
            refund: refund(),
            credit_note: credit_note(),
        }),
    ]
    .into_iter()
    .enumerate()
    {
        assert!(
            intent.amount().is_ok(),
            "fixture {fixture} has a valid amount"
        );
        assert_eq!(
            decode_intent(encode_intent(&intent).unwrap()).unwrap(),
            intent
        );
    }
}

#[test]
fn a_nested_amount_in_another_currency_is_a_currency_mismatch() {
    let other_code = |m: &PostedMoney| money(&m.amount().to_string(), "EUR", m.currency().scale());
    for (why, intent) in mutated(&other_code) {
        assert!(
            matches!(intent.amount(), Err(DomainError::CurrencyMismatch(_))),
            "{why}: amount()"
        );
        let value = encode_intent(&intent).unwrap();
        assert!(
            matches!(
                decode_client_intent(value.clone()),
                Err(DomainError::CurrencyMismatch(_))
            ),
            "{why}: client decode"
        );
        assert!(
            matches!(decode_intent(value), Err(DomainError::Internal(_))),
            "{why}: stored decode is corruption"
        );
    }
}

#[test]
fn a_nested_amount_at_another_scale_is_an_inconsistent_scale() {
    let other_scale = |m: &PostedMoney| money(&m.amount().to_string(), m.currency().code(), 3);
    for (why, intent) in mutated(&other_scale) {
        assert!(
            matches!(intent.amount(), Err(DomainError::InconsistentScale(_))),
            "{why}: amount()"
        );
        let value = encode_intent(&intent).unwrap();
        assert!(
            matches!(
                decode_client_intent(value.clone()),
                Err(DomainError::InconsistentScale(_))
            ),
            "{why}: client decode"
        );
        assert!(
            matches!(decode_intent(value), Err(DomainError::Internal(_))),
            "{why}: stored decode is corruption"
        );
    }
}

/// The composite is governed by its larger leg, whichever leg that is. This is
/// the one rule the refund service's gate facts come from (`intent.amount()`),
/// so a credit note above D2 paired with a small refund still gates.
#[test]
fn the_composite_amount_is_its_larger_leg() {
    use crate::domain::approval::ApprovalKind;
    use crate::domain::approval::policy::{
        DualControlPolicy, OperationFacts, requires_dual_control,
    };
    let composite = |refund_amount: &str, note_amount: &str| {
        let mut i = RefundWithCreditNoteIntent {
            refund: refund(),
            credit_note: credit_note(),
        };
        i.refund.amount = usd(refund_amount);
        i.credit_note.amount = usd(note_amount);
        ApprovalIntent::RefundWithCreditNote(i)
    };
    assert_eq!(composite("50", "2500").amount().unwrap(), Some(usd("2500")));
    assert_eq!(composite("2500", "50").amount().unwrap(), Some(usd("2500")));
    assert_eq!(composite("50", "50").amount().unwrap(), Some(usd("50")));
    let facts = OperationFacts {
        kind: ApprovalKind::Refund,
        amount: composite("50", "2500").amount().unwrap(),
        effective_at: None,
        has_outstanding_balance: false,
    };
    let today = chrono::NaiveDate::from_ymd_opt(2026, 10, 9).unwrap();
    assert_eq!(
        requires_dual_control(&facts, &DualControlPolicy::DEFAULT, today),
        Ok(true),
        "the credit-note leg alone crosses D2"
    );
}
