//! Unit tests: `ApprovalIntent` jsonb roundtrip + derived keys.

use bss_ledger_sdk::{AccountClass, Side};
use chrono::NaiveDate;
use uuid::Uuid;

use super::{
    ApprovalIntent, BackdatedInvoiceItem, BackdatedInvoiceSnapshot, BackdatedPost,
    BackdatedTaxBreakdown, ChargebackLossIntent, CreditGrantIntent, CreditNoteIntent,
    DebitNoteIntent, ManualAdjustmentIntent, RecognitionChangeSegment,
    RecognitionScheduleChangeIntent, RefundIntent, ReverseIntent,
};
use crate::domain::adjustment::credit_note::CreditNoteRequest;
use crate::domain::adjustment::debit_note::DebitNoteRequest;
use crate::domain::adjustment::manual::{
    ManualAdjustmentAction, ManualAdjustmentRequest, ManualLeg,
};
use crate::domain::adjustment::refund::{
    RefundDirection, RefundPattern, RefundPhase, RefundRequest,
};
use crate::domain::approval::ApprovalKind;
use crate::domain::invoice::builder::{InvoiceItem, PostedInvoice, TaxBreakdown};
use crate::domain::recognition::input::{RecognitionInput, RecognitionTiming};

#[test]
fn credit_grant_intent_roundtrips() {
    let intent = ApprovalIntent::CreditGrant(CreditGrantIntent {
        tenant_id: Uuid::now_v7(),
        payer_tenant_id: Uuid::now_v7(),
        credit_application_id: "app-1".to_owned(),
        currency: "USD".to_owned(),
        amount_minor: 5_000,
        credit_grant_event_type: Some("promo".to_owned()),
    });
    let value = serde_json::to_value(&intent).unwrap();
    let back: ApprovalIntent = serde_json::from_value(value).unwrap();
    assert_eq!(intent, back);
    assert_eq!(intent.kind(), ApprovalKind::CreditGrant);
    assert_eq!(intent.business_key(), "app-1");
    assert_eq!(intent.amount_minor(), Some(5_000));
    assert_eq!(intent.currency(), Some("USD"));
}

#[test]
fn reverse_intent_roundtrips_and_has_no_carried_amount() {
    let entry_id = Uuid::now_v7();
    let intent = ApprovalIntent::Reverse(ReverseIntent {
        entry_id,
        into_period_id: Some("202606".to_owned()),
        effective_at: None,
        reason: "duplicate".to_owned(),
    });
    let back: ApprovalIntent =
        serde_json::from_value(serde_json::to_value(&intent).unwrap()).unwrap();
    assert_eq!(intent, back);
    assert_eq!(intent.kind(), ApprovalKind::Reverse);
    assert_eq!(intent.business_key(), entry_id.to_string());
    assert_eq!(
        intent.amount_minor(),
        None,
        "reverse amount comes from the original entry"
    );
}

#[test]
fn chargeback_loss_intent_roundtrips_and_keys_by_dispute_cycle() {
    let intent = ApprovalIntent::ChargebackLoss(ChargebackLossIntent {
        tenant_id: Uuid::now_v7(),
        payer_tenant_id: Uuid::now_v7(),
        payment_id: "pay-1".to_owned(),
        dispute_id: "disp-1".to_owned(),
        invoice_id: None,
        cycle: 2,
        funds_at_open: "withheld".to_owned(),
        disputed_amount_minor: 250_000,
        currency: "USD".to_owned(),
    });
    let back: ApprovalIntent =
        serde_json::from_value(serde_json::to_value(&intent).unwrap()).unwrap();
    assert_eq!(intent, back);
    assert_eq!(intent.business_key(), "disp-1:2:LOST");
    assert_eq!(intent.amount_minor(), Some(250_000));
}

#[test]
fn recognition_schedule_change_intent_roundtrips_and_keys_by_change_id() {
    let intent = ApprovalIntent::RecognitionScheduleChange(RecognitionScheduleChangeIntent {
        tenant_id: Uuid::now_v7(),
        schedule_id: "sched-1".to_owned(),
        change_id: "chg-7".to_owned(),
        action: "replace".to_owned(),
        treatment: "prospective".to_owned(),
        new_segments: Some(vec![
            RecognitionChangeSegment {
                period_id: "202607".to_owned(),
                amount_minor: 400,
            },
            RecognitionChangeSegment {
                period_id: "202608".to_owned(),
                amount_minor: 400,
            },
        ]),
    });
    let back: ApprovalIntent =
        serde_json::from_value(serde_json::to_value(&intent).unwrap()).unwrap();
    assert_eq!(intent, back);
    assert_eq!(intent.kind(), ApprovalKind::RecognitionScheduleChange);
    assert_eq!(
        intent.business_key(),
        "chg-7",
        "keyed by the idempotency change_id"
    );
    assert_eq!(
        intent.amount_minor(),
        None,
        "the affected deferred remainder is read from the schedule at gate time"
    );
    assert_eq!(intent.currency(), None);
}

#[test]
fn refund_intent_roundtrips_and_keys_by_psp_phase() {
    let intent = ApprovalIntent::Refund(RefundIntent {
        tenant_id: Uuid::now_v7(),
        payer_tenant_id: Uuid::now_v7(),
        refund_id: "rf-9".to_owned(),
        psp_refund_id: "psp-9".to_owned(),
        phase: RefundPhase::Initiated.as_str().to_owned(),
        pattern: RefundPattern::BRestoreAr.as_str().to_owned(),
        payment_id: "pay-9".to_owned(),
        invoice_id: Some("inv-9".to_owned()),
        currency: "USD".to_owned(),
        amount_minor: 150_000,
        two_stage: true,
        relates_to_refund_id: None,
        direction: RefundDirection::Outbound.as_str().to_owned(),
    });
    // The nested `kind`-tagged enum must survive the jsonb roundtrip verbatim.
    let back: ApprovalIntent =
        serde_json::from_value(serde_json::to_value(&intent).unwrap()).unwrap();
    assert_eq!(intent, back);
    assert_eq!(intent.kind(), ApprovalKind::Refund);
    assert_eq!(
        intent.business_key(),
        "psp-9:initiated",
        "keyed by the engine idempotency grain psp_refund_id:phase"
    );
    assert_eq!(
        intent.amount_minor(),
        Some(150_000),
        "the returned cash is the D2 comparand"
    );
    assert_eq!(intent.currency(), Some("USD"));
}

#[test]
fn refund_intent_rebuilds_into_an_identical_request() {
    let req = RefundRequest {
        tenant_id: Uuid::now_v7(),
        payer_tenant_id: Uuid::now_v7(),
        refund_id: "rf-1".to_owned(),
        psp_refund_id: "psp-1".to_owned(),
        phase: RefundPhase::Confirmed,
        pattern: RefundPattern::AUnallocated,
        payment_id: "pay-1".to_owned(),
        invoice_id: None,
        currency: "EUR".to_owned(),
        amount_minor: 999_999,
        two_stage: true,
        // A refund-of-refund claw-back so the round-trip also exercises the
        // direction + relates_to_refund_id snapshot fields (Group E).
        relates_to_refund_id: Some("rf-origin".to_owned()),
        direction: RefundDirection::Clawback,
    };
    // Snapshot -> jsonb -> snapshot -> RefundRequest reproduces the request exactly
    // (the executor's replay path: phase/pattern/direction survive as wire tokens).
    let snap = RefundIntent::from(&req);
    let back: RefundIntent = serde_json::from_value(serde_json::to_value(&snap).unwrap()).unwrap();
    let rebuilt = RefundRequest::try_from(&back).unwrap();
    assert_eq!(req, rebuilt);
}

#[test]
fn refund_intent_rejects_unknown_phase_or_pattern_token() {
    let mut snap = RefundIntent::from(&RefundRequest {
        tenant_id: Uuid::now_v7(),
        payer_tenant_id: Uuid::now_v7(),
        refund_id: "rf-1".to_owned(),
        psp_refund_id: "psp-1".to_owned(),
        phase: RefundPhase::Initiated,
        pattern: RefundPattern::AUnallocated,
        payment_id: "pay-1".to_owned(),
        invoice_id: None,
        currency: "USD".to_owned(),
        amount_minor: 100,
        two_stage: true,
        relates_to_refund_id: None,
        direction: RefundDirection::Outbound,
    });
    snap.phase = "NOT_A_PHASE".to_owned();
    assert!(
        RefundRequest::try_from(&snap).is_err(),
        "a corrupt phase token must fail the replay, not silently default"
    );
    snap.phase = RefundPhase::Initiated.as_str().to_owned();
    snap.pattern = "NOT_A_PATTERN".to_owned();
    assert!(
        RefundRequest::try_from(&snap).is_err(),
        "a corrupt pattern token must fail the replay"
    );
    snap.pattern = RefundPattern::AUnallocated.as_str().to_owned();
    snap.direction = "NOT_A_DIRECTION".to_owned();
    assert!(
        RefundRequest::try_from(&snap).is_err(),
        "a corrupt direction token must fail the replay, not silently default"
    );
}

/// A balanced two-leg manual adjustment (no tax) for the round-trip tests:
/// `DR SUSPENSE 1 · CR CASH_CLEARING 1` — a `RoundingCorrection` within its allow-list.
fn sample_manual_request() -> ManualAdjustmentRequest {
    ManualAdjustmentRequest {
        tenant_id: Uuid::now_v7(),
        payer_tenant_id: Some(Uuid::now_v7()),
        adjustment_id: "adj-1".to_owned(),
        action: ManualAdjustmentAction::RoundingCorrection,
        currency: "USD".to_owned(),
        legs: vec![
            ManualLeg {
                account_class: AccountClass::Suspense,
                side: Side::Debit,
                amount_minor: 1,
                revenue_stream: None,
            },
            ManualLeg {
                account_class: AccountClass::CashClearing,
                side: Side::Credit,
                amount_minor: 1,
                revenue_stream: None,
            },
        ],
        reason_code: "ROUNDING".to_owned(),
        preparer_actor_id: Uuid::now_v7(),
        approver_actor_id: None,
        // The MVP governed actions move no tax (TAX_PAYABLE is in no allow-list).
        tax: Vec::new(),
    }
}

#[test]
fn manual_adjustment_intent_roundtrips_and_keys_by_adjustment_id() {
    let req = sample_manual_request();
    let intent = ApprovalIntent::ManualAdjustment(ManualAdjustmentIntent::from(&req));
    // The nested `kind`-tagged enum must survive the jsonb roundtrip verbatim.
    let back: ApprovalIntent =
        serde_json::from_value(serde_json::to_value(&intent).unwrap()).unwrap();
    assert_eq!(intent, back);
    assert_eq!(intent.kind(), ApprovalKind::ManualAdjustment);
    assert_eq!(
        intent.business_key(),
        "adj-1",
        "keyed by the engine idempotency grain adjustment_id"
    );
    assert_eq!(
        intent.amount_minor(),
        Some(1),
        "the gross adjustment amount (Σ DR) is the D2 comparand"
    );
    assert_eq!(intent.currency(), Some("USD"));
}

#[test]
fn manual_adjustment_intent_rebuilds_into_an_identical_request() {
    let req = sample_manual_request();
    // Snapshot -> jsonb -> snapshot -> ManualAdjustmentRequest reproduces the request
    // exactly (the executor's replay path: action/class/side survive as wire tokens,
    // tax is rebuilt empty as it is never carried).
    let snap = ManualAdjustmentIntent::from(&req);
    let back: ManualAdjustmentIntent =
        serde_json::from_value(serde_json::to_value(&snap).unwrap()).unwrap();
    let rebuilt = ManualAdjustmentRequest::try_from(&back).unwrap();
    assert_eq!(req.tenant_id, rebuilt.tenant_id);
    assert_eq!(req.payer_tenant_id, rebuilt.payer_tenant_id);
    assert_eq!(req.adjustment_id, rebuilt.adjustment_id);
    assert_eq!(req.action, rebuilt.action);
    assert_eq!(req.currency, rebuilt.currency);
    assert_eq!(req.reason_code, rebuilt.reason_code);
    assert_eq!(req.preparer_actor_id, rebuilt.preparer_actor_id);
    assert_eq!(req.approver_actor_id, rebuilt.approver_actor_id);
    // Per-leg class + side + amount survive (the SDK enums via as_str/parse).
    assert_eq!(req.legs.len(), rebuilt.legs.len());
    for (orig, got) in req.legs.iter().zip(rebuilt.legs.iter()) {
        assert_eq!(orig.account_class, got.account_class);
        assert_eq!(orig.side, got.side);
        assert_eq!(orig.amount_minor, got.amount_minor);
        assert_eq!(orig.revenue_stream, got.revenue_stream);
    }
    // tax is never carried — empty in both.
    assert!(req.tax.is_empty());
    assert!(rebuilt.tax.is_empty());
    // The whole request is reproduced exactly.
    assert_eq!(req, rebuilt);
}

#[test]
fn manual_adjustment_intent_rejects_unknown_tokens() {
    let mut snap = ManualAdjustmentIntent::from(&sample_manual_request());
    snap.action = "NOT_AN_ACTION".to_owned();
    assert!(
        ManualAdjustmentRequest::try_from(&snap).is_err(),
        "a corrupt action token must fail the replay, not silently default"
    );
    snap.action = ManualAdjustmentAction::RoundingCorrection
        .as_str()
        .to_owned();
    snap.legs[0].account_class = "NOT_A_CLASS".to_owned();
    assert!(
        ManualAdjustmentRequest::try_from(&snap).is_err(),
        "a corrupt account_class token must fail the replay"
    );
    snap.legs[0].account_class = AccountClass::Suspense.as_str().to_owned();
    snap.legs[0].side = "NOT_A_SIDE".to_owned();
    assert!(
        ManualAdjustmentRequest::try_from(&snap).is_err(),
        "a corrupt side token must fail the replay"
    );
}

#[test]
fn credit_note_intent_rebuilds_into_an_identical_request() {
    // Z6-1: an over-D2 credit note is gated BEFORE its post, so the snapshot must
    // round-trip the WHOLE request (incl. the per-component tax dims) → the approved
    // replay re-drives the identical credit note.
    let req = CreditNoteRequest {
        tenant_id: Uuid::from_u128(1),
        payer_tenant_id: Uuid::from_u128(2),
        credit_note_id: "cn-1".to_owned(),
        origin_invoice_id: "inv-1".to_owned(),
        origin_invoice_item_ref: Some("item-1".to_owned()),
        po_allocation_group: Some("po-1".to_owned()),
        revenue_stream: "subscription".to_owned(),
        currency: "USD".to_owned(),
        amount_minor: 5_000,
        tax_minor: 500,
        tax: vec![TaxBreakdown {
            amount_minor: 500,
            currency: "USD".to_owned(),
            tax_jurisdiction: "US-CA".to_owned(),
            tax_filing_period: "202606".to_owned(),
            tax_rate_ref: Some("rate-1".to_owned()),
        }],
        requested_deferred_minor: 1_000,
        reason_code: "SERVICE_CREDIT".to_owned(),
        goodwill: false,
    };
    let snap = CreditNoteIntent::from(&req);
    let back: CreditNoteIntent =
        serde_json::from_value(serde_json::to_value(&snap).unwrap()).unwrap();
    let rebuilt = CreditNoteRequest::from(&back);
    assert_eq!(
        req, rebuilt,
        "credit-note replay must reproduce the request exactly"
    );
}

#[test]
fn debit_note_intent_rebuilds_into_an_identical_request_with_recognition() {
    // Z6-1: a DEFERRED over-D2 debit note carries a recognition spec; the snapshot must
    // round-trip it (incl. the StraightLine timing) so the approved replay rebuilds the
    // SAME schedule.
    let req = DebitNoteRequest {
        tenant_id: Uuid::from_u128(3),
        payer_tenant_id: Uuid::from_u128(4),
        debit_note_id: "dn-1".to_owned(),
        origin_invoice_id: "inv-2".to_owned(),
        origin_invoice_item_ref: Some("item-2".to_owned()),
        revenue_stream: "subscription".to_owned(),
        currency: "EUR".to_owned(),
        amount_minor: 12_000,
        tax_minor: 2_000,
        tax: vec![TaxBreakdown {
            amount_minor: 2_000,
            currency: "EUR".to_owned(),
            tax_jurisdiction: "DE".to_owned(),
            tax_filing_period: "202606".to_owned(),
            tax_rate_ref: None,
        }],
        deferred_minor: 6_000,
        reason_code: "UPSELL".to_owned(),
        recognition: Some(RecognitionInput {
            policy_ref: "policy-1".to_owned(),
            timing: RecognitionTiming::StraightLine {
                periods: 12,
                first_period_id: Some("202607".to_owned()),
            },
            po_allocation_group: Some("po-2".to_owned()),
            multi_po: false,
            ssp_snapshot_ref: None,
            subscription_ref: Some("sub-1".to_owned()),
            vc_estimate_ref: None,
            vc_method_ref: None,
            immaterial_one_shot_sku: false,
        }),
    };
    let snap = DebitNoteIntent::from(&req);
    let back: DebitNoteIntent =
        serde_json::from_value(serde_json::to_value(&snap).unwrap()).unwrap();
    let rebuilt = DebitNoteRequest::from(&back);
    assert_eq!(
        req, rebuilt,
        "debit-note replay must reproduce the request (incl. recognition) exactly"
    );
}

fn sample_snapshot() -> BackdatedInvoiceSnapshot {
    BackdatedInvoiceSnapshot {
        invoice_id: "inv-backdated-1".to_owned(),
        payer_tenant_id: Uuid::now_v7(),
        resource_tenant_id: None,
        seller_tenant_id: Uuid::now_v7(),
        effective_at: NaiveDate::from_ymd_opt(2026, 1, 15).unwrap(),
        due_date: Some(NaiveDate::from_ymd_opt(2026, 2, 15).unwrap()),
        period_id: "202601".to_owned(),
        items: vec![BackdatedInvoiceItem {
            amount_minor_ex_tax: 90_000,
            currency: "USD".to_owned(),
            revenue_stream: "subscription".to_owned(),
            catalog_class: Some("REVENUE".to_owned()),
            contract_class: None,
            gl_code: Some("4000".to_owned()),
            invoice_item_ref: Some("item-1".to_owned()),
            sku_or_plan_ref: None,
            price_id: None,
            pricing_snapshot_ref: None,
        }],
        tax: vec![BackdatedTaxBreakdown {
            amount_minor: 10_000,
            currency: "USD".to_owned(),
            tax_jurisdiction: "US-CA".to_owned(),
            tax_filing_period: "2026Q1".to_owned(),
            tax_rate_ref: None,
        }],
        posted_by_actor_id: Uuid::now_v7(),
        correlation_id: Uuid::now_v7(),
    }
}

#[test]
fn material_backdating_intent_roundtrips_and_keys_by_invoice() {
    let intent = ApprovalIntent::MaterialBackdating(BackdatedPost::Invoice(sample_snapshot()));
    // Nested internally-tagged enums (`kind` + `post`) must survive the jsonb roundtrip.
    let back: ApprovalIntent =
        serde_json::from_value(serde_json::to_value(&intent).unwrap()).unwrap();
    assert_eq!(intent, back);
    assert_eq!(intent.kind(), ApprovalKind::MaterialBackdating);
    assert_eq!(intent.business_key(), "inv-backdated-1");
    // gross = Σ items ex-tax (90_000) + Σ tax (10_000).
    assert_eq!(intent.amount_minor(), Some(100_000));
    assert_eq!(intent.currency(), Some("USD"));
}

#[test]
fn posted_invoice_to_snapshot_roundtrips_preserving_account_class() {
    let original = PostedInvoice {
        invoice_id: "inv-1".to_owned(),
        payer_tenant_id: Uuid::now_v7(),
        resource_tenant_id: Some(Uuid::now_v7()),
        seller_tenant_id: Uuid::now_v7(),
        effective_at: NaiveDate::from_ymd_opt(2026, 1, 10).unwrap(),
        due_date: None,
        period_id: "202601".to_owned(),
        items: vec![InvoiceItem {
            amount_minor_ex_tax: 5_000,
            // The backdating snapshot does not capture recognition (see the
            // `TryFrom<&BackdatedInvoiceItem>` seam note), so the round-trip yields
            // a non-deferred item.
            deferred_minor: 0,
            currency: "EUR".to_owned(),
            revenue_stream: "usage".to_owned(),
            catalog_class: Some(AccountClass::Revenue),
            contract_class: Some(AccountClass::ContractLiability),
            gl_code: Some("4100".to_owned()),
            recognition: None,
            invoice_item_ref: Some("ii-1".to_owned()),
            sku_or_plan_ref: Some("sku-9".to_owned()),
            price_id: None,
            pricing_snapshot_ref: None,
        }],
        tax: vec![TaxBreakdown {
            amount_minor: 950,
            currency: "EUR".to_owned(),
            tax_jurisdiction: "DE".to_owned(),
            tax_filing_period: "2026Q1".to_owned(),
            tax_rate_ref: Some("vat-19".to_owned()),
        }],
        posted_by_actor_id: Uuid::now_v7(),
        correlation_id: Uuid::now_v7(),
    };
    let snapshot = BackdatedInvoiceSnapshot::from(&original);
    // `AccountClass` is stored as its stable string token, not the enum.
    assert_eq!(snapshot.items[0].catalog_class.as_deref(), Some("REVENUE"));
    assert_eq!(
        snapshot.items[0].contract_class.as_deref(),
        Some("CONTRACT_LIABILITY")
    );
    // Survives a jsonb roundtrip and rebuilds into an identical PostedInvoice.
    let back: BackdatedInvoiceSnapshot =
        serde_json::from_value(serde_json::to_value(&snapshot).unwrap()).unwrap();
    let rebuilt = PostedInvoice::try_from(&back).unwrap();
    assert_eq!(original, rebuilt);
}

#[test]
fn snapshot_rebuild_rejects_unknown_account_class_token() {
    let mut snapshot = sample_snapshot();
    snapshot.items[0].catalog_class = Some("NOT_A_REAL_CLASS".to_owned());
    assert!(
        PostedInvoice::try_from(&snapshot).is_err(),
        "a corrupt account_class token must fail the replay, not silently drop"
    );
}
