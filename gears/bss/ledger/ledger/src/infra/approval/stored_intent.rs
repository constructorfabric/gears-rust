//! Explicit persisted and wire approval payloads. Domain money never gains Serde.
use crate::domain::approval::intent as domain;
use crate::domain::error::DomainError;
use crate::domain::exact_money::map_money_error;
use crate::infra::storage::money_text::StoredMoney;
use bss_ledger_sdk::{CurrencySpec, PostedMoney};
use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Explicit stored currency metadata for a manual request with possibly no legs.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StoredCurrencySpec {
    pub currency: String,
    pub currency_scale: u8,
}
impl From<&CurrencySpec> for StoredCurrencySpec {
    fn from(v: &CurrencySpec) -> Self {
        Self {
            currency: v.code().into(),
            currency_scale: v.scale(),
        }
    }
}
impl TryFrom<StoredCurrencySpec> for CurrencySpec {
    type Error = DomainError;
    fn try_from(v: StoredCurrencySpec) -> Result<Self, Self::Error> {
        Self::try_new(v.currency, v.currency_scale).map_err(map_money_error)
    }
}

/// Explicit durable mirror of [`domain::PeriodReopenIntent`].
#[derive(Clone, Debug, Serialize, Deserialize)]
#[allow(
    clippy::struct_field_names,
    reason = "mirrors the domain `PeriodReopenIntent` field names on the durable wire contract"
)]
pub struct StoredPeriodReopenIntent {
    pub tenant_id: Uuid,
    pub legal_entity_id: Uuid,
    pub period_id: String,
}
impl From<&domain::PeriodReopenIntent> for StoredPeriodReopenIntent {
    fn from(v: &domain::PeriodReopenIntent) -> Self {
        Self {
            tenant_id: v.tenant_id,
            legal_entity_id: v.legal_entity_id,
            period_id: v.period_id.clone(),
        }
    }
}
impl TryFrom<StoredPeriodReopenIntent> for domain::PeriodReopenIntent {
    type Error = DomainError;
    fn try_from(v: StoredPeriodReopenIntent) -> Result<Self, Self::Error> {
        Ok(Self {
            tenant_id: v.tenant_id,
            legal_entity_id: v.legal_entity_id,
            period_id: v.period_id,
        })
    }
}

/// Explicit durable mirror of [`domain::ReverseIntent`].
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StoredReverseIntent {
    pub entry_id: Uuid,
    pub into_period_id: Option<String>,
    pub effective_at: Option<NaiveDate>,
    pub reason: String,
}
impl From<&domain::ReverseIntent> for StoredReverseIntent {
    fn from(v: &domain::ReverseIntent) -> Self {
        Self {
            entry_id: v.entry_id,
            into_period_id: v.into_period_id.clone(),
            effective_at: v.effective_at,
            reason: v.reason.clone(),
        }
    }
}
impl TryFrom<StoredReverseIntent> for domain::ReverseIntent {
    type Error = DomainError;
    fn try_from(v: StoredReverseIntent) -> Result<Self, Self::Error> {
        Ok(Self {
            entry_id: v.entry_id,
            into_period_id: v.into_period_id,
            effective_at: v.effective_at,
            reason: v.reason,
        })
    }
}

/// Explicit durable mirror of [`domain::CreditGrantIntent`].
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StoredCreditGrantIntent {
    pub tenant_id: Uuid,
    pub payer_tenant_id: Uuid,
    pub credit_application_id: String,
    pub amount: StoredMoney,
    pub credit_grant_event_type: Option<String>,
}
impl From<&domain::CreditGrantIntent> for StoredCreditGrantIntent {
    fn from(v: &domain::CreditGrantIntent) -> Self {
        Self {
            tenant_id: v.tenant_id,
            payer_tenant_id: v.payer_tenant_id,
            credit_application_id: v.credit_application_id.clone(),
            amount: StoredMoney::from(&v.amount),
            credit_grant_event_type: v.credit_grant_event_type.clone(),
        }
    }
}
impl TryFrom<StoredCreditGrantIntent> for domain::CreditGrantIntent {
    type Error = DomainError;
    fn try_from(v: StoredCreditGrantIntent) -> Result<Self, Self::Error> {
        Ok(Self {
            tenant_id: v.tenant_id,
            payer_tenant_id: v.payer_tenant_id,
            credit_application_id: v.credit_application_id,
            amount: PostedMoney::try_from(v.amount).map_err(map_money_error)?,
            credit_grant_event_type: v.credit_grant_event_type,
        })
    }
}

/// Explicit durable mirror of [`domain::ChargebackLossIntent`].
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StoredChargebackLossIntent {
    pub tenant_id: Uuid,
    pub payer_tenant_id: Uuid,
    pub payment_id: String,
    pub dispute_id: String,
    pub invoice_id: Option<String>,
    pub cycle: i32,
    pub funds_at_open: String,
    pub disputed_amount: StoredMoney,
}
impl From<&domain::ChargebackLossIntent> for StoredChargebackLossIntent {
    fn from(v: &domain::ChargebackLossIntent) -> Self {
        Self {
            tenant_id: v.tenant_id,
            payer_tenant_id: v.payer_tenant_id,
            payment_id: v.payment_id.clone(),
            dispute_id: v.dispute_id.clone(),
            invoice_id: v.invoice_id.clone(),
            cycle: v.cycle,
            funds_at_open: v.funds_at_open.clone(),
            disputed_amount: StoredMoney::from(&v.disputed_amount),
        }
    }
}
impl TryFrom<StoredChargebackLossIntent> for domain::ChargebackLossIntent {
    type Error = DomainError;
    fn try_from(v: StoredChargebackLossIntent) -> Result<Self, Self::Error> {
        Ok(Self {
            tenant_id: v.tenant_id,
            payer_tenant_id: v.payer_tenant_id,
            payment_id: v.payment_id,
            dispute_id: v.dispute_id,
            invoice_id: v.invoice_id,
            cycle: v.cycle,
            funds_at_open: v.funds_at_open,
            disputed_amount: PostedMoney::try_from(v.disputed_amount).map_err(map_money_error)?,
        })
    }
}

/// Explicit durable mirror of [`domain::PayerClosureIntent`].
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StoredPayerClosureIntent {
    pub tenant_id: Uuid,
    pub payer_tenant_id: Uuid,
    pub closed_with_open_balance: bool,
    pub disposition: Option<String>,
}
impl From<&domain::PayerClosureIntent> for StoredPayerClosureIntent {
    fn from(v: &domain::PayerClosureIntent) -> Self {
        Self {
            tenant_id: v.tenant_id,
            payer_tenant_id: v.payer_tenant_id,
            closed_with_open_balance: v.closed_with_open_balance,
            disposition: v.disposition.clone(),
        }
    }
}
impl TryFrom<StoredPayerClosureIntent> for domain::PayerClosureIntent {
    type Error = DomainError;
    fn try_from(v: StoredPayerClosureIntent) -> Result<Self, Self::Error> {
        Ok(Self {
            tenant_id: v.tenant_id,
            payer_tenant_id: v.payer_tenant_id,
            closed_with_open_balance: v.closed_with_open_balance,
            disposition: v.disposition,
        })
    }
}

/// Explicit durable mirror of [`domain::RecognitionScheduleChangeIntent`].
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StoredRecognitionScheduleChangeIntent {
    pub tenant_id: Uuid,
    pub schedule_id: String,
    pub change_id: String,
    pub action: String,
    pub treatment: String,
    pub new_segments: Option<Vec<StoredRecognitionChangeSegment>>,
}
impl From<&domain::RecognitionScheduleChangeIntent> for StoredRecognitionScheduleChangeIntent {
    fn from(v: &domain::RecognitionScheduleChangeIntent) -> Self {
        Self {
            tenant_id: v.tenant_id,
            schedule_id: v.schedule_id.clone(),
            change_id: v.change_id.clone(),
            action: v.action.clone(),
            treatment: v.treatment.clone(),
            new_segments: v.new_segments.as_ref().map(|values| {
                values
                    .iter()
                    .map(StoredRecognitionChangeSegment::from)
                    .collect()
            }),
        }
    }
}
impl TryFrom<StoredRecognitionScheduleChangeIntent> for domain::RecognitionScheduleChangeIntent {
    type Error = DomainError;
    fn try_from(v: StoredRecognitionScheduleChangeIntent) -> Result<Self, Self::Error> {
        Ok(Self {
            tenant_id: v.tenant_id,
            schedule_id: v.schedule_id,
            change_id: v.change_id,
            action: v.action,
            treatment: v.treatment,
            new_segments: v
                .new_segments
                .map(|values| {
                    values
                        .into_iter()
                        .map(domain::RecognitionChangeSegment::try_from)
                        .collect::<Result<Vec<_>, _>>()
                })
                .transpose()?,
        })
    }
}

/// Explicit durable mirror of [`domain::RecognitionChangeSegment`].
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StoredRecognitionChangeSegment {
    pub period_id: String,
    pub amount: StoredMoney,
}
impl From<&domain::RecognitionChangeSegment> for StoredRecognitionChangeSegment {
    fn from(v: &domain::RecognitionChangeSegment) -> Self {
        Self {
            period_id: v.period_id.clone(),
            amount: StoredMoney::from(&v.amount),
        }
    }
}
impl TryFrom<StoredRecognitionChangeSegment> for domain::RecognitionChangeSegment {
    type Error = DomainError;
    fn try_from(v: StoredRecognitionChangeSegment) -> Result<Self, Self::Error> {
        Ok(Self {
            period_id: v.period_id,
            amount: PostedMoney::try_from(v.amount).map_err(map_money_error)?,
        })
    }
}

/// Explicit durable mirror of [`domain::RefundIntent`].
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StoredRefundIntent {
    pub tenant_id: Uuid,
    pub payer_tenant_id: Uuid,
    pub refund_id: String,
    pub psp_refund_id: String,
    pub phase: String,
    pub pattern: String,
    pub payment_id: String,
    pub invoice_id: Option<String>,
    pub amount: StoredMoney,
    pub two_stage: bool,
    pub relates_to_refund_id: Option<String>,
    pub direction: String,
}
impl From<&domain::RefundIntent> for StoredRefundIntent {
    fn from(v: &domain::RefundIntent) -> Self {
        Self {
            tenant_id: v.tenant_id,
            payer_tenant_id: v.payer_tenant_id,
            refund_id: v.refund_id.clone(),
            psp_refund_id: v.psp_refund_id.clone(),
            phase: v.phase.clone(),
            pattern: v.pattern.clone(),
            payment_id: v.payment_id.clone(),
            invoice_id: v.invoice_id.clone(),
            amount: StoredMoney::from(&v.amount),
            two_stage: v.two_stage,
            relates_to_refund_id: v.relates_to_refund_id.clone(),
            direction: v.direction.clone(),
        }
    }
}
impl TryFrom<StoredRefundIntent> for domain::RefundIntent {
    type Error = DomainError;
    fn try_from(v: StoredRefundIntent) -> Result<Self, Self::Error> {
        Ok(Self {
            tenant_id: v.tenant_id,
            payer_tenant_id: v.payer_tenant_id,
            refund_id: v.refund_id,
            psp_refund_id: v.psp_refund_id,
            phase: v.phase,
            pattern: v.pattern,
            payment_id: v.payment_id,
            invoice_id: v.invoice_id,
            amount: PostedMoney::try_from(v.amount).map_err(map_money_error)?,
            two_stage: v.two_stage,
            relates_to_refund_id: v.relates_to_refund_id,
            direction: v.direction,
        })
    }
}

/// Explicit durable mirror of [`domain::ManualLegIntent`].
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StoredManualLegIntent {
    pub account_class: String,
    pub side: String,
    pub amount: StoredMoney,
    pub revenue_stream: Option<String>,
}
impl From<&domain::ManualLegIntent> for StoredManualLegIntent {
    fn from(v: &domain::ManualLegIntent) -> Self {
        Self {
            account_class: v.account_class.clone(),
            side: v.side.clone(),
            amount: StoredMoney::from(&v.amount),
            revenue_stream: v.revenue_stream.clone(),
        }
    }
}
impl TryFrom<StoredManualLegIntent> for domain::ManualLegIntent {
    type Error = DomainError;
    fn try_from(v: StoredManualLegIntent) -> Result<Self, Self::Error> {
        Ok(Self {
            account_class: v.account_class,
            side: v.side,
            amount: PostedMoney::try_from(v.amount).map_err(map_money_error)?,
            revenue_stream: v.revenue_stream,
        })
    }
}

/// Explicit durable mirror of [`domain::ManualAdjustmentIntent`].
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StoredManualAdjustmentIntent {
    pub tenant_id: Uuid,
    pub payer_tenant_id: Option<Uuid>,
    pub adjustment_id: String,
    pub action: String,
    pub currency: StoredCurrencySpec,
    pub legs: Vec<StoredManualLegIntent>,
    pub tax: Vec<StoredBackdatedTaxBreakdown>,
    pub reason_code: String,
    pub preparer_actor_id: Uuid,
    pub approver_actor_id: Option<Uuid>,
}
impl From<&domain::ManualAdjustmentIntent> for StoredManualAdjustmentIntent {
    fn from(v: &domain::ManualAdjustmentIntent) -> Self {
        Self {
            tenant_id: v.tenant_id,
            payer_tenant_id: v.payer_tenant_id,
            adjustment_id: v.adjustment_id.clone(),
            action: v.action.clone(),
            currency: StoredCurrencySpec::from(&v.currency),
            legs: v.legs.iter().map(StoredManualLegIntent::from).collect(),
            tax: v
                .tax
                .iter()
                .map(StoredBackdatedTaxBreakdown::from)
                .collect(),
            reason_code: v.reason_code.clone(),
            preparer_actor_id: v.preparer_actor_id,
            approver_actor_id: v.approver_actor_id,
        }
    }
}
impl TryFrom<StoredManualAdjustmentIntent> for domain::ManualAdjustmentIntent {
    type Error = DomainError;
    fn try_from(v: StoredManualAdjustmentIntent) -> Result<Self, Self::Error> {
        Ok(Self {
            tenant_id: v.tenant_id,
            payer_tenant_id: v.payer_tenant_id,
            adjustment_id: v.adjustment_id,
            action: v.action,
            currency: CurrencySpec::try_from(v.currency)?,
            legs: v
                .legs
                .into_iter()
                .map(domain::ManualLegIntent::try_from)
                .collect::<Result<Vec<_>, _>>()?,
            tax: v
                .tax
                .into_iter()
                .map(domain::BackdatedTaxBreakdown::try_from)
                .collect::<Result<Vec<_>, _>>()?,
            reason_code: v.reason_code,
            preparer_actor_id: v.preparer_actor_id,
            approver_actor_id: v.approver_actor_id,
        })
    }
}

/// Explicit durable mirror of [`domain::CreditNoteIntent`].
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StoredCreditNoteIntent {
    pub tenant_id: Uuid,
    pub payer_tenant_id: Uuid,
    pub credit_note_id: String,
    pub origin_invoice_id: String,
    pub origin_invoice_item_ref: Option<String>,
    pub po_allocation_group: Option<String>,
    pub revenue_stream: String,
    pub amount: StoredMoney,
    pub tax_amount: StoredMoney,
    pub tax: Vec<StoredBackdatedTaxBreakdown>,
    pub requested_deferred: StoredMoney,
    pub reason_code: String,
    pub goodwill: bool,
}
impl From<&domain::CreditNoteIntent> for StoredCreditNoteIntent {
    fn from(v: &domain::CreditNoteIntent) -> Self {
        Self {
            tenant_id: v.tenant_id,
            payer_tenant_id: v.payer_tenant_id,
            credit_note_id: v.credit_note_id.clone(),
            origin_invoice_id: v.origin_invoice_id.clone(),
            origin_invoice_item_ref: v.origin_invoice_item_ref.clone(),
            po_allocation_group: v.po_allocation_group.clone(),
            revenue_stream: v.revenue_stream.clone(),
            amount: StoredMoney::from(&v.amount),
            tax_amount: StoredMoney::from(&v.tax_amount),
            tax: v
                .tax
                .iter()
                .map(StoredBackdatedTaxBreakdown::from)
                .collect(),
            requested_deferred: StoredMoney::from(&v.requested_deferred),
            reason_code: v.reason_code.clone(),
            goodwill: v.goodwill,
        }
    }
}
impl TryFrom<StoredCreditNoteIntent> for domain::CreditNoteIntent {
    type Error = DomainError;
    fn try_from(v: StoredCreditNoteIntent) -> Result<Self, Self::Error> {
        Ok(Self {
            tenant_id: v.tenant_id,
            payer_tenant_id: v.payer_tenant_id,
            credit_note_id: v.credit_note_id,
            origin_invoice_id: v.origin_invoice_id,
            origin_invoice_item_ref: v.origin_invoice_item_ref,
            po_allocation_group: v.po_allocation_group,
            revenue_stream: v.revenue_stream,
            amount: PostedMoney::try_from(v.amount).map_err(map_money_error)?,
            tax_amount: PostedMoney::try_from(v.tax_amount).map_err(map_money_error)?,
            tax: v
                .tax
                .into_iter()
                .map(domain::BackdatedTaxBreakdown::try_from)
                .collect::<Result<Vec<_>, _>>()?,
            requested_deferred: PostedMoney::try_from(v.requested_deferred)
                .map_err(map_money_error)?,
            reason_code: v.reason_code,
            goodwill: v.goodwill,
        })
    }
}

/// Explicit durable mirror of [`domain::DebitNoteIntent`].
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StoredDebitNoteIntent {
    pub tenant_id: Uuid,
    pub payer_tenant_id: Uuid,
    pub debit_note_id: String,
    pub origin_invoice_id: String,
    pub origin_invoice_item_ref: Option<String>,
    pub revenue_stream: String,
    pub amount: StoredMoney,
    pub tax_amount: StoredMoney,
    pub tax: Vec<StoredBackdatedTaxBreakdown>,
    pub deferred: StoredMoney,
    pub reason_code: String,
    pub recognition: Option<StoredDebitNoteRecognitionSnapshot>,
}
impl From<&domain::DebitNoteIntent> for StoredDebitNoteIntent {
    fn from(v: &domain::DebitNoteIntent) -> Self {
        Self {
            tenant_id: v.tenant_id,
            payer_tenant_id: v.payer_tenant_id,
            debit_note_id: v.debit_note_id.clone(),
            origin_invoice_id: v.origin_invoice_id.clone(),
            origin_invoice_item_ref: v.origin_invoice_item_ref.clone(),
            revenue_stream: v.revenue_stream.clone(),
            amount: StoredMoney::from(&v.amount),
            tax_amount: StoredMoney::from(&v.tax_amount),
            tax: v
                .tax
                .iter()
                .map(StoredBackdatedTaxBreakdown::from)
                .collect(),
            deferred: StoredMoney::from(&v.deferred),
            reason_code: v.reason_code.clone(),
            recognition: v
                .recognition
                .as_ref()
                .map(StoredDebitNoteRecognitionSnapshot::from),
        }
    }
}
impl TryFrom<StoredDebitNoteIntent> for domain::DebitNoteIntent {
    type Error = DomainError;
    fn try_from(v: StoredDebitNoteIntent) -> Result<Self, Self::Error> {
        Ok(Self {
            tenant_id: v.tenant_id,
            payer_tenant_id: v.payer_tenant_id,
            debit_note_id: v.debit_note_id,
            origin_invoice_id: v.origin_invoice_id,
            origin_invoice_item_ref: v.origin_invoice_item_ref,
            revenue_stream: v.revenue_stream,
            amount: PostedMoney::try_from(v.amount).map_err(map_money_error)?,
            tax_amount: PostedMoney::try_from(v.tax_amount).map_err(map_money_error)?,
            tax: v
                .tax
                .into_iter()
                .map(domain::BackdatedTaxBreakdown::try_from)
                .collect::<Result<Vec<_>, _>>()?,
            deferred: PostedMoney::try_from(v.deferred).map_err(map_money_error)?,
            reason_code: v.reason_code,
            recognition: v
                .recognition
                .map(domain::DebitNoteRecognitionSnapshot::try_from)
                .transpose()?,
        })
    }
}

/// Explicit durable mirror of [`domain::DebitNoteRecognitionSnapshot`].
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StoredDebitNoteRecognitionSnapshot {
    pub policy_ref: String,
    pub timing: StoredDebitNoteTimingSnapshot,
    pub po_allocation_group: Option<String>,
    pub multi_po: bool,
    pub ssp_snapshot_ref: Option<String>,
    pub subscription_ref: Option<String>,
    pub vc_estimate_ref: Option<String>,
    pub vc_method_ref: Option<String>,
    pub immaterial_one_shot_sku: bool,
}
impl From<&domain::DebitNoteRecognitionSnapshot> for StoredDebitNoteRecognitionSnapshot {
    fn from(v: &domain::DebitNoteRecognitionSnapshot) -> Self {
        Self {
            policy_ref: v.policy_ref.clone(),
            timing: StoredDebitNoteTimingSnapshot::from(&v.timing),
            po_allocation_group: v.po_allocation_group.clone(),
            multi_po: v.multi_po,
            ssp_snapshot_ref: v.ssp_snapshot_ref.clone(),
            subscription_ref: v.subscription_ref.clone(),
            vc_estimate_ref: v.vc_estimate_ref.clone(),
            vc_method_ref: v.vc_method_ref.clone(),
            immaterial_one_shot_sku: v.immaterial_one_shot_sku,
        }
    }
}
impl TryFrom<StoredDebitNoteRecognitionSnapshot> for domain::DebitNoteRecognitionSnapshot {
    type Error = DomainError;
    fn try_from(v: StoredDebitNoteRecognitionSnapshot) -> Result<Self, Self::Error> {
        Ok(Self {
            policy_ref: v.policy_ref,
            timing: domain::DebitNoteTimingSnapshot::try_from(v.timing)?,
            po_allocation_group: v.po_allocation_group,
            multi_po: v.multi_po,
            ssp_snapshot_ref: v.ssp_snapshot_ref,
            subscription_ref: v.subscription_ref,
            vc_estimate_ref: v.vc_estimate_ref,
            vc_method_ref: v.vc_method_ref,
            immaterial_one_shot_sku: v.immaterial_one_shot_sku,
        })
    }
}

/// Explicit durable mirror of [`domain::RefundWithCreditNoteIntent`].
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StoredRefundWithCreditNoteIntent {
    pub refund: StoredRefundIntent,
    pub credit_note: StoredCreditNoteIntent,
}
impl From<&domain::RefundWithCreditNoteIntent> for StoredRefundWithCreditNoteIntent {
    fn from(v: &domain::RefundWithCreditNoteIntent) -> Self {
        Self {
            refund: StoredRefundIntent::from(&v.refund),
            credit_note: StoredCreditNoteIntent::from(&v.credit_note),
        }
    }
}
impl TryFrom<StoredRefundWithCreditNoteIntent> for domain::RefundWithCreditNoteIntent {
    type Error = DomainError;
    fn try_from(v: StoredRefundWithCreditNoteIntent) -> Result<Self, Self::Error> {
        Ok(Self {
            refund: domain::RefundIntent::try_from(v.refund)?,
            credit_note: domain::CreditNoteIntent::try_from(v.credit_note)?,
        })
    }
}

/// Explicit durable mirror of [`domain::BackdatedInvoiceSnapshot`].
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StoredBackdatedInvoiceSnapshot {
    pub invoice_id: String,
    pub payer_tenant_id: Uuid,
    pub resource_tenant_id: Option<Uuid>,
    pub seller_tenant_id: Uuid,
    pub effective_at: NaiveDate,
    pub due_date: Option<NaiveDate>,
    pub period_id: String,
    pub items: Vec<StoredBackdatedInvoiceItem>,
    pub tax: Vec<StoredBackdatedTaxBreakdown>,
    pub posted_by_actor_id: Uuid,
    pub correlation_id: Uuid,
}
impl From<&domain::BackdatedInvoiceSnapshot> for StoredBackdatedInvoiceSnapshot {
    fn from(v: &domain::BackdatedInvoiceSnapshot) -> Self {
        Self {
            invoice_id: v.invoice_id.clone(),
            payer_tenant_id: v.payer_tenant_id,
            resource_tenant_id: v.resource_tenant_id,
            seller_tenant_id: v.seller_tenant_id,
            effective_at: v.effective_at,
            due_date: v.due_date,
            period_id: v.period_id.clone(),
            items: v
                .items
                .iter()
                .map(StoredBackdatedInvoiceItem::from)
                .collect(),
            tax: v
                .tax
                .iter()
                .map(StoredBackdatedTaxBreakdown::from)
                .collect(),
            posted_by_actor_id: v.posted_by_actor_id,
            correlation_id: v.correlation_id,
        }
    }
}
impl TryFrom<StoredBackdatedInvoiceSnapshot> for domain::BackdatedInvoiceSnapshot {
    type Error = DomainError;
    fn try_from(v: StoredBackdatedInvoiceSnapshot) -> Result<Self, Self::Error> {
        Ok(Self {
            invoice_id: v.invoice_id,
            payer_tenant_id: v.payer_tenant_id,
            resource_tenant_id: v.resource_tenant_id,
            seller_tenant_id: v.seller_tenant_id,
            effective_at: v.effective_at,
            due_date: v.due_date,
            period_id: v.period_id,
            items: v
                .items
                .into_iter()
                .map(domain::BackdatedInvoiceItem::try_from)
                .collect::<Result<Vec<_>, _>>()?,
            tax: v
                .tax
                .into_iter()
                .map(domain::BackdatedTaxBreakdown::try_from)
                .collect::<Result<Vec<_>, _>>()?,
            posted_by_actor_id: v.posted_by_actor_id,
            correlation_id: v.correlation_id,
        })
    }
}

/// Explicit durable mirror of [`domain::BackdatedInvoiceItem`].
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StoredBackdatedInvoiceItem {
    pub amount_ex_tax: StoredMoney,
    pub deferred: StoredMoney,
    pub recognition: Option<StoredDebitNoteRecognitionSnapshot>,
    pub revenue_stream: String,
    pub catalog_class: Option<String>,
    pub contract_class: Option<String>,
    pub gl_code: Option<String>,
    pub invoice_item_ref: Option<String>,
    pub sku_or_plan_ref: Option<String>,
    pub price_id: Option<String>,
    pub pricing_snapshot_ref: Option<String>,
}
impl From<&domain::BackdatedInvoiceItem> for StoredBackdatedInvoiceItem {
    fn from(v: &domain::BackdatedInvoiceItem) -> Self {
        Self {
            amount_ex_tax: StoredMoney::from(&v.amount_ex_tax),
            deferred: StoredMoney::from(&v.deferred),
            recognition: v
                .recognition
                .as_ref()
                .map(StoredDebitNoteRecognitionSnapshot::from),
            revenue_stream: v.revenue_stream.clone(),
            catalog_class: v.catalog_class.clone(),
            contract_class: v.contract_class.clone(),
            gl_code: v.gl_code.clone(),
            invoice_item_ref: v.invoice_item_ref.clone(),
            sku_or_plan_ref: v.sku_or_plan_ref.clone(),
            price_id: v.price_id.clone(),
            pricing_snapshot_ref: v.pricing_snapshot_ref.clone(),
        }
    }
}
impl TryFrom<StoredBackdatedInvoiceItem> for domain::BackdatedInvoiceItem {
    type Error = DomainError;
    fn try_from(v: StoredBackdatedInvoiceItem) -> Result<Self, Self::Error> {
        Ok(Self {
            amount_ex_tax: PostedMoney::try_from(v.amount_ex_tax).map_err(map_money_error)?,
            deferred: PostedMoney::try_from(v.deferred).map_err(map_money_error)?,
            recognition: v
                .recognition
                .map(domain::DebitNoteRecognitionSnapshot::try_from)
                .transpose()?,
            revenue_stream: v.revenue_stream,
            catalog_class: v.catalog_class,
            contract_class: v.contract_class,
            gl_code: v.gl_code,
            invoice_item_ref: v.invoice_item_ref,
            sku_or_plan_ref: v.sku_or_plan_ref,
            price_id: v.price_id,
            pricing_snapshot_ref: v.pricing_snapshot_ref,
        })
    }
}

/// Explicit durable mirror of [`domain::BackdatedTaxBreakdown`].
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StoredBackdatedTaxBreakdown {
    pub amount: StoredMoney,
    pub tax_jurisdiction: String,
    pub tax_filing_period: String,
    pub tax_rate_ref: Option<String>,
}
impl From<&domain::BackdatedTaxBreakdown> for StoredBackdatedTaxBreakdown {
    fn from(v: &domain::BackdatedTaxBreakdown) -> Self {
        Self {
            amount: StoredMoney::from(&v.amount),
            tax_jurisdiction: v.tax_jurisdiction.clone(),
            tax_filing_period: v.tax_filing_period.clone(),
            tax_rate_ref: v.tax_rate_ref.clone(),
        }
    }
}
impl TryFrom<StoredBackdatedTaxBreakdown> for domain::BackdatedTaxBreakdown {
    type Error = DomainError;
    fn try_from(v: StoredBackdatedTaxBreakdown) -> Result<Self, Self::Error> {
        Ok(Self {
            amount: PostedMoney::try_from(v.amount).map_err(map_money_error)?,
            tax_jurisdiction: v.tax_jurisdiction,
            tax_filing_period: v.tax_filing_period,
            tax_rate_ref: v.tax_rate_ref,
        })
    }
}

/// Explicit discriminated durable replay payload.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "SCREAMING_SNAKE_CASE")]
pub enum StoredApprovalIntent {
    Reverse(StoredReverseIntent),
    CreditGrant(StoredCreditGrantIntent),
    ChargebackLoss(StoredChargebackLossIntent),
    PayerClosure(StoredPayerClosureIntent),
    PeriodReopen(StoredPeriodReopenIntent),
    MaterialBackdating(StoredBackdatedPost),
    RecognitionScheduleChange(StoredRecognitionScheduleChangeIntent),
    Refund(StoredRefundIntent),
    ManualAdjustment(StoredManualAdjustmentIntent),
    CreditNote(StoredCreditNoteIntent),
    DebitNote(StoredDebitNoteIntent),
    RefundWithCreditNote(StoredRefundWithCreditNoteIntent),
}
impl From<&domain::ApprovalIntent> for StoredApprovalIntent {
    fn from(v: &domain::ApprovalIntent) -> Self {
        match v {
            domain::ApprovalIntent::Reverse(v) => Self::Reverse(StoredReverseIntent::from(v)),
            domain::ApprovalIntent::CreditGrant(v) => {
                Self::CreditGrant(StoredCreditGrantIntent::from(v))
            }
            domain::ApprovalIntent::ChargebackLoss(v) => {
                Self::ChargebackLoss(StoredChargebackLossIntent::from(v))
            }
            domain::ApprovalIntent::PayerClosure(v) => {
                Self::PayerClosure(StoredPayerClosureIntent::from(v))
            }
            domain::ApprovalIntent::PeriodReopen(v) => {
                Self::PeriodReopen(StoredPeriodReopenIntent::from(v))
            }
            domain::ApprovalIntent::MaterialBackdating(v) => {
                Self::MaterialBackdating(StoredBackdatedPost::from(v))
            }
            domain::ApprovalIntent::RecognitionScheduleChange(v) => {
                Self::RecognitionScheduleChange(StoredRecognitionScheduleChangeIntent::from(v))
            }
            domain::ApprovalIntent::Refund(v) => Self::Refund(StoredRefundIntent::from(v)),
            domain::ApprovalIntent::ManualAdjustment(v) => {
                Self::ManualAdjustment(StoredManualAdjustmentIntent::from(v))
            }
            domain::ApprovalIntent::CreditNote(v) => {
                Self::CreditNote(StoredCreditNoteIntent::from(v))
            }
            domain::ApprovalIntent::DebitNote(v) => Self::DebitNote(StoredDebitNoteIntent::from(v)),
            domain::ApprovalIntent::RefundWithCreditNote(v) => {
                Self::RefundWithCreditNote(StoredRefundWithCreditNoteIntent::from(v))
            }
        }
    }
}
impl TryFrom<StoredApprovalIntent> for domain::ApprovalIntent {
    type Error = DomainError;
    fn try_from(v: StoredApprovalIntent) -> Result<Self, Self::Error> {
        let intent = match v {
            StoredApprovalIntent::Reverse(v) => Self::Reverse(domain::ReverseIntent::try_from(v)?),
            StoredApprovalIntent::CreditGrant(v) => {
                Self::CreditGrant(domain::CreditGrantIntent::try_from(v)?)
            }
            StoredApprovalIntent::ChargebackLoss(v) => {
                Self::ChargebackLoss(domain::ChargebackLossIntent::try_from(v)?)
            }
            StoredApprovalIntent::PayerClosure(v) => {
                Self::PayerClosure(domain::PayerClosureIntent::try_from(v)?)
            }
            StoredApprovalIntent::PeriodReopen(v) => {
                Self::PeriodReopen(domain::PeriodReopenIntent::try_from(v)?)
            }
            StoredApprovalIntent::MaterialBackdating(v) => {
                Self::MaterialBackdating(domain::BackdatedPost::try_from(v)?)
            }
            StoredApprovalIntent::RecognitionScheduleChange(v) => Self::RecognitionScheduleChange(
                domain::RecognitionScheduleChangeIntent::try_from(v)?,
            ),
            StoredApprovalIntent::Refund(v) => Self::Refund(domain::RefundIntent::try_from(v)?),
            StoredApprovalIntent::ManualAdjustment(v) => {
                Self::ManualAdjustment(domain::ManualAdjustmentIntent::try_from(v)?)
            }
            StoredApprovalIntent::CreditNote(v) => {
                Self::CreditNote(domain::CreditNoteIntent::try_from(v)?)
            }
            StoredApprovalIntent::DebitNote(v) => {
                Self::DebitNote(domain::DebitNoteIntent::try_from(v)?)
            }
            StoredApprovalIntent::RefundWithCreditNote(v) => {
                Self::RefundWithCreditNote(domain::RefundWithCreditNoteIntent::try_from(v)?)
            }
        };
        intent.validate_money_metadata()?;
        Ok(intent)
    }
}

/// Explicit discriminated durable replay payload.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "post", rename_all = "SCREAMING_SNAKE_CASE")]
pub enum StoredBackdatedPost {
    Invoice(StoredBackdatedInvoiceSnapshot),
}
impl From<&domain::BackdatedPost> for StoredBackdatedPost {
    fn from(v: &domain::BackdatedPost) -> Self {
        match v {
            domain::BackdatedPost::Invoice(v) => {
                Self::Invoice(StoredBackdatedInvoiceSnapshot::from(v))
            }
        }
    }
}
impl TryFrom<StoredBackdatedPost> for domain::BackdatedPost {
    type Error = DomainError;
    fn try_from(v: StoredBackdatedPost) -> Result<Self, Self::Error> {
        Ok(match v {
            StoredBackdatedPost::Invoice(v) => {
                Self::Invoice(domain::BackdatedInvoiceSnapshot::try_from(v)?)
            }
        })
    }
}

/// Recognition timing discriminator, preserving optional period strings verbatim.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "timing", rename_all = "SCREAMING_SNAKE_CASE")]
pub enum StoredDebitNoteTimingSnapshot {
    PointInTime,
    StraightLine {
        periods: u32,
        first_period_id: Option<String>,
    },
}
impl From<&domain::DebitNoteTimingSnapshot> for StoredDebitNoteTimingSnapshot {
    fn from(v: &domain::DebitNoteTimingSnapshot) -> Self {
        match v {
            domain::DebitNoteTimingSnapshot::PointInTime => Self::PointInTime,
            domain::DebitNoteTimingSnapshot::StraightLine {
                periods,
                first_period_id,
            } => Self::StraightLine {
                periods: *periods,
                first_period_id: first_period_id.clone(),
            },
        }
    }
}
impl TryFrom<StoredDebitNoteTimingSnapshot> for domain::DebitNoteTimingSnapshot {
    type Error = DomainError;
    fn try_from(v: StoredDebitNoteTimingSnapshot) -> Result<Self, Self::Error> {
        Ok(match v {
            StoredDebitNoteTimingSnapshot::PointInTime => Self::PointInTime,
            StoredDebitNoteTimingSnapshot::StraightLine {
                periods,
                first_period_id,
            } => Self::StraightLine {
                periods,
                first_period_id,
            },
        })
    }
}

/// Encode only money canonically; Serde JSON provides unambiguous presence/length framing.
///
/// # Errors
/// [`DomainError::Internal`] when the intent cannot be serialized to JSON.
pub fn encode_intent(intent: &domain::ApprovalIntent) -> Result<serde_json::Value, DomainError> {
    serde_json::to_value(StoredApprovalIntent::from(intent))
        .map_err(|e| DomainError::Internal(format!("encode approval intent: {e}")))
}
/// Decode historical snapshots using their stored metadata, without registry refresh.
///
/// # Errors
/// [`DomainError::Internal`] when the stored JSON does not deserialize, or the decoded intent
/// fails the domain's money-metadata validation.
pub fn decode_intent(value: serde_json::Value) -> Result<domain::ApprovalIntent, DomainError> {
    let dto: StoredApprovalIntent = serde_json::from_value(value)
        .map_err(|e| DomainError::Internal(format!("decode approval intent: {e}")))?;
    domain::ApprovalIntent::try_from(dto)
        .map_err(|e| DomainError::Internal(format!("invalid stored approval intent: {e}")))
}
/// Decode an intent a client sent (resubmit). Unlike [`decode_intent`], which reads
/// a stored row where any failure is corruption, a malformed body is the client's
/// `InvalidRequest` and a money violation keeps its own client category.
///
/// # Errors
/// [`DomainError::InvalidRequest`] for a body that is not an intent; the intent's
/// own validation error (for example [`DomainError::InvalidPostingIncrement`]).
pub fn decode_client_intent(
    value: serde_json::Value,
) -> Result<domain::ApprovalIntent, DomainError> {
    let dto: StoredApprovalIntent = serde_json::from_value(value)
        .map_err(|e| DomainError::InvalidRequest(format!("approval intent: {e}")))?;
    domain::ApprovalIntent::try_from(dto)
}
/// Stable bytes binding all nested fields, including discriminators and optional presence.
///
/// # Errors
/// [`DomainError::Internal`] when the intent cannot be encoded or serialized.
pub fn canonical_identity(intent: &domain::ApprovalIntent) -> Result<Vec<u8>, DomainError> {
    serde_json::to_vec(&encode_intent(intent)?)
        .map_err(|e| DomainError::Internal(format!("approval identity: {e}")))
}

/// Policy provenance captured at gate or resubmission, using explicit money DTOs.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredThresholdSnapshot {
    pub d2_default: String,
    pub d2_thresholds: Vec<StoredMoney>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub d2_threshold: Option<StoredMoney>,
    pub policy_version: Option<i64>,
    pub policy_effective_from: Option<String>,
    pub basis: SnapshotBasis,
    pub a6_backdating_biz_days: i32,
    pub pending_ttl_seconds: i64,
    pub resolved_at: String,
}
/// A resubmission records transaction policy provenance without running a gate.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SnapshotBasis {
    TransactionGate,
    FunctionalGate,
    ResubmissionTransactionSnapshot,
}

/// Decode and validate a stored threshold snapshot with its captured specs, never
/// the live registry.
///
/// # Errors
/// [`DomainError::Internal`] when the stored snapshot does not deserialize, or when
/// [`check_threshold_snapshot`] rejects it.
pub fn validate_threshold_snapshot(
    value: serde_json::Value,
) -> Result<StoredThresholdSnapshot, DomainError> {
    let snapshot: StoredThresholdSnapshot = serde_json::from_value(value)
        .map_err(|e| DomainError::Internal(format!("decode approval policy snapshot: {e}")))?;
    check_threshold_snapshot(snapshot)
}

/// Validate a typed threshold snapshot and return it with canonical money text.
///
/// # Errors
/// [`DomainError::Internal`] when a captured threshold violates its own currency spec, the
/// captured policy fails `DualControlPolicy::try_new`, the D2 default rule is unknown, or the captured
/// resolved threshold differs from the captured policy.
pub fn check_threshold_snapshot(
    mut snapshot: StoredThresholdSnapshot,
) -> Result<StoredThresholdSnapshot, DomainError> {
    use crate::domain::approval::policy::{D2_DEFAULT_RULE, D2Thresholds, DualControlPolicy};
    let invalid = |e: DomainError| {
        DomainError::Internal(format!("invalid stored approval policy snapshot: {e}"))
    };
    // Both money fields are re-encoded below, so they are moved out, not cloned.
    let thresholds = std::mem::take(&mut snapshot.d2_thresholds)
        .into_iter()
        .map(|v| {
            PostedMoney::try_from(v)
                .map_err(map_money_error)
                .map_err(&invalid)
        })
        .collect::<Result<Vec<_>, _>>()?;
    // The captured policy is validated once, by its type.
    let policy = D2Thresholds::try_new(thresholds)
        .and_then(|d2| {
            DualControlPolicy::try_new(
                d2,
                snapshot.a6_backdating_biz_days,
                snapshot.pending_ttl_seconds,
            )
        })
        .map_err(|e| DomainError::Internal(format!("invalid stored approval policy: {e:?}")))?;
    if snapshot.d2_default != D2_DEFAULT_RULE {
        return Err(DomainError::Internal(
            "unknown stored D2 default rule".into(),
        ));
    }
    if let Some(value) = snapshot.d2_threshold.take() {
        let money = PostedMoney::try_from(value)
            .map_err(map_money_error)
            .map_err(&invalid)?;
        if policy
            .d2_threshold(money.currency())
            .map_err(|e| DomainError::Internal(format!("stored threshold metadata: {e:?}")))?
            != money
        {
            return Err(DomainError::Internal(
                "stored resolved threshold differs from captured policy".into(),
            ));
        }
        snapshot.d2_threshold = Some(StoredMoney::from(&money));
    }
    snapshot.d2_thresholds = policy
        .d2_thresholds()
        .iter()
        .map(StoredMoney::from)
        .collect();
    Ok(snapshot)
}

#[cfg(test)]
#[path = "stored_intent_snapshot_tests.rs"]
mod snapshot_tests;

#[cfg(test)]
#[path = "stored_intent_sweep_tests.rs"]
mod sweep_tests;
