//! Unit tests: `ApprovalIntent` jsonb roundtrip + derived keys.

use crate::infra::approval::stored_intent::{
    StoredBackdatedInvoiceSnapshot, StoredCreditNoteIntent, StoredDebitNoteIntent,
    StoredManualAdjustmentIntent, StoredRefundIntent, decode_client_intent, decode_intent,
    encode_intent,
};
use bss_ledger_sdk::{AccountClass, CurrencySpec, PostedMoney, Side};
use rust_decimal::Decimal;
fn money(units: i64, code: &str) -> PostedMoney {
    PostedMoney::try_new(
        Decimal::new(units, 2),
        CurrencySpec::try_new(code.into(), 2).unwrap(),
    )
    .unwrap()
}
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

        amount: money(5_000, "USD"),
        credit_grant_event_type: Some("promo".to_owned()),
    });
    let value = encode_intent(&intent).unwrap();
    let back: ApprovalIntent = decode_intent(value).unwrap();
    assert_eq!(intent, back);
    assert_eq!(intent.kind(), ApprovalKind::CreditGrant);
    assert_eq!(intent.business_key(), "app-1");
    assert_eq!(intent.amount().unwrap(), Some(money(5_000, "USD")));
    assert_eq!(
        intent
            .amount()
            .unwrap()
            .as_ref()
            .map(|v| v.currency().code()),
        Some("USD")
    );
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
    let back: ApprovalIntent = decode_intent(encode_intent(&intent).unwrap()).unwrap();
    assert_eq!(intent, back);
    assert_eq!(intent.kind(), ApprovalKind::Reverse);
    assert_eq!(intent.business_key(), entry_id.to_string());
    assert_eq!(
        intent.amount().unwrap(),
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
        disputed_amount: money(250_000, "USD"),
    });
    let back: ApprovalIntent = decode_intent(encode_intent(&intent).unwrap()).unwrap();
    assert_eq!(intent, back);
    assert_eq!(intent.business_key(), "disp-1:2:LOST");
    assert_eq!(intent.amount().unwrap(), Some(money(250_000, "USD")));
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
                amount: money(400, "USD"),
            },
            RecognitionChangeSegment {
                period_id: "202608".to_owned(),
                amount: money(400, "USD"),
            },
        ]),
    });
    let back: ApprovalIntent = decode_intent(encode_intent(&intent).unwrap()).unwrap();
    assert_eq!(intent, back);
    assert_eq!(intent.kind(), ApprovalKind::RecognitionScheduleChange);
    assert_eq!(
        intent.business_key(),
        "chg-7",
        "keyed by the idempotency change_id"
    );
    assert_eq!(
        intent.amount().unwrap(),
        None,
        "the affected deferred remainder is read from the schedule at gate time"
    );
    assert_eq!(
        intent
            .amount()
            .unwrap()
            .as_ref()
            .map(|v| v.currency().code()),
        None
    );
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

        amount: money(150_000, "USD"),
        two_stage: true,
        relates_to_refund_id: None,
        direction: RefundDirection::Outbound.as_str().to_owned(),
    });
    // The nested `kind`-tagged enum must survive the jsonb roundtrip verbatim.
    let back: ApprovalIntent = decode_intent(encode_intent(&intent).unwrap()).unwrap();
    assert_eq!(intent, back);
    assert_eq!(intent.kind(), ApprovalKind::Refund);
    assert_eq!(
        intent.business_key(),
        "psp-9:initiated",
        "keyed by the engine idempotency grain psp_refund_id:phase"
    );
    assert_eq!(
        intent.amount().unwrap(),
        Some(money(150_000, "USD")),
        "the returned cash is the D2 comparand"
    );
    assert_eq!(
        intent
            .amount()
            .unwrap()
            .as_ref()
            .map(|v| v.currency().code()),
        Some("USD")
    );
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

        amount: money(999_999, "EUR"),
        two_stage: true,
        // A refund-of-refund claw-back so the round-trip also exercises the
        // direction + relates_to_refund_id snapshot fields (Group E).
        relates_to_refund_id: Some("rf-origin".to_owned()),
        direction: RefundDirection::Clawback,
    };
    // Snapshot -> jsonb -> snapshot -> RefundRequest reproduces the request exactly
    // (the executor's replay path: phase/pattern/direction survive as wire tokens).
    let snap = RefundIntent::from(&req);
    let dto: StoredRefundIntent =
        serde_json::from_value(serde_json::to_value(StoredRefundIntent::from(&snap)).unwrap())
            .unwrap();
    let back = RefundIntent::try_from(dto).unwrap();
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

        amount: money(100, "USD"),
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

        currency: CurrencySpec::try_new("USD".into(), 2).unwrap(),
        legs: vec![
            ManualLeg {
                account_class: AccountClass::Suspense,
                side: Side::Debit,
                amount: money(1, "USD"),
                revenue_stream: None,
            },
            ManualLeg {
                account_class: AccountClass::CashClearing,
                side: Side::Credit,
                amount: money(1, "USD"),
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
    let back: ApprovalIntent = decode_intent(encode_intent(&intent).unwrap()).unwrap();
    assert_eq!(intent, back);
    assert_eq!(intent.kind(), ApprovalKind::ManualAdjustment);
    assert_eq!(
        intent.business_key(),
        "adj-1",
        "keyed by the engine idempotency grain adjustment_id"
    );
    assert_eq!(
        intent.amount().unwrap(),
        Some(money(1, "USD")),
        "the gross adjustment amount (Σ DR) is the D2 comparand"
    );
    assert_eq!(
        intent
            .amount()
            .unwrap()
            .as_ref()
            .map(|v| v.currency().code()),
        Some("USD")
    );
}

#[test]
fn manual_adjustment_intent_rebuilds_into_an_identical_request() {
    let req = sample_manual_request();
    // Snapshot -> jsonb -> snapshot -> ManualAdjustmentRequest reproduces the request
    // exactly (the executor's replay path: action/class/side survive as wire tokens,
    // tax is rebuilt empty as it is never carried).
    let snap = ManualAdjustmentIntent::from(&req);
    let dto: StoredManualAdjustmentIntent = serde_json::from_value(
        serde_json::to_value(StoredManualAdjustmentIntent::from(&snap)).unwrap(),
    )
    .unwrap();
    let back = ManualAdjustmentIntent::try_from(dto).unwrap();
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
        assert_eq!(orig.amount, got.amount);
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

        amount: money(5_000, "USD"),
        tax_amount: money(500, "USD"),
        tax: vec![TaxBreakdown {
            amount: money(500, "USD"),

            tax_jurisdiction: "US-CA".to_owned(),
            tax_filing_period: "202606".to_owned(),
            tax_rate_ref: Some("rate-1".to_owned()),
        }],
        requested_deferred: money(1_000, "USD"),
        reason_code: "SERVICE_CREDIT".to_owned(),
        goodwill: false,
    };
    let snap = CreditNoteIntent::from(&req);
    let dto: StoredCreditNoteIntent =
        serde_json::from_value(serde_json::to_value(StoredCreditNoteIntent::from(&snap)).unwrap())
            .unwrap();
    let back = CreditNoteIntent::try_from(dto).unwrap();
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

        amount: money(12_000, "EUR"),
        tax_amount: money(2_000, "EUR"),
        tax: vec![TaxBreakdown {
            amount: money(2_000, "EUR"),

            tax_jurisdiction: "DE".to_owned(),
            tax_filing_period: "202606".to_owned(),
            tax_rate_ref: None,
        }],
        deferred: money(6_000, "EUR"),
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
    let dto: StoredDebitNoteIntent =
        serde_json::from_value(serde_json::to_value(StoredDebitNoteIntent::from(&snap)).unwrap())
            .unwrap();
    let back = DebitNoteIntent::try_from(dto).unwrap();
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
            amount_ex_tax: money(90_000, "USD"),
            deferred: money(0, "USD"),
            recognition: None,

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
            amount: money(10_000, "USD"),

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
    let back: ApprovalIntent = decode_intent(encode_intent(&intent).unwrap()).unwrap();
    assert_eq!(intent, back);
    assert_eq!(intent.kind(), ApprovalKind::MaterialBackdating);
    assert_eq!(intent.business_key(), "inv-backdated-1");
    // gross = Σ items ex-tax (90_000) + Σ tax (10_000).
    assert_eq!(intent.amount().unwrap(), Some(money(100_000, "USD")));
    assert_eq!(
        intent
            .amount()
            .unwrap()
            .as_ref()
            .map(|v| v.currency().code()),
        Some("USD")
    );
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
            amount_ex_tax: money(5_000, "EUR"),
            // The backdating snapshot does not capture recognition (see the
            // `TryFrom<&BackdatedInvoiceItem>` seam note), so the round-trip yields
            // a non-deferred item.
            deferred: money(2500, "EUR"),

            revenue_stream: "usage".to_owned(),
            catalog_class: Some(AccountClass::Revenue),
            contract_class: Some(AccountClass::ContractLiability),
            gl_code: Some("4100".to_owned()),
            recognition: Some(RecognitionInput {
                policy_ref: "001.00".into(),
                timing: RecognitionTiming::StraightLine {
                    periods: 3,
                    first_period_id: Some("0007".into()),
                },
                po_allocation_group: Some("po:001".into()),
                multi_po: true,
                ssp_snapshot_ref: Some("ssp".into()),
                subscription_ref: Some("sub".into()),
                vc_estimate_ref: Some("estimate".into()),
                vc_method_ref: Some("method".into()),
                immaterial_one_shot_sku: true,
            }),
            invoice_item_ref: Some("ii-1".to_owned()),
            sku_or_plan_ref: Some("sku-9".to_owned()),
            price_id: None,
            pricing_snapshot_ref: None,
        }],
        tax: vec![TaxBreakdown {
            amount: money(950, "EUR"),

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
    let dto: StoredBackdatedInvoiceSnapshot = serde_json::from_value(
        serde_json::to_value(StoredBackdatedInvoiceSnapshot::from(&snapshot)).unwrap(),
    )
    .unwrap();
    let back = BackdatedInvoiceSnapshot::try_from(dto).unwrap();
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

#[test]
fn canonical_identity_preserves_all_nonmoney_strings_and_optional_presence() {
    use crate::infra::approval::stored_intent::{StoredApprovalIntent, canonical_identity};
    let original = ApprovalIntent::CreditGrant(CreditGrantIntent {
        tenant_id: Uuid::from_u128(1),
        payer_tenant_id: Uuid::from_u128(2),
        credit_application_id: "001.00:a\\b\n雪".into(),
        amount: money(1234, "EUR"),
        credit_grant_event_type: Some("001.00".into()),
    });
    let mut json = encode_intent(&original).unwrap();
    json["amount"]["amount"] = "12.3400".into();
    let equivalent = decode_intent(json.clone()).unwrap();
    assert_eq!(
        canonical_identity(&original).unwrap(),
        canonical_identity(&equivalent).unwrap()
    );
    assert_eq!(
        encode_intent(&equivalent).unwrap()["credit_grant_event_type"],
        "001.00"
    );
    for changed in ["12.35", "13"] {
        json["amount"]["amount"] = changed.into();
        assert_ne!(
            canonical_identity(&original).unwrap(),
            canonical_identity(&decode_intent(json.clone()).unwrap()).unwrap()
        );
    }
    json = encode_intent(&original).unwrap();
    json["credit_grant_event_type"] = "1".into();
    assert_ne!(
        canonical_identity(&original).unwrap(),
        canonical_identity(&decode_intent(json.clone()).unwrap()).unwrap()
    );
    for code in ["USD", "EUR"] {
        json = encode_intent(&original).unwrap();
        json["amount"]["currency"] = code.into();
        json["amount"]["currency_scale"] = 3.into();
        assert_ne!(
            canonical_identity(&original).unwrap(),
            canonical_identity(&decode_intent(json.clone()).unwrap()).unwrap()
        );
    }
    for invalid in ["0.001", "1e3", "nan", "99999999999999999999999999999"] {
        json = encode_intent(&original).unwrap();
        json["amount"]["amount"] = invalid.into();
        assert!(matches!(
            decode_intent(json.clone()),
            Err(crate::domain::error::DomainError::Internal(_))
        ));
        let dto: StoredApprovalIntent = serde_json::from_value(json.clone()).unwrap();
        assert!(ApprovalIntent::try_from(dto).is_err());
    }
    let mut a = original.clone();
    let mut b = original;
    if let ApprovalIntent::CreditGrant(i) = &mut a {
        i.credit_grant_event_type = None;
    }
    if let ApprovalIntent::CreditGrant(i) = &mut b {
        i.credit_grant_event_type = Some(String::new());
    }
    assert_ne!(
        canonical_identity(&a).unwrap(),
        canonical_identity(&b).unwrap()
    );
}

#[test]
fn same_target_allows_only_grant_or_chargeback_magnitude_edits_with_frozen_spec() {
    let original = ApprovalIntent::CreditGrant(CreditGrantIntent {
        tenant_id: Uuid::from_u128(1),
        payer_tenant_id: Uuid::from_u128(2),
        credit_application_id: "grant".into(),
        amount: money(100_000, "EUR"),
        credit_grant_event_type: None,
    });
    let mut changed = original.clone();
    if let ApprovalIntent::CreditGrant(i) = &mut changed {
        i.amount = money(99999, "EUR");
    }
    assert!(original.same_target(&changed));
    if let ApprovalIntent::CreditGrant(i) = &mut changed {
        i.amount = PostedMoney::try_new(
            Decimal::ONE,
            CurrencySpec::try_new("EUR".into(), 3).unwrap(),
        )
        .unwrap();
    }
    assert!(!original.same_target(&changed));
    if let ApprovalIntent::CreditGrant(i) = &mut changed {
        i.amount = money(100_000, "USD");
    }
    assert!(!original.same_target(&changed));
    if let ApprovalIntent::CreditGrant(i) = &mut changed {
        i.amount = money(100_000, "EUR");
        i.payer_tenant_id = Uuid::from_u128(3);
    }
    assert!(!original.same_target(&changed));
    let original = ApprovalIntent::ChargebackLoss(ChargebackLossIntent {
        tenant_id: Uuid::from_u128(1),
        payer_tenant_id: Uuid::from_u128(2),
        payment_id: "pay".into(),
        dispute_id: "dispute".into(),
        invoice_id: None,
        cycle: 2,
        funds_at_open: "001.00".into(),
        disputed_amount: money(100_000, "EUR"),
    });
    let mut changed = original.clone();
    if let ApprovalIntent::ChargebackLoss(i) = &mut changed {
        i.disputed_amount = money(50000, "EUR");
    }
    assert!(original.same_target(&changed));
    if let ApprovalIntent::ChargebackLoss(i) = &mut changed {
        i.disputed_amount = money(50000, "USD");
    }
    assert!(!original.same_target(&changed));
    let req = sample_manual_request();
    let original = ApprovalIntent::ManualAdjustment(ManualAdjustmentIntent::from(&req));
    let mut changed = original.clone();
    if let ApprovalIntent::ManualAdjustment(i) = &mut changed {
        i.legs[0].amount = money(2, "USD");
    }
    assert!(!original.same_target(&changed));
}

#[test]
fn exact_gross_rejects_final_overflow_and_conflicting_metadata_without_saturation() {
    let mut invoice = sample_snapshot();
    invoice.items[0].amount_ex_tax = PostedMoney::try_new(
        Decimal::from_str_exact("9999999999999999999999999999").unwrap(),
        CurrencySpec::try_new("USD".into(), 2).unwrap(),
    )
    .unwrap();
    invoice.tax[0].amount = money(100, "USD");
    assert!(matches!(
        ApprovalIntent::MaterialBackdating(BackdatedPost::Invoice(invoice.clone())).amount(),
        Err(crate::domain::error::DomainError::AmountOutOfRange(_))
    ));
    invoice.items[0].amount_ex_tax = money(100, "USD");
    invoice.tax[0].amount = money(100, "EUR");
    assert!(matches!(
        ApprovalIntent::MaterialBackdating(BackdatedPost::Invoice(invoice)).amount(),
        Err(crate::domain::error::DomainError::CurrencyMismatch(_))
    ));
    let mut request = sample_manual_request();
    let large = PostedMoney::try_new(
        Decimal::from_str_exact("9999999999999999999999999999").unwrap(),
        request.currency.clone(),
    )
    .unwrap();
    request.legs[0].amount = large.clone();
    request.legs[1].side = Side::Debit;
    request.legs[1].amount = large.clone();
    let mut negative = request.legs[0].clone();
    negative.amount = PostedMoney::try_new(-large.amount(), request.currency.clone()).unwrap();
    request.legs.push(negative);
    assert_eq!(
        ApprovalIntent::ManualAdjustment(ManualAdjustmentIntent::from(&request))
            .amount()
            .unwrap(),
        Some(large)
    );
}

#[test]
fn composite_refund_note_preserves_both_halves_and_money_errors_are_named_on_wire() {
    use crate::infra::approval::stored_intent::StoredApprovalIntent;
    let refund = RefundRequest {
        tenant_id: Uuid::from_u128(1),
        payer_tenant_id: Uuid::from_u128(2),
        refund_id: "001.00".into(),
        psp_refund_id: "psp".into(),
        phase: RefundPhase::Confirmed,
        pattern: RefundPattern::AUnallocated,
        payment_id: "pay".into(),
        invoice_id: Some("invoice".into()),
        amount: money(100_000, "EUR"),
        two_stage: true,
        relates_to_refund_id: Some("origin".into()),
        direction: RefundDirection::Clawback,
    };
    let note = CreditNoteRequest {
        tenant_id: refund.tenant_id,
        payer_tenant_id: refund.payer_tenant_id,
        credit_note_id: "note".into(),
        origin_invoice_id: "invoice".into(),
        origin_invoice_item_ref: Some("item".into()),
        po_allocation_group: Some("po".into()),
        revenue_stream: "stream".into(),
        amount: money(100_000, "EUR"),
        tax_amount: money(1, "EUR"),
        tax: vec![TaxBreakdown {
            amount: money(1, "EUR"),
            tax_jurisdiction: "001.00".into(),
            tax_filing_period: "period".into(),
            tax_rate_ref: Some("rate".into()),
        }],
        requested_deferred: money(50, "EUR"),
        reason_code: "reason".into(),
        goodwill: false,
    };
    let original = ApprovalIntent::RefundWithCreditNote(
        super::RefundWithCreditNoteIntent::from_requests(&refund, &note),
    );
    let rebuilt = decode_intent(encode_intent(&original).unwrap()).unwrap();
    assert_eq!(original, rebuilt);
    // The composite is governed by its larger leg, whichever leg that is.
    assert_eq!(original.amount().unwrap(), Some(money(100_000, "EUR")));
    let mut larger_note = note.clone();
    larger_note.amount = money(250_000, "EUR");
    assert_eq!(
        ApprovalIntent::RefundWithCreditNote(super::RefundWithCreditNoteIntent::from_requests(
            &refund,
            &larger_note
        ))
        .amount()
        .unwrap(),
        Some(money(250_000, "EUR"))
    );
    let mut larger_refund = refund.clone();
    larger_refund.amount = money(300_000, "EUR");
    assert_eq!(
        ApprovalIntent::RefundWithCreditNote(super::RefundWithCreditNoteIntent::from_requests(
            &larger_refund,
            &note
        ))
        .amount()
        .unwrap(),
        Some(money(300_000, "EUR"))
    );
    if let ApprovalIntent::RefundWithCreditNote(v) = rebuilt {
        assert_eq!(v.to_requests().unwrap(), (refund, note));
    }
    let mut invalid = encode_intent(&original).unwrap();
    invalid["credit_note"]["tax"][0]["amount"]["amount"] = "0.001".into();
    assert!(matches!(
        ApprovalIntent::try_from(
            serde_json::from_value::<StoredApprovalIntent>(invalid.clone()).unwrap()
        ),
        Err(crate::domain::error::DomainError::InvalidPostingIncrement(
            _
        ))
    ));
    assert!(matches!(
        decode_intent(invalid.clone()),
        Err(crate::domain::error::DomainError::Internal(_))
    ));
    // The same bytes from a client (resubmit) keep their client category.
    assert!(matches!(
        decode_client_intent(invalid),
        Err(crate::domain::error::DomainError::InvalidPostingIncrement(
            _
        ))
    ));
    assert!(matches!(
        decode_client_intent(serde_json::json!({ "kind": "nonsense" })),
        Err(crate::domain::error::DomainError::InvalidRequest(_))
    ));
    assert_eq!(
        decode_client_intent(encode_intent(&original).unwrap()).unwrap(),
        original
    );
}
