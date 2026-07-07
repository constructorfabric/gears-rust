//! Serializable replay payloads for dual-control approvals. Stored as the
//! `ledger_approval.intent` jsonb at create-pending time and replayed verbatim by
//! the `ApprovalExecutor` on approve — so the executed mutation is exactly the one
//! the preparer submitted (and edited on resubmit). Phase 1 covered the three
//! seams on the payments-and-allocation base (reverse / credit-grant /
//! chargeback-loss); Phase 2 adds payer-closure and material-backdating. The
//! period-reopen intent lands with Slice 7 (no reopen operation exists yet).

use std::str::FromStr;

use bss_ledger_sdk::{AccountClass, Side};
use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use toolkit_macros::domain_model;
use uuid::Uuid;

use super::ApprovalKind;
use crate::domain::adjustment::credit_note::CreditNoteRequest;
use crate::domain::adjustment::debit_note::DebitNoteRequest;
use crate::domain::adjustment::manual::{
    ManualAdjustmentAction, ManualAdjustmentRequest, ManualLeg,
};
use crate::domain::adjustment::refund::{
    RefundDirection, RefundPattern, RefundPhase, RefundRequest,
};
use crate::domain::error::DomainError;
use crate::domain::invoice::builder::{InvoiceItem, PostedInvoice, TaxBreakdown};
use crate::domain::recognition::input::{RecognitionInput, RecognitionTiming};

/// A governed mutation captured for later replay, discriminated by `kind`.
#[domain_model]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ApprovalIntent {
    Reverse(ReverseIntent),
    CreditGrant(CreditGrantIntent),
    ChargebackLoss(ChargebackLossIntent),
    PayerClosure(PayerClosureIntent),
    PeriodReopen(PeriodReopenIntent),
    MaterialBackdating(BackdatedPost),
    RecognitionScheduleChange(RecognitionScheduleChangeIntent),
    Refund(RefundIntent),
    ManualAdjustment(ManualAdjustmentIntent),
    CreditNote(CreditNoteIntent),
    DebitNote(DebitNoteIntent),
    /// An over-threshold refund-with-credit-note composite (K-3): carries BOTH
    /// snapshots so the approved replay re-drives the SAME atomic two-entry post.
    /// Stamped as [`ApprovalKind::Refund`] (it rides the refund's D2 grain), but the
    /// executor replays the composite, not a bare refund.
    RefundWithCreditNote(RefundWithCreditNoteIntent),
}

impl ApprovalIntent {
    /// The operation's TRANSACTION currency for the FX-aware dual-control threshold
    /// (DC10): the D2 threshold is held in the tenant's FUNCTIONAL (reporting)
    /// currency, so the dual-control gate translates the comparand from this
    /// currency before comparing. `None` for non-amount kinds (payer-closure /
    /// material-backdating) and for the two whose comparand is derived at gate time,
    /// so their currency is not carried on the stored intent (`Reverse` reads the
    /// original entry; `RecognitionScheduleChange` reads the schedule) — those keep
    /// the pre-FX transaction-currency comparand (single-currency-correct; a
    /// documented residual until the currency rides those intents).
    #[must_use]
    pub fn transaction_currency(&self) -> Option<&str> {
        match self {
            Self::Refund(i) => Some(&i.currency),
            Self::RefundWithCreditNote(i) => Some(&i.refund.currency),
            Self::CreditGrant(i) => Some(&i.currency),
            Self::ChargebackLoss(i) => Some(&i.currency),
            Self::ManualAdjustment(i) => Some(&i.currency),
            Self::CreditNote(i) => Some(&i.currency),
            Self::DebitNote(i) => Some(&i.currency),
            Self::Reverse(_)
            | Self::PayerClosure(_)
            | Self::MaterialBackdating(_)
            | Self::RecognitionScheduleChange(_)
            | Self::PeriodReopen(_) => None,
        }
    }
}

/// Replay payload for a fiscal-period reopen (Slice 7, design §7 / N-core-3): the
/// `CLOSED → REOPENED` transition (`fiscal_period` flipped back to `OPEN`) for the
/// `(tenant, legal_entity, period)` it targets. Always dual-control (policy
/// `requires_dual_control` returns `true` for `PeriodReopen`); the amount is
/// structural, so `amount_minor` / `currency` return `None`.
#[domain_model]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[allow(
    clippy::struct_field_names,
    reason = "(tenant, legal_entity, period) is the canonical fiscal-period coordinate; the _id suffix is the domain convention, not redundant naming"
)]
pub struct PeriodReopenIntent {
    pub tenant_id: Uuid,
    pub legal_entity_id: Uuid,
    pub period_id: String,
}

/// Replay payload for a reversal. The reversed amount is derived from the original
/// entry at gate time (so it is not carried here); tenant + actor come from the
/// approve request's `ctx`.
#[domain_model]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReverseIntent {
    pub entry_id: Uuid,
    pub into_period_id: Option<String>,
    pub effective_at: Option<NaiveDate>,
    pub reason: String,
}

/// Replay payload for a high-value reusable-credit grant.
#[domain_model]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreditGrantIntent {
    pub tenant_id: Uuid,
    pub payer_tenant_id: Uuid,
    pub credit_application_id: String,
    pub currency: String,
    pub amount_minor: i64,
    pub credit_grant_event_type: Option<String>,
}

/// Replay payload for a chargeback-loss (`LOST`) dispute phase.
#[domain_model]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChargebackLossIntent {
    pub tenant_id: Uuid,
    pub payer_tenant_id: Uuid,
    pub payment_id: String,
    pub dispute_id: String,
    pub invoice_id: Option<String>,
    pub cycle: i32,
    pub funds_at_open: String,
    pub disputed_amount_minor: i64,
    pub currency: String,
}

/// Replay payload for a payer-closure (sets `lifecycle_state = CLOSED`). The
/// `disposition` records the customer-balance election when closing with a
/// positive balance (design 01 §4.2); `tenant_id` is the seller ledger.
#[domain_model]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PayerClosureIntent {
    pub tenant_id: Uuid,
    pub payer_tenant_id: Uuid,
    pub closed_with_open_balance: bool,
    pub disposition: Option<String>,
}

/// Replay payload for an ASC 606 recognition-schedule change/cancel (Group H ×
/// dual-control). A plain-type mirror of the SDK `ChangeRecognitionSchedule`
/// command (no SDK import in `domain`): the api handler builds it from the command
/// at gate time, and the executor rebuilds the command from it on approve and
/// replays `change_recognition_schedule` (idempotent on `change_id`). The threshold
/// amount (the schedule's un-recognized deferred remainder) is read from the
/// schedule by the gate — like `Reverse` — so it is not carried here.
#[domain_model]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecognitionScheduleChangeIntent {
    pub tenant_id: Uuid,
    pub schedule_id: String,
    pub change_id: String,
    pub action: String,
    pub treatment: String,
    pub new_segments: Option<Vec<RecognitionChangeSegment>>,
}

/// One replacement segment in a [`RecognitionScheduleChangeIntent`] — the plain
/// mirror of the SDK `ChangeSegment` (`None`-list on a `cancel`).
#[domain_model]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecognitionChangeSegment {
    pub period_id: String,
    pub amount_minor: i64,
}

/// Replay payload for a high-value refund (Slice 3 Group D × dual-control,
/// design §4.4 / §1.4 D2). A plain-type serde mirror of the domain
/// [`RefundRequest`] (no SDK/enum-with-no-serde imported into the stored jsonb):
/// `phase` + `pattern` are stored as their stable `as_str` tokens and rebuilt via
/// `parse` on approve (precedent: `BackdatedInvoiceItem.account_class`). The whole
/// request is carried because a refund is gated BEFORE its post (the journal entry
/// does not exist at gate time — like `MaterialBackdating`, unlike `Reverse`); the
/// executor rebuilds the [`RefundRequest`] from this and re-drives
/// `RefundHandler::post_refund` idempotently (the engine's
/// `(tenant, REFUND, psp_refund_id:phase)` claim makes the replay at-most-once).
#[domain_model]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
// The `*_id` fields mirror the storage / domain column names verbatim (the same
// `allow` `RefundRequest` carries).
#[allow(clippy::struct_field_names)]
pub struct RefundIntent {
    pub tenant_id: Uuid,
    pub payer_tenant_id: Uuid,
    pub refund_id: String,
    pub psp_refund_id: String,
    /// The phase wire literal (`RefundPhase::as_str`), rebuilt via
    /// `RefundPhase::parse` on replay.
    pub phase: String,
    /// The pattern wire literal (`RefundPattern::as_str`), rebuilt via
    /// `RefundPattern::parse` on replay.
    pub pattern: String,
    pub payment_id: String,
    pub invoice_id: Option<String>,
    pub currency: String,
    pub amount_minor: i64,
    pub two_stage: bool,
    /// The prior refund this one claws back / extends (refund-of-refund, Group E);
    /// `None` for a first-order refund. Snapshotted verbatim so the approved replay
    /// re-drives the SAME self-link.
    pub relates_to_refund_id: Option<String>,
    /// The economic direction wire literal (`RefundDirection::as_str`), rebuilt via
    /// `RefundDirection::parse` on replay. Carries which money-out effect an
    /// over-threshold refund-of-refund had at gate time (claw-back vs outbound).
    pub direction: String,
}

impl From<&RefundRequest> for RefundIntent {
    fn from(req: &RefundRequest) -> Self {
        Self {
            tenant_id: req.tenant_id,
            payer_tenant_id: req.payer_tenant_id,
            refund_id: req.refund_id.clone(),
            psp_refund_id: req.psp_refund_id.clone(),
            phase: req.phase.as_str().to_owned(),
            pattern: req.pattern.as_str().to_owned(),
            payment_id: req.payment_id.clone(),
            invoice_id: req.invoice_id.clone(),
            currency: req.currency.clone(),
            amount_minor: req.amount_minor,
            two_stage: req.two_stage,
            relates_to_refund_id: req.relates_to_refund_id.clone(),
            direction: req.direction.as_str().to_owned(),
        }
    }
}

impl TryFrom<&RefundIntent> for RefundRequest {
    type Error = DomainError;

    fn try_from(i: &RefundIntent) -> Result<Self, Self::Error> {
        let phase = RefundPhase::parse(&i.phase).ok_or_else(|| {
            DomainError::Internal(format!("refund replay: unknown phase token {:?}", i.phase))
        })?;
        let pattern = RefundPattern::parse(&i.pattern).ok_or_else(|| {
            DomainError::Internal(format!(
                "refund replay: unknown pattern token {:?}",
                i.pattern
            ))
        })?;
        let direction = RefundDirection::parse(&i.direction).ok_or_else(|| {
            DomainError::Internal(format!(
                "refund replay: unknown direction token {:?}",
                i.direction
            ))
        })?;
        Ok(Self {
            tenant_id: i.tenant_id,
            payer_tenant_id: i.payer_tenant_id,
            refund_id: i.refund_id.clone(),
            psp_refund_id: i.psp_refund_id.clone(),
            phase,
            pattern,
            payment_id: i.payment_id.clone(),
            invoice_id: i.invoice_id.clone(),
            currency: i.currency.clone(),
            amount_minor: i.amount_minor,
            two_stage: i.two_stage,
            relates_to_refund_id: i.relates_to_refund_id.clone(),
            direction,
        })
    }
}

/// One leg of a [`ManualAdjustmentIntent`] — a plain-type serde mirror of the domain
/// [`ManualLeg`]. `account_class` + `side` are the SDK enums [`AccountClass`] /
/// [`Side`] (no serde — dylint DE0101), so they are stored as their stable `as_str`
/// tokens and rebuilt via `parse` (`FromStr`) on replay (precedent:
/// [`BackdatedInvoiceItem.account_class`](BackdatedInvoiceItem)).
#[domain_model]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManualLegIntent {
    /// The `AccountClass::as_str` token, rebuilt via `AccountClass::from_str` on
    /// replay.
    pub account_class: String,
    /// The `Side::as_str` token (`DR` / `CR`), rebuilt via `Side::from_str` on replay.
    pub side: String,
    pub amount_minor: i64,
    pub revenue_stream: Option<String>,
}

/// Replay payload for an over-threshold governed manual adjustment (Slice 3
/// Group 5 / Phase 3 governance, design §4.6 / §1.4 D2). A plain-type serde mirror
/// of the domain [`ManualAdjustmentRequest`] (no SDK/enum-with-no-serde stored in the
/// jsonb): `action` is stored as its `as_str` token and rebuilt via
/// `ManualAdjustmentAction::parse`, and each leg's `account_class` / `side` are
/// stored as their `as_str` tokens and rebuilt via `parse` (precedent:
/// [`RefundIntent`] / [`BackdatedInvoiceItem`]). The whole request is carried because
/// a manual adjustment is gated BEFORE its post (the journal entry does not exist at
/// gate time — like a refund); the executor rebuilds the [`ManualAdjustmentRequest`]
/// and re-drives [`post_manual_adjustment_approved`](crate::infra::adjustment::manual_adjustment_service::ManualAdjustmentHandler::post_manual_adjustment_approved)
/// idempotently (the engine's `(tenant, MANUAL_ADJUSTMENT, adjustment_id)` claim makes
/// the replay at-most-once).
///
/// **No `tax` field.** The MVP governed actions move no tax — `TAX_PAYABLE` is in NO
/// action's allow-list (so a governed manual adjustment can never carry a tax leg) —
/// so the snapshot does not store it; the rebuilt request restores `tax: Vec::new()`.
#[domain_model]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
// The `*_id` fields mirror the storage / domain column names verbatim.
#[allow(clippy::struct_field_names)]
pub struct ManualAdjustmentIntent {
    pub tenant_id: Uuid,
    pub payer_tenant_id: Option<Uuid>,
    pub adjustment_id: String,
    /// The action wire literal (`ManualAdjustmentAction::as_str`), rebuilt via
    /// `ManualAdjustmentAction::parse` on replay.
    pub action: String,
    pub currency: String,
    pub legs: Vec<ManualLegIntent>,
    pub reason_code: String,
    pub preparer_actor_id: Uuid,
    pub approver_actor_id: Option<Uuid>,
}

impl From<&ManualAdjustmentRequest> for ManualAdjustmentIntent {
    fn from(req: &ManualAdjustmentRequest) -> Self {
        Self {
            tenant_id: req.tenant_id,
            payer_tenant_id: req.payer_tenant_id,
            adjustment_id: req.adjustment_id.clone(),
            action: req.action.as_str().to_owned(),
            currency: req.currency.clone(),
            legs: req
                .legs
                .iter()
                .map(|l| ManualLegIntent {
                    account_class: l.account_class.as_str().to_owned(),
                    side: l.side.as_str().to_owned(),
                    amount_minor: l.amount_minor,
                    revenue_stream: l.revenue_stream.clone(),
                })
                .collect(),
            reason_code: req.reason_code.clone(),
            preparer_actor_id: req.preparer_actor_id,
            approver_actor_id: req.approver_actor_id,
        }
    }
}

impl TryFrom<&ManualAdjustmentIntent> for ManualAdjustmentRequest {
    type Error = DomainError;

    fn try_from(i: &ManualAdjustmentIntent) -> Result<Self, Self::Error> {
        let action = ManualAdjustmentAction::parse(&i.action).ok_or_else(|| {
            DomainError::Internal(format!(
                "manual adjustment replay: unknown action token {:?}",
                i.action
            ))
        })?;
        let legs = i
            .legs
            .iter()
            .map(|l| {
                let account_class = l.account_class.parse::<AccountClass>().map_err(|_| {
                    DomainError::Internal(format!(
                        "manual adjustment replay: unknown account_class token {:?}",
                        l.account_class
                    ))
                })?;
                let side = l.side.parse::<Side>().map_err(|_| {
                    DomainError::Internal(format!(
                        "manual adjustment replay: unknown side token {:?}",
                        l.side
                    ))
                })?;
                Ok(ManualLeg {
                    account_class,
                    side,
                    amount_minor: l.amount_minor,
                    revenue_stream: l.revenue_stream.clone(),
                })
            })
            .collect::<Result<Vec<_>, DomainError>>()?;
        Ok(Self {
            tenant_id: i.tenant_id,
            payer_tenant_id: i.payer_tenant_id,
            adjustment_id: i.adjustment_id.clone(),
            action,
            currency: i.currency.clone(),
            legs,
            reason_code: i.reason_code.clone(),
            preparer_actor_id: i.preparer_actor_id,
            approver_actor_id: i.approver_actor_id,
            // The MVP governed actions move no tax (TAX_PAYABLE is in no allow-list),
            // so the snapshot carries none — rebuild empty.
            tax: Vec::new(),
        })
    }
}

impl ManualAdjustmentIntent {
    /// Gross adjustment amount in minor units = `Σ DR` (== `Σ CR`; `govern` balanced
    /// the legs). `i128` fold to avoid an intermediate overflow, saturating at
    /// `i64::MAX` (the post / `govern` guards reject an out-of-i64 set). This is the
    /// D2 comparand — matching the gross the handler passes the gate (so the resubmit
    /// re-evaluation reads the same amount).
    fn gross_minor(&self) -> i64 {
        let dr_token = Side::Debit.as_str();
        let dr: i128 = self
            .legs
            .iter()
            .filter(|l| l.side == dr_token)
            .map(|l| i128::from(l.amount_minor))
            .sum();
        i64::try_from(dr).unwrap_or(i64::MAX)
    }
}

/// Replay payload for an over-threshold credit note (Slice 3 Phase 1 × dual-control,
/// design §5 D1–D2). A plain-type serde mirror of the domain [`CreditNoteRequest`]
/// (no SDK/enum-with-no-serde stored in the jsonb): the `tax` breakdown reuses the
/// [`BackdatedTaxBreakdown`] mirror. The whole request is carried because a credit
/// note is gated BEFORE its post (the journal entry does not exist at gate time —
/// like a refund); the executor rebuilds the [`CreditNoteRequest`] and re-drives
/// `post_credit_note_approved` idempotently (the engine's
/// `(tenant, CREDIT_NOTE, credit_note_id)` claim makes the replay at-most-once).
#[domain_model]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
// The `*_id` / `*_ref` / `*_group` fields mirror the domain column names verbatim.
#[allow(clippy::struct_field_names)]
pub struct CreditNoteIntent {
    pub tenant_id: Uuid,
    pub payer_tenant_id: Uuid,
    pub credit_note_id: String,
    pub origin_invoice_id: String,
    pub origin_invoice_item_ref: Option<String>,
    pub po_allocation_group: Option<String>,
    pub revenue_stream: String,
    pub currency: String,
    pub amount_minor: i64,
    pub tax_minor: i64,
    /// The authoritative tax breakdown dims, mirrored verbatim (sums to `tax_minor`).
    pub tax: Vec<BackdatedTaxBreakdown>,
    pub requested_deferred_minor: i64,
    pub reason_code: String,
    pub goodwill: bool,
}

impl From<&CreditNoteRequest> for CreditNoteIntent {
    fn from(req: &CreditNoteRequest) -> Self {
        Self {
            tenant_id: req.tenant_id,
            payer_tenant_id: req.payer_tenant_id,
            credit_note_id: req.credit_note_id.clone(),
            origin_invoice_id: req.origin_invoice_id.clone(),
            origin_invoice_item_ref: req.origin_invoice_item_ref.clone(),
            po_allocation_group: req.po_allocation_group.clone(),
            revenue_stream: req.revenue_stream.clone(),
            currency: req.currency.clone(),
            amount_minor: req.amount_minor,
            tax_minor: req.tax_minor,
            tax: req.tax.iter().map(BackdatedTaxBreakdown::from).collect(),
            requested_deferred_minor: req.requested_deferred_minor,
            reason_code: req.reason_code.clone(),
            goodwill: req.goodwill,
        }
    }
}

impl From<&CreditNoteIntent> for CreditNoteRequest {
    fn from(i: &CreditNoteIntent) -> Self {
        Self {
            tenant_id: i.tenant_id,
            payer_tenant_id: i.payer_tenant_id,
            credit_note_id: i.credit_note_id.clone(),
            origin_invoice_id: i.origin_invoice_id.clone(),
            origin_invoice_item_ref: i.origin_invoice_item_ref.clone(),
            po_allocation_group: i.po_allocation_group.clone(),
            revenue_stream: i.revenue_stream.clone(),
            currency: i.currency.clone(),
            amount_minor: i.amount_minor,
            tax_minor: i.tax_minor,
            tax: i.tax.iter().map(TaxBreakdown::from).collect(),
            requested_deferred_minor: i.requested_deferred_minor,
            reason_code: i.reason_code.clone(),
            goodwill: i.goodwill,
        }
    }
}

/// Replay payload for an over-threshold debit note (Slice 3 Phase 1 × dual-control,
/// design §5 D1–D2). A plain-type serde mirror of the domain [`DebitNoteRequest`];
/// the optional `recognition` spec (a deferred debit note builds a schedule) is
/// carried via [`DebitNoteRecognitionSnapshot`] so the approved replay rebuilds the
/// SAME schedule. The executor re-drives `post_debit_note_approved` idempotently
/// (the engine's `(tenant, DEBIT_NOTE, debit_note_id)` claim makes it at-most-once).
#[domain_model]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[allow(clippy::struct_field_names)]
pub struct DebitNoteIntent {
    pub tenant_id: Uuid,
    pub payer_tenant_id: Uuid,
    pub debit_note_id: String,
    pub origin_invoice_id: String,
    pub origin_invoice_item_ref: Option<String>,
    pub revenue_stream: String,
    pub currency: String,
    pub amount_minor: i64,
    pub tax_minor: i64,
    pub tax: Vec<BackdatedTaxBreakdown>,
    pub deferred_minor: i64,
    pub reason_code: String,
    /// The ASC 606 recognition spec for a deferred debit note (Slice 4); `None` for
    /// a fully-recognized note. Carried so a deferred over-D2 debit note rebuilds its
    /// schedule on the approved replay (faithful K-3-style replay).
    pub recognition: Option<DebitNoteRecognitionSnapshot>,
}

/// Serde mirror of [`RecognitionInput`] — primitives + the [`DebitNoteTimingSnapshot`]
/// timing mirror (the domain timing carries no serde). Round-trips losslessly.
#[domain_model]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DebitNoteRecognitionSnapshot {
    pub policy_ref: String,
    pub timing: DebitNoteTimingSnapshot,
    pub po_allocation_group: Option<String>,
    pub multi_po: bool,
    pub ssp_snapshot_ref: Option<String>,
    pub subscription_ref: Option<String>,
    pub vc_estimate_ref: Option<String>,
    pub vc_method_ref: Option<String>,
    pub immaterial_one_shot_sku: bool,
}

/// Serde mirror of [`RecognitionTiming`] (`POINT_IN_TIME` / `STRAIGHT_LINE { periods,
/// first_period_id }`).
#[domain_model]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "timing", rename_all = "SCREAMING_SNAKE_CASE")]
pub enum DebitNoteTimingSnapshot {
    PointInTime,
    StraightLine {
        periods: u32,
        first_period_id: Option<String>,
    },
}

impl From<&RecognitionTiming> for DebitNoteTimingSnapshot {
    fn from(t: &RecognitionTiming) -> Self {
        match t {
            RecognitionTiming::PointInTime => Self::PointInTime,
            RecognitionTiming::StraightLine {
                periods,
                first_period_id,
            } => Self::StraightLine {
                periods: *periods,
                first_period_id: first_period_id.clone(),
            },
        }
    }
}

impl From<&DebitNoteTimingSnapshot> for RecognitionTiming {
    fn from(t: &DebitNoteTimingSnapshot) -> Self {
        match t {
            DebitNoteTimingSnapshot::PointInTime => Self::PointInTime,
            DebitNoteTimingSnapshot::StraightLine {
                periods,
                first_period_id,
            } => Self::StraightLine {
                periods: *periods,
                first_period_id: first_period_id.clone(),
            },
        }
    }
}

impl From<&RecognitionInput> for DebitNoteRecognitionSnapshot {
    fn from(r: &RecognitionInput) -> Self {
        Self {
            policy_ref: r.policy_ref.clone(),
            timing: DebitNoteTimingSnapshot::from(&r.timing),
            po_allocation_group: r.po_allocation_group.clone(),
            multi_po: r.multi_po,
            ssp_snapshot_ref: r.ssp_snapshot_ref.clone(),
            subscription_ref: r.subscription_ref.clone(),
            vc_estimate_ref: r.vc_estimate_ref.clone(),
            vc_method_ref: r.vc_method_ref.clone(),
            immaterial_one_shot_sku: r.immaterial_one_shot_sku,
        }
    }
}

impl From<&DebitNoteRecognitionSnapshot> for RecognitionInput {
    fn from(s: &DebitNoteRecognitionSnapshot) -> Self {
        Self {
            policy_ref: s.policy_ref.clone(),
            timing: RecognitionTiming::from(&s.timing),
            po_allocation_group: s.po_allocation_group.clone(),
            multi_po: s.multi_po,
            ssp_snapshot_ref: s.ssp_snapshot_ref.clone(),
            subscription_ref: s.subscription_ref.clone(),
            vc_estimate_ref: s.vc_estimate_ref.clone(),
            vc_method_ref: s.vc_method_ref.clone(),
            immaterial_one_shot_sku: s.immaterial_one_shot_sku,
        }
    }
}

impl From<&DebitNoteRequest> for DebitNoteIntent {
    fn from(req: &DebitNoteRequest) -> Self {
        Self {
            tenant_id: req.tenant_id,
            payer_tenant_id: req.payer_tenant_id,
            debit_note_id: req.debit_note_id.clone(),
            origin_invoice_id: req.origin_invoice_id.clone(),
            origin_invoice_item_ref: req.origin_invoice_item_ref.clone(),
            revenue_stream: req.revenue_stream.clone(),
            currency: req.currency.clone(),
            amount_minor: req.amount_minor,
            tax_minor: req.tax_minor,
            tax: req.tax.iter().map(BackdatedTaxBreakdown::from).collect(),
            deferred_minor: req.deferred_minor,
            reason_code: req.reason_code.clone(),
            recognition: req
                .recognition
                .as_ref()
                .map(DebitNoteRecognitionSnapshot::from),
        }
    }
}

impl From<&DebitNoteIntent> for DebitNoteRequest {
    fn from(i: &DebitNoteIntent) -> Self {
        Self {
            tenant_id: i.tenant_id,
            payer_tenant_id: i.payer_tenant_id,
            debit_note_id: i.debit_note_id.clone(),
            origin_invoice_id: i.origin_invoice_id.clone(),
            origin_invoice_item_ref: i.origin_invoice_item_ref.clone(),
            revenue_stream: i.revenue_stream.clone(),
            currency: i.currency.clone(),
            amount_minor: i.amount_minor,
            tax_minor: i.tax_minor,
            tax: i.tax.iter().map(TaxBreakdown::from).collect(),
            deferred_minor: i.deferred_minor,
            reason_code: i.reason_code.clone(),
            recognition: i.recognition.as_ref().map(RecognitionInput::from),
        }
    }
}

/// Replay payload for an over-threshold refund-with-credit-note composite (Slice 3
/// Group G / K-3 × dual-control). Carries BOTH the refund and the credit-note
/// snapshots so the approved replay re-drives the SAME atomic two-entry post (the
/// refund's `(tenant, REFUND, psp_refund_id:phase)` claim is the composite's primary
/// idempotency grain). Stamped as [`ApprovalKind::Refund`] — it rides the refund's D2
/// grain — but the executor replays `post_refund_with_credit_note_approved`, NOT a
/// bare refund (the bug Z5-1 fixed: a plain `Refund` intent dropped the credit note).
#[domain_model]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RefundWithCreditNoteIntent {
    pub refund: RefundIntent,
    pub credit_note: CreditNoteIntent,
}

impl RefundWithCreditNoteIntent {
    /// Build from the two domain requests at gate time.
    #[must_use]
    pub fn from_requests(refund: &RefundRequest, credit_note: &CreditNoteRequest) -> Self {
        Self {
            refund: RefundIntent::from(refund),
            credit_note: CreditNoteIntent::from(credit_note),
        }
    }

    /// Rebuild the two domain requests on the approved replay. The refund half is
    /// fallible (its phase/pattern/direction tokens parse); the credit-note half is
    /// infallible.
    ///
    /// # Errors
    /// [`DomainError::Internal`] if a refund token fails to parse.
    pub fn to_requests(&self) -> Result<(RefundRequest, CreditNoteRequest), DomainError> {
        let refund = RefundRequest::try_from(&self.refund)?;
        let credit_note = CreditNoteRequest::from(&self.credit_note);
        Ok((refund, credit_note))
    }
}

/// A materially-backdated post captured for replay (design J). Backdating writes
/// a brand-new entry whose `effective_at` predates the tenant's A6 window — so at
/// gate time the ledger object does NOT exist yet (the gate fires *before* the
/// post), and the source invoice is external (pushed in the request body, never
/// pulled by the ledger). There is therefore no id to reference on approve: the
/// whole post is carried here. The variant discriminates which surface the
/// backdated post replays through; invoice-post is the first seam, and
/// settle / return / allocate / dispute / credit extend it (each gates the same
/// A6 way and replays its own command).
#[domain_model]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "post", rename_all = "SCREAMING_SNAKE_CASE")]
pub enum BackdatedPost {
    Invoice(BackdatedInvoiceSnapshot),
}

impl BackdatedPost {
    /// The external business id — the DC13 active-uniqueness key.
    fn business_id(&self) -> &str {
        match self {
            Self::Invoice(s) => &s.invoice_id,
        }
    }

    /// Gross minor of the backdated post (for the approval-queue display).
    fn gross_minor(&self) -> i64 {
        match self {
            Self::Invoice(s) => s.gross_minor(),
        }
    }

    /// The post currency, if any.
    fn currency(&self) -> Option<&str> {
        match self {
            Self::Invoice(s) => s.currency(),
        }
    }
}

/// Serializable mirror of [`PostedInvoice`] (`domain::invoice::builder`). The SDK
/// `PostedInvoice`/`InvoiceItem` carry `AccountClass`, a contract enum with no
/// serde (dylint DE0101), and a DTO may not live in `domain` (DE0301) — so the
/// replay payload mirrors the domain primitive here with serde-able fields,
/// storing each `AccountClass` as its stable `as_str` token (rebuilt via
/// `from_str` on replay). Precedent: `pending_event_queue.payload`.
#[domain_model]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackdatedInvoiceSnapshot {
    pub invoice_id: String,
    pub payer_tenant_id: Uuid,
    pub resource_tenant_id: Option<Uuid>,
    pub seller_tenant_id: Uuid,
    pub effective_at: NaiveDate,
    pub due_date: Option<NaiveDate>,
    pub period_id: String,
    pub items: Vec<BackdatedInvoiceItem>,
    pub tax: Vec<BackdatedTaxBreakdown>,
    pub posted_by_actor_id: Uuid,
    pub correlation_id: Uuid,
}

impl BackdatedInvoiceSnapshot {
    /// Gross receivable in minor units (`Σ items ex-tax + Σ tax`), mirroring
    /// [`PostedInvoice::gross_minor`] — `i128` fold to avoid an intermediate
    /// overflow, saturating at `i64::MAX` (the post guards reject an overflow).
    fn gross_minor(&self) -> i64 {
        let items: i128 = self
            .items
            .iter()
            .map(|i| i128::from(i.amount_minor_ex_tax))
            .sum();
        let tax: i128 = self.tax.iter().map(|t| i128::from(t.amount_minor)).sum();
        i64::try_from(items + tax).unwrap_or(i64::MAX)
    }

    /// The post currency — first item, else first tax breakdown.
    fn currency(&self) -> Option<&str> {
        self.items
            .first()
            .map(|i| i.currency.as_str())
            .or_else(|| self.tax.first().map(|t| t.currency.as_str()))
    }
}

/// Serializable mirror of [`InvoiceItem`] — the `account_class` fields are stored
/// as the `AccountClass::as_str` token (see [`BackdatedInvoiceSnapshot`]).
#[domain_model]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
// Field names mirror `InvoiceItem` / the storage columns verbatim.
#[allow(clippy::struct_field_names)]
pub struct BackdatedInvoiceItem {
    pub amount_minor_ex_tax: i64,
    pub currency: String,
    pub revenue_stream: String,
    pub catalog_class: Option<String>,
    pub contract_class: Option<String>,
    pub gl_code: Option<String>,
    pub invoice_item_ref: Option<String>,
    pub sku_or_plan_ref: Option<String>,
    pub price_id: Option<String>,
    pub pricing_snapshot_ref: Option<String>,
}

/// Serializable mirror of [`TaxBreakdown`] (all-primitive fields).
#[domain_model]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackdatedTaxBreakdown {
    pub amount_minor: i64,
    pub currency: String,
    pub tax_jurisdiction: String,
    pub tax_filing_period: String,
    pub tax_rate_ref: Option<String>,
}

impl From<&PostedInvoice> for BackdatedInvoiceSnapshot {
    fn from(inv: &PostedInvoice) -> Self {
        Self {
            invoice_id: inv.invoice_id.clone(),
            payer_tenant_id: inv.payer_tenant_id,
            resource_tenant_id: inv.resource_tenant_id,
            seller_tenant_id: inv.seller_tenant_id,
            effective_at: inv.effective_at,
            due_date: inv.due_date,
            period_id: inv.period_id.clone(),
            items: inv.items.iter().map(BackdatedInvoiceItem::from).collect(),
            tax: inv.tax.iter().map(BackdatedTaxBreakdown::from).collect(),
            posted_by_actor_id: inv.posted_by_actor_id,
            correlation_id: inv.correlation_id,
        }
    }
}

impl From<&InvoiceItem> for BackdatedInvoiceItem {
    fn from(i: &InvoiceItem) -> Self {
        Self {
            amount_minor_ex_tax: i.amount_minor_ex_tax,
            currency: i.currency.clone(),
            revenue_stream: i.revenue_stream.clone(),
            catalog_class: i.catalog_class.map(|c| c.as_str().to_owned()),
            contract_class: i.contract_class.map(|c| c.as_str().to_owned()),
            gl_code: i.gl_code.clone(),
            invoice_item_ref: i.invoice_item_ref.clone(),
            sku_or_plan_ref: i.sku_or_plan_ref.clone(),
            price_id: i.price_id.clone(),
            pricing_snapshot_ref: i.pricing_snapshot_ref.clone(),
        }
    }
}

impl From<&TaxBreakdown> for BackdatedTaxBreakdown {
    fn from(t: &TaxBreakdown) -> Self {
        Self {
            amount_minor: t.amount_minor,
            currency: t.currency.clone(),
            tax_jurisdiction: t.tax_jurisdiction.clone(),
            tax_filing_period: t.tax_filing_period.clone(),
            tax_rate_ref: t.tax_rate_ref.clone(),
        }
    }
}

impl TryFrom<&BackdatedInvoiceSnapshot> for PostedInvoice {
    type Error = DomainError;

    fn try_from(s: &BackdatedInvoiceSnapshot) -> Result<Self, Self::Error> {
        let items = s
            .items
            .iter()
            .map(InvoiceItem::try_from)
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Self {
            invoice_id: s.invoice_id.clone(),
            payer_tenant_id: s.payer_tenant_id,
            resource_tenant_id: s.resource_tenant_id,
            seller_tenant_id: s.seller_tenant_id,
            effective_at: s.effective_at,
            due_date: s.due_date,
            period_id: s.period_id.clone(),
            items,
            tax: s.tax.iter().map(TaxBreakdown::from).collect(),
            posted_by_actor_id: s.posted_by_actor_id,
            correlation_id: s.correlation_id,
        })
    }
}

impl TryFrom<&BackdatedInvoiceItem> for InvoiceItem {
    type Error = DomainError;

    fn try_from(i: &BackdatedInvoiceItem) -> Result<Self, Self::Error> {
        Ok(Self {
            amount_minor_ex_tax: i.amount_minor_ex_tax,
            // SEAM (dual-control × Slice 4 recognition): the backdating snapshot
            // `BackdatedInvoiceItem` predates ASC 606 recognition and does not
            // capture the deferred split / schedule spec, so a replayed backdated
            // post is treated as non-deferred — no recognition schedule is rebuilt.
            // To let a backdated post carry recognition, add `deferred_minor` +
            // `recognition` to the snapshot and thread them through here.
            deferred_minor: 0,
            currency: i.currency.clone(),
            revenue_stream: i.revenue_stream.clone(),
            catalog_class: parse_account_class(i.catalog_class.as_deref())?,
            contract_class: parse_account_class(i.contract_class.as_deref())?,
            gl_code: i.gl_code.clone(),
            recognition: None,
            invoice_item_ref: i.invoice_item_ref.clone(),
            sku_or_plan_ref: i.sku_or_plan_ref.clone(),
            price_id: i.price_id.clone(),
            pricing_snapshot_ref: i.pricing_snapshot_ref.clone(),
        })
    }
}

impl From<&BackdatedTaxBreakdown> for TaxBreakdown {
    fn from(t: &BackdatedTaxBreakdown) -> Self {
        Self {
            amount_minor: t.amount_minor,
            currency: t.currency.clone(),
            tax_jurisdiction: t.tax_jurisdiction.clone(),
            tax_filing_period: t.tax_filing_period.clone(),
            tax_rate_ref: t.tax_rate_ref.clone(),
        }
    }
}

/// Parse a stored `AccountClass` token back to the enum (`None` passes through).
/// A corrupt token fails the replay rather than silently dropping the mapping.
fn parse_account_class(token: Option<&str>) -> Result<Option<AccountClass>, DomainError> {
    token
        .map(|t| {
            AccountClass::from_str(t).map_err(|e| {
                DomainError::Internal(format!(
                    "backdating replay: unknown account_class {t:?}: {e}"
                ))
            })
        })
        .transpose()
}

impl ApprovalIntent {
    /// The discriminator stamped on the `ledger_approval` row.
    #[must_use]
    pub fn kind(&self) -> ApprovalKind {
        match self {
            Self::Reverse(_) => ApprovalKind::Reverse,
            Self::CreditGrant(_) => ApprovalKind::CreditGrant,
            Self::ChargebackLoss(_) => ApprovalKind::ChargebackLoss,
            Self::PayerClosure(_) => ApprovalKind::PayerClosure,
            Self::PeriodReopen(_) => ApprovalKind::PeriodReopen,
            Self::MaterialBackdating(_) => ApprovalKind::MaterialBackdating,
            Self::RecognitionScheduleChange(_) => ApprovalKind::RecognitionScheduleChange,
            // The composite rides the refund's D2 grain → stamped REFUND (no new
            // kind), so it shares the `Refund` arm.
            Self::Refund(_) | Self::RefundWithCreditNote(_) => ApprovalKind::Refund,
            Self::ManualAdjustment(_) => ApprovalKind::ManualAdjustment,
            Self::CreditNote(_) => ApprovalKind::CreditNote,
            Self::DebitNote(_) => ApprovalKind::DebitNote,
        }
    }

    /// The idempotency/business key for the active-uniqueness guard (DC13): one
    /// active approval per `(tenant, kind, business_key)`.
    #[must_use]
    pub fn business_key(&self) -> String {
        match self {
            Self::Reverse(i) => i.entry_id.to_string(),
            Self::CreditGrant(i) => i.credit_application_id.clone(),
            Self::ChargebackLoss(i) => format!("{}:{}:LOST", i.dispute_id, i.cycle),
            Self::PayerClosure(i) => i.payer_tenant_id.to_string(),
            Self::PeriodReopen(i) => format!("{}:{}", i.legal_entity_id, i.period_id),
            Self::MaterialBackdating(p) => p.business_id().to_owned(),
            // One active approval per change request (the change-service is itself
            // idempotent on `change_id`).
            Self::RecognitionScheduleChange(i) => i.change_id.clone(),
            // Key on the engine's exact idempotency grain (`psp_refund_id:phase`):
            // one active approval per PSP-refund phase, so the DC13 active-uniqueness
            // slot lines up 1:1 with the at-most-once posting claim the executor
            // replays against.
            Self::Refund(i) => format!("{}:{}", i.psp_refund_id, i.phase),
            // One active approval per adjustment id — lines up 1:1 with the engine's
            // `(tenant, MANUAL_ADJUSTMENT, adjustment_id)` at-most-once posting claim
            // the executor replays against.
            Self::ManualAdjustment(i) => i.adjustment_id.clone(),
            // One active approval per note id — lines up 1:1 with the engine's
            // `(tenant, {CREDIT,DEBIT}_NOTE, note_id)` at-most-once posting claim.
            Self::CreditNote(i) => i.credit_note_id.clone(),
            Self::DebitNote(i) => i.debit_note_id.clone(),
            // Keyed on the refund grain (`psp_refund_id:phase`), same as a plain
            // refund — so a plain refund and a composite for the same PSP-refund
            // phase cannot both be active at once.
            Self::RefundWithCreditNote(i) => {
                format!("{}:{}", i.refund.psp_refund_id, i.refund.phase)
            }
        }
    }

    /// The native-currency minor amount for amount-gated kinds (credit-grant,
    /// chargeback-loss, refund, manual-adjustment, material-backdating). `Reverse` /
    /// `PayerClosure` / `RecognitionScheduleChange` return `None` — their threshold
    /// amount is derived/structural (read from the entry/schedule by the gate), not
    /// carried in the intent.
    #[must_use]
    pub fn amount_minor(&self) -> Option<i64> {
        match self {
            Self::Reverse(_)
            | Self::PayerClosure(_)
            | Self::PeriodReopen(_)
            | Self::RecognitionScheduleChange(_) => None,
            Self::CreditGrant(i) => Some(i.amount_minor),
            Self::ChargebackLoss(i) => Some(i.disputed_amount_minor),
            Self::MaterialBackdating(p) => Some(p.gross_minor()),
            Self::Refund(i) => Some(i.amount_minor),
            // The gross adjustment amount = Σ DR (== Σ CR; govern balanced the legs).
            // `i128` fold then saturate (the post / govern guards reject an out-of-i64
            // set) — matches the D2 comparand the handler passes the gate.
            Self::ManualAdjustment(i) => Some(i.gross_minor()),
            Self::CreditNote(i) => Some(i.amount_minor),
            Self::DebitNote(i) => Some(i.amount_minor),
            // The composite gates on its refund leg's cash amount (the credit note
            // rides the same approval).
            Self::RefundWithCreditNote(i) => Some(i.refund.amount_minor),
        }
    }

    /// The operation currency for amount-gated kinds (for the USD-eq conversion,
    /// DC10). `Reverse` carries no currency here (derived from the original entry).
    #[must_use]
    pub fn currency(&self) -> Option<&str> {
        match self {
            Self::Reverse(_)
            | Self::PayerClosure(_)
            | Self::PeriodReopen(_)
            | Self::RecognitionScheduleChange(_) => None,
            Self::CreditGrant(i) => Some(&i.currency),
            Self::ChargebackLoss(i) => Some(&i.currency),
            Self::MaterialBackdating(p) => p.currency(),
            Self::Refund(i) => Some(&i.currency),
            Self::ManualAdjustment(i) => Some(&i.currency),
            Self::CreditNote(i) => Some(&i.currency),
            Self::DebitNote(i) => Some(&i.currency),
            Self::RefundWithCreditNote(i) => Some(&i.refund.currency),
        }
    }

    /// True iff `other` addresses the SAME approval target as `self` — same kind
    /// and the same immutable recipient/target fields. The ONLY field a resubmit
    /// (DC17) may edit is the scalar approval amount on the amount-bearing kinds
    /// (`CreditGrant.amount_minor` / `ChargebackLoss.disputed_amount_minor`); every
    /// other field (recipient tenant, entry / application / dispute / payment /
    /// schedule id, currency, …) is pinned. So a resubmit can lower the amount on
    /// rework but CANNOT swap the recipient under the still-frozen `business_key`:
    /// a credit-grant's `payer_tenant_id` is orthogonal to its
    /// `business_key` (`credit_application_id`), so without this a recipient swap
    /// would clear the `kind` + `business_key` guards undetected. Kinds whose amount
    /// is derived/structural (`Reverse`, `PayerClosure`, `RecognitionScheduleChange`,
    /// `MaterialBackdating`) are compared whole.
    #[must_use]
    pub fn same_target(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::CreditGrant(a), Self::CreditGrant(b)) => {
                CreditGrantIntent {
                    amount_minor: 0,
                    ..a.clone()
                } == CreditGrantIntent {
                    amount_minor: 0,
                    ..b.clone()
                }
            }
            (Self::ChargebackLoss(a), Self::ChargebackLoss(b)) => {
                ChargebackLossIntent {
                    disputed_amount_minor: 0,
                    ..a.clone()
                } == ChargebackLossIntent {
                    disputed_amount_minor: 0,
                    ..b.clone()
                }
            }
            // The remaining kinds carry no editable scalar amount (it is derived or
            // structural), so the whole intent is the pinned target identity.
            _ => self == other,
        }
    }
}

#[cfg(test)]
#[path = "intent_tests.rs"]
mod tests;
