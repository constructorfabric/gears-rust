//! Generated sweep over the durable approval-intent codec (deterministic, no
//! extra test dependency): intents of every amount-bearing shape, at every
//! scale 0..=28, with optional fields present and absent and empty and
//! non-empty leg/tax/segment lists, decode back to themselves and keep a stable
//! canonical identity.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::*;
use crate::domain::approval::intent::{
    ApprovalIntent, BackdatedTaxBreakdown, ChargebackLossIntent, CreditGrantIntent,
    CreditNoteIntent, ManualAdjustmentIntent, ManualLegIntent, RecognitionChangeSegment,
    RecognitionScheduleChangeIntent, RefundIntent, RefundWithCreditNoteIntent, ReverseIntent,
};
use rust_decimal::Decimal;

struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        self.0 >> 11
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
    fn flag(&mut self) -> bool {
        self.below(2) == 1
    }
    fn text(&mut self, prefix: &str) -> String {
        format!("{prefix}-{}", self.below(1_000_000))
    }
    fn uuid(&mut self) -> Uuid {
        Uuid::from_u128(u128::from(self.next()) << 64 | u128::from(self.next()))
    }
    /// A valid posting at `spec` with up to 18 coefficient digits.
    fn money(&mut self, spec: &CurrencySpec) -> PostedMoney {
        let digits = 1 + self.below(18);
        let mut mantissa = i128::from(self.below(9) + 1);
        for _ in 1..digits {
            mantissa = mantissa * 10 + i128::from(self.below(10));
        }
        let places = u32::try_from(self.below(u64::from(spec.scale()) + 1)).unwrap();
        PostedMoney::try_new(
            Decimal::from_i128_with_scale(mantissa, places),
            spec.clone(),
        )
        .unwrap()
    }
    fn opt_text(&mut self, prefix: &str) -> Option<String> {
        self.flag().then(|| self.text(prefix))
    }
}

fn taxes(rng: &mut Lcg, spec: &CurrencySpec) -> Vec<BackdatedTaxBreakdown> {
    (0..rng.below(3))
        .map(|_| BackdatedTaxBreakdown {
            amount: rng.money(spec),
            tax_jurisdiction: rng.text("J"),
            tax_filing_period: "2026Q4".to_owned(),
            tax_rate_ref: rng.opt_text("rate"),
        })
        .collect()
}

fn credit_note(rng: &mut Lcg, spec: &CurrencySpec) -> CreditNoteIntent {
    CreditNoteIntent {
        tenant_id: rng.uuid(),
        payer_tenant_id: rng.uuid(),
        credit_note_id: rng.text("cn"),
        origin_invoice_id: rng.text("inv"),
        origin_invoice_item_ref: rng.opt_text("item"),
        po_allocation_group: rng.opt_text("po"),
        revenue_stream: rng.text("stream"),
        amount: rng.money(spec),
        tax_amount: rng.money(spec),
        tax: taxes(rng, spec),
        requested_deferred: rng.money(spec),
        reason_code: rng.text("reason"),
        goodwill: rng.flag(),
    }
}

fn refund(rng: &mut Lcg, spec: &CurrencySpec) -> RefundIntent {
    RefundIntent {
        tenant_id: rng.uuid(),
        payer_tenant_id: rng.uuid(),
        refund_id: rng.text("rf"),
        psp_refund_id: rng.text("psp"),
        phase: "initiated".to_owned(),
        pattern: "B_RESTORE_AR".to_owned(),
        payment_id: rng.text("pay"),
        invoice_id: rng.opt_text("inv"),
        amount: rng.money(spec),
        two_stage: rng.flag(),
        relates_to_refund_id: rng.opt_text("rf"),
        direction: "OUTBOUND".to_owned(),
    }
}

fn generate(rng: &mut Lcg, spec: &CurrencySpec) -> Vec<ApprovalIntent> {
    vec![
        ApprovalIntent::CreditGrant(CreditGrantIntent {
            tenant_id: rng.uuid(),
            payer_tenant_id: rng.uuid(),
            credit_application_id: rng.text("ca"),
            amount: rng.money(spec),
            credit_grant_event_type: rng.opt_text("evt"),
        }),
        ApprovalIntent::ChargebackLoss(ChargebackLossIntent {
            tenant_id: rng.uuid(),
            payer_tenant_id: rng.uuid(),
            payment_id: rng.text("pay"),
            dispute_id: rng.text("dsp"),
            invoice_id: rng.opt_text("inv"),
            cycle: i32::try_from(rng.below(5)).unwrap(),
            funds_at_open: "withheld".to_owned(),
            disputed_amount: rng.money(spec),
        }),
        ApprovalIntent::Refund(refund(rng, spec)),
        ApprovalIntent::CreditNote(credit_note(rng, spec)),
        ApprovalIntent::RefundWithCreditNote(RefundWithCreditNoteIntent {
            refund: refund(rng, spec),
            credit_note: credit_note(rng, spec),
        }),
        ApprovalIntent::ManualAdjustment(ManualAdjustmentIntent {
            tenant_id: rng.uuid(),
            payer_tenant_id: rng.flag().then(|| rng.uuid()),
            adjustment_id: rng.text("adj"),
            action: "ROUNDING_CORRECTION".to_owned(),
            currency: spec.clone(),
            legs: (0..rng.below(4))
                .map(|_| ManualLegIntent {
                    account_class: "SUSPENSE".to_owned(),
                    side: if rng.flag() { "DR" } else { "CR" }.to_owned(),
                    amount: rng.money(spec),
                    revenue_stream: rng.opt_text("stream"),
                })
                .collect(),
            tax: taxes(rng, spec),
            reason_code: rng.text("reason"),
            preparer_actor_id: rng.uuid(),
            approver_actor_id: rng.flag().then(|| rng.uuid()),
        }),
        ApprovalIntent::RecognitionScheduleChange(RecognitionScheduleChangeIntent {
            tenant_id: rng.uuid(),
            schedule_id: rng.text("sch"),
            change_id: rng.text("chg"),
            action: if rng.flag() { "cancel" } else { "replace" }.to_owned(),
            treatment: "PROSPECTIVE".to_owned(),
            new_segments: rng.flag().then(|| {
                (0..rng.below(4))
                    .map(|i| RecognitionChangeSegment {
                        period_id: format!("2026{:02}", i + 1),
                        amount: rng.money(spec),
                    })
                    .collect()
            }),
        }),
        ApprovalIntent::Reverse(ReverseIntent {
            entry_id: rng.uuid(),
            into_period_id: rng.opt_text("p"),
            effective_at: rng
                .flag()
                .then(|| chrono::NaiveDate::from_ymd_opt(2026, 10, 9).unwrap()),
            reason: rng.text("why"),
        }),
    ]
}

#[test]
fn generated_intents_round_trip_with_a_stable_identity() {
    let mut rng = Lcg(0x1d70_5eed);
    let mut checked = 0_u32;
    for round in 0..4 {
        for scale in 0..=28_u8 {
            let code = ["USD", "JPY", "KWD", "XTS"][round];
            let spec = CurrencySpec::try_new(code.to_owned(), scale).unwrap();
            for intent in generate(&mut rng, &spec) {
                let encoded = encode_intent(&intent).unwrap();
                let decoded = decode_intent(encoded.clone()).unwrap();
                assert_eq!(decoded, intent, "{encoded}");
                assert_eq!(encode_intent(&decoded).unwrap(), encoded);
                assert_eq!(
                    canonical_identity(&decoded).unwrap(),
                    canonical_identity(&intent).unwrap()
                );
                checked += 1;
            }
        }
    }
    assert_eq!(checked, 4 * 29 * 8);
}
