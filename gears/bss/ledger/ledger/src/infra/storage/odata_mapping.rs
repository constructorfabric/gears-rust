//! `OData` field → `SeaORM` column mappers for the ledger's list endpoints.
//!
//! `paginate_odata::<F, M, _, _, _, _>(...)` is generic over a filter-field
//! type `F` and a mapper `M: ODataFieldMapping<F>`; this module supplies the
//! mappers `M` for the three row collections (`tenant_account`, `journal_line`,
//! `account_balance`). Each maps every [`crate::odata`] filter field to its
//! backing column and supplies the seekset cursor-value extractor.

use toolkit_db::odata::sea_orm_filter::{FieldToColumn, ODataFieldMapping};

use crate::infra::storage::entity::account_balance::{
    Column as BalanceColumn, Entity as BalanceEntity, Model as BalanceModel,
};
use crate::infra::storage::entity::credit_note::{
    Column as CreditNoteColumn, Entity as CreditNoteEntity, Model as CreditNoteModel,
};
use crate::infra::storage::entity::debit_note::{
    Column as DebitNoteColumn, Entity as DebitNoteEntity, Model as DebitNoteModel,
};
use crate::infra::storage::entity::dispute::{
    Column as DisputeColumn, Entity as DisputeEntity, Model as DisputeModel,
};
use crate::infra::storage::entity::exception_queue::{
    Column as ExceptionColumn, Entity as ExceptionEntity, Model as ExceptionModel,
};
use crate::infra::storage::entity::journal_entry::{
    Column as JournalEntryColumn, Entity as JournalEntryEntity, Model as JournalEntryModel,
};
use crate::infra::storage::entity::journal_line::{
    Column as JournalLineColumn, Entity as JournalLineEntity, Model as JournalLineModel,
};
use crate::infra::storage::entity::recognition_run::{
    Column as RecognitionRunColumn, Entity as RecognitionRunEntity, Model as RecognitionRunModel,
};
use crate::infra::storage::entity::refund::{
    Column as RefundColumn, Entity as RefundEntity, Model as RefundModel,
};
use crate::infra::storage::entity::tenant_account::{
    Column as TenantAccountColumn, Entity as TenantAccountEntity, Model as TenantAccountModel,
};
use crate::odata::{
    AccountInfoFilterField, BalanceFilterField, CreditNoteFilterField, DebitNoteFilterField,
    DisputeFilterField, ExceptionFilterField, JournalEntryFilterField, JournalLineFilterField,
    RecognitionRunFilterField, RefundFilterField,
};

/// Maps [`AccountInfoFilterField`] variants to their backing
/// [`tenant_account`](crate::infra::storage::entity::tenant_account) columns and
/// supplies the seekset cursor-value extractor.
pub struct AccountInfoODataMapper;

impl FieldToColumn<AccountInfoFilterField> for AccountInfoODataMapper {
    type Column = TenantAccountColumn;

    fn map_field(field: AccountInfoFilterField) -> TenantAccountColumn {
        match field {
            AccountInfoFilterField::AccountId => TenantAccountColumn::AccountId,
            AccountInfoFilterField::AccountClass => TenantAccountColumn::AccountClass,
            AccountInfoFilterField::Currency => TenantAccountColumn::Currency,
            AccountInfoFilterField::RevenueStream => TenantAccountColumn::RevenueStream,
            AccountInfoFilterField::LifecycleState => TenantAccountColumn::LifecycleState,
        }
    }
}

impl ODataFieldMapping<AccountInfoFilterField> for AccountInfoODataMapper {
    type Entity = TenantAccountEntity;

    fn extract_cursor_value(
        model: &TenantAccountModel,
        field: AccountInfoFilterField,
    ) -> sea_orm::Value {
        match field {
            AccountInfoFilterField::AccountId => {
                sea_orm::Value::Uuid(Some(Box::new(model.account_id)))
            }
            AccountInfoFilterField::AccountClass => {
                sea_orm::Value::String(Some(Box::new(model.account_class.clone())))
            }
            AccountInfoFilterField::Currency => {
                sea_orm::Value::String(Some(Box::new(model.currency.clone())))
            }
            // `revenue_stream` is nullable; a `None` cursor value round-trips as
            // a typed NULL (the keyset never seeks past a NULL on the default
            // `account_id` order, so this is belt-and-braces).
            AccountInfoFilterField::RevenueStream => match &model.revenue_stream {
                Some(s) => sea_orm::Value::String(Some(Box::new(s.clone()))),
                None => sea_orm::Value::String(None),
            },
            AccountInfoFilterField::LifecycleState => {
                sea_orm::Value::String(Some(Box::new(model.lifecycle_state.clone())))
            }
        }
    }
}

/// Maps [`JournalLineFilterField`] variants to their backing
/// [`journal_line`](crate::infra::storage::entity::journal_line) columns and
/// supplies the seekset cursor-value extractor.
pub struct JournalLineODataMapper;

impl FieldToColumn<JournalLineFilterField> for JournalLineODataMapper {
    type Column = JournalLineColumn;

    fn map_field(field: JournalLineFilterField) -> JournalLineColumn {
        match field {
            JournalLineFilterField::LineId => JournalLineColumn::LineId,
            JournalLineFilterField::PayerTenantId => JournalLineColumn::PayerTenantId,
            JournalLineFilterField::AccountClass => JournalLineColumn::AccountClass,
            JournalLineFilterField::PeriodId => JournalLineColumn::PeriodId,
            JournalLineFilterField::InvoiceId => JournalLineColumn::InvoiceId,
        }
    }
}

impl ODataFieldMapping<JournalLineFilterField> for JournalLineODataMapper {
    type Entity = JournalLineEntity;

    fn extract_cursor_value(
        model: &JournalLineModel,
        field: JournalLineFilterField,
    ) -> sea_orm::Value {
        match field {
            JournalLineFilterField::LineId => sea_orm::Value::Uuid(Some(Box::new(model.line_id))),
            JournalLineFilterField::PayerTenantId => {
                sea_orm::Value::Uuid(Some(Box::new(model.payer_tenant_id)))
            }
            JournalLineFilterField::AccountClass => {
                sea_orm::Value::String(Some(Box::new(model.account_class.clone())))
            }
            JournalLineFilterField::PeriodId => {
                sea_orm::Value::String(Some(Box::new(model.period_id.clone())))
            }
            JournalLineFilterField::InvoiceId => match &model.invoice_id {
                Some(s) => sea_orm::Value::String(Some(Box::new(s.clone()))),
                None => sea_orm::Value::String(None),
            },
        }
    }
}

/// Maps [`JournalEntryFilterField`] variants to their backing
/// [`journal_entry`](crate::infra::storage::entity::journal_entry) HEADER columns
/// and supplies the seekset cursor-value extractor. `entry_id` is the surrogate
/// `Uuid` keyset column (mapped to `Value::Uuid`); `source_doc_type` /
/// `source_business_id` / `period_id` are non-nullable `varchar` dims (mapped to
/// `Value::String`), so no `None`-cursor arm is needed (unlike `RefundODataMapper`'s
/// `invoice_id`).
pub struct JournalEntryODataMapper;

impl FieldToColumn<JournalEntryFilterField> for JournalEntryODataMapper {
    type Column = JournalEntryColumn;

    fn map_field(field: JournalEntryFilterField) -> JournalEntryColumn {
        match field {
            JournalEntryFilterField::EntryId => JournalEntryColumn::EntryId,
            JournalEntryFilterField::SourceDocType => JournalEntryColumn::SourceDocType,
            JournalEntryFilterField::SourceBusinessId => JournalEntryColumn::SourceBusinessId,
            JournalEntryFilterField::PeriodId => JournalEntryColumn::PeriodId,
        }
    }
}

impl ODataFieldMapping<JournalEntryFilterField> for JournalEntryODataMapper {
    type Entity = JournalEntryEntity;

    fn extract_cursor_value(
        model: &JournalEntryModel,
        field: JournalEntryFilterField,
    ) -> sea_orm::Value {
        match field {
            JournalEntryFilterField::EntryId => {
                sea_orm::Value::Uuid(Some(Box::new(model.entry_id)))
            }
            JournalEntryFilterField::SourceDocType => {
                sea_orm::Value::String(Some(Box::new(model.source_doc_type.clone())))
            }
            JournalEntryFilterField::SourceBusinessId => {
                sea_orm::Value::String(Some(Box::new(model.source_business_id.clone())))
            }
            JournalEntryFilterField::PeriodId => {
                sea_orm::Value::String(Some(Box::new(model.period_id.clone())))
            }
        }
    }
}

/// Maps [`BalanceFilterField`] variants to their backing
/// [`account_balance`](crate::infra::storage::entity::account_balance) columns
/// and supplies the seekset cursor-value extractor.
pub struct BalanceODataMapper;

impl FieldToColumn<BalanceFilterField> for BalanceODataMapper {
    type Column = BalanceColumn;

    fn map_field(field: BalanceFilterField) -> BalanceColumn {
        match field {
            BalanceFilterField::AccountId => BalanceColumn::AccountId,
            BalanceFilterField::AccountClass => BalanceColumn::AccountClass,
            BalanceFilterField::Currency => BalanceColumn::Currency,
        }
    }
}

impl ODataFieldMapping<BalanceFilterField> for BalanceODataMapper {
    type Entity = BalanceEntity;

    fn extract_cursor_value(model: &BalanceModel, field: BalanceFilterField) -> sea_orm::Value {
        match field {
            BalanceFilterField::AccountId => sea_orm::Value::Uuid(Some(Box::new(model.account_id))),
            BalanceFilterField::AccountClass => {
                sea_orm::Value::String(Some(Box::new(model.account_class.clone())))
            }
            BalanceFilterField::Currency => {
                sea_orm::Value::String(Some(Box::new(model.currency.clone())))
            }
        }
    }
}

/// Maps [`RefundFilterField`] variants to their backing
/// [`refund`](crate::infra::storage::entity::refund) columns and supplies the
/// seekset cursor-value extractor. Every dim is a `varchar` (`refund_id` is the
/// default keyset column); `invoice_id` is nullable (Pattern A has none).
pub struct RefundODataMapper;

impl FieldToColumn<RefundFilterField> for RefundODataMapper {
    type Column = RefundColumn;

    fn map_field(field: RefundFilterField) -> RefundColumn {
        match field {
            RefundFilterField::RefundId => RefundColumn::RefundId,
            RefundFilterField::PaymentId => RefundColumn::PaymentId,
            RefundFilterField::PspRefundId => RefundColumn::PspRefundId,
            RefundFilterField::Phase => RefundColumn::Phase,
            RefundFilterField::Pattern => RefundColumn::Pattern,
            RefundFilterField::ClearingState => RefundColumn::ClearingState,
            RefundFilterField::InvoiceId => RefundColumn::InvoiceId,
        }
    }
}

impl ODataFieldMapping<RefundFilterField> for RefundODataMapper {
    type Entity = RefundEntity;

    fn extract_cursor_value(model: &RefundModel, field: RefundFilterField) -> sea_orm::Value {
        match field {
            RefundFilterField::RefundId => {
                sea_orm::Value::String(Some(Box::new(model.refund_id.clone())))
            }
            RefundFilterField::PaymentId => {
                sea_orm::Value::String(Some(Box::new(model.payment_id.clone())))
            }
            RefundFilterField::PspRefundId => {
                sea_orm::Value::String(Some(Box::new(model.psp_refund_id.clone())))
            }
            RefundFilterField::Phase => sea_orm::Value::String(Some(Box::new(model.phase.clone()))),
            RefundFilterField::Pattern => {
                sea_orm::Value::String(Some(Box::new(model.pattern.clone())))
            }
            RefundFilterField::ClearingState => {
                sea_orm::Value::String(Some(Box::new(model.clearing_state.clone())))
            }
            // `invoice_id` is nullable (Pattern A has none); a `None` cursor value
            // round-trips as a typed NULL (the keyset never seeks past a NULL on
            // the default `refund_id` order, so this is belt-and-braces).
            RefundFilterField::InvoiceId => match &model.invoice_id {
                Some(s) => sea_orm::Value::String(Some(Box::new(s.clone()))),
                None => sea_orm::Value::String(None),
            },
        }
    }
}

/// Maps [`CreditNoteFilterField`] variants to their backing
/// [`credit_note`](crate::infra::storage::entity::credit_note) columns and
/// supplies the seekset cursor-value extractor. Every dim is a `varchar`
/// (`credit_note_id` is the default keyset column); all four filter columns are
/// non-nullable, so no `None`-cursor arm is needed (unlike `RefundODataMapper`'s
/// `invoice_id`).
pub struct CreditNoteODataMapper;

impl FieldToColumn<CreditNoteFilterField> for CreditNoteODataMapper {
    type Column = CreditNoteColumn;

    fn map_field(field: CreditNoteFilterField) -> CreditNoteColumn {
        match field {
            CreditNoteFilterField::CreditNoteId => CreditNoteColumn::CreditNoteId,
            CreditNoteFilterField::OriginInvoiceId => CreditNoteColumn::OriginInvoiceId,
            CreditNoteFilterField::RevenueStream => CreditNoteColumn::RevenueStream,
            CreditNoteFilterField::ReasonCode => CreditNoteColumn::ReasonCode,
        }
    }
}

impl ODataFieldMapping<CreditNoteFilterField> for CreditNoteODataMapper {
    type Entity = CreditNoteEntity;

    fn extract_cursor_value(
        model: &CreditNoteModel,
        field: CreditNoteFilterField,
    ) -> sea_orm::Value {
        match field {
            CreditNoteFilterField::CreditNoteId => {
                sea_orm::Value::String(Some(Box::new(model.credit_note_id.clone())))
            }
            CreditNoteFilterField::OriginInvoiceId => {
                sea_orm::Value::String(Some(Box::new(model.origin_invoice_id.clone())))
            }
            CreditNoteFilterField::RevenueStream => {
                sea_orm::Value::String(Some(Box::new(model.revenue_stream.clone())))
            }
            CreditNoteFilterField::ReasonCode => {
                sea_orm::Value::String(Some(Box::new(model.reason_code.clone())))
            }
        }
    }
}

/// Maps [`DebitNoteFilterField`] variants to their backing
/// [`debit_note`](crate::infra::storage::entity::debit_note) columns and supplies
/// the seekset cursor-value extractor. Both dims are non-nullable `varchar`
/// (`debit_note_id` is the default keyset column); the `debit_note` table is
/// leaner than `credit_note` (no `revenue_stream` / `reason_code`).
pub struct DebitNoteODataMapper;

impl FieldToColumn<DebitNoteFilterField> for DebitNoteODataMapper {
    type Column = DebitNoteColumn;

    fn map_field(field: DebitNoteFilterField) -> DebitNoteColumn {
        match field {
            DebitNoteFilterField::DebitNoteId => DebitNoteColumn::DebitNoteId,
            DebitNoteFilterField::OriginInvoiceId => DebitNoteColumn::OriginInvoiceId,
        }
    }
}

impl ODataFieldMapping<DebitNoteFilterField> for DebitNoteODataMapper {
    type Entity = DebitNoteEntity;

    fn extract_cursor_value(model: &DebitNoteModel, field: DebitNoteFilterField) -> sea_orm::Value {
        match field {
            DebitNoteFilterField::DebitNoteId => {
                sea_orm::Value::String(Some(Box::new(model.debit_note_id.clone())))
            }
            DebitNoteFilterField::OriginInvoiceId => {
                sea_orm::Value::String(Some(Box::new(model.origin_invoice_id.clone())))
            }
        }
    }
}

/// Maps [`DisputeFilterField`] variants to their backing
/// [`dispute`](crate::infra::storage::entity::dispute) columns and supplies the
/// seekset cursor-value extractor. Every dim is a non-nullable `varchar`
/// (`dispute_id` is the default keyset column), so no `None`-cursor arm is needed
/// (unlike `RefundODataMapper`'s `invoice_id`).
pub struct DisputeODataMapper;

impl FieldToColumn<DisputeFilterField> for DisputeODataMapper {
    type Column = DisputeColumn;

    fn map_field(field: DisputeFilterField) -> DisputeColumn {
        match field {
            DisputeFilterField::DisputeId => DisputeColumn::DisputeId,
            DisputeFilterField::PaymentId => DisputeColumn::PaymentId,
            DisputeFilterField::LastPhase => DisputeColumn::LastPhase,
            DisputeFilterField::Variant => DisputeColumn::Variant,
        }
    }
}

impl ODataFieldMapping<DisputeFilterField> for DisputeODataMapper {
    type Entity = DisputeEntity;

    fn extract_cursor_value(model: &DisputeModel, field: DisputeFilterField) -> sea_orm::Value {
        match field {
            DisputeFilterField::DisputeId => {
                sea_orm::Value::String(Some(Box::new(model.dispute_id.clone())))
            }
            DisputeFilterField::PaymentId => {
                sea_orm::Value::String(Some(Box::new(model.payment_id.clone())))
            }
            DisputeFilterField::LastPhase => {
                sea_orm::Value::String(Some(Box::new(model.last_phase.clone())))
            }
            DisputeFilterField::Variant => {
                sea_orm::Value::String(Some(Box::new(model.variant.clone())))
            }
        }
    }
}

/// Maps [`RecognitionRunFilterField`] variants to their backing
/// [`recognition_run`](crate::infra::storage::entity::recognition_run) columns and
/// supplies the seekset cursor-value extractor. `run_id` is the surrogate `Uuid`
/// keyset column (mapped to `Value::Uuid`); `period_id` / `status` are
/// non-nullable `varchar` dims (mapped to `Value::String`), so no `None`-cursor
/// arm is needed (unlike `RefundODataMapper`'s `invoice_id`).
pub struct RecognitionRunODataMapper;

impl FieldToColumn<RecognitionRunFilterField> for RecognitionRunODataMapper {
    type Column = RecognitionRunColumn;

    fn map_field(field: RecognitionRunFilterField) -> RecognitionRunColumn {
        match field {
            RecognitionRunFilterField::RunId => RecognitionRunColumn::RunId,
            RecognitionRunFilterField::PeriodId => RecognitionRunColumn::PeriodId,
            RecognitionRunFilterField::Status => RecognitionRunColumn::Status,
        }
    }
}

impl ODataFieldMapping<RecognitionRunFilterField> for RecognitionRunODataMapper {
    type Entity = RecognitionRunEntity;

    fn extract_cursor_value(
        model: &RecognitionRunModel,
        field: RecognitionRunFilterField,
    ) -> sea_orm::Value {
        match field {
            RecognitionRunFilterField::RunId => sea_orm::Value::Uuid(Some(Box::new(model.run_id))),
            RecognitionRunFilterField::PeriodId => {
                sea_orm::Value::String(Some(Box::new(model.period_id.clone())))
            }
            RecognitionRunFilterField::Status => {
                sea_orm::Value::String(Some(Box::new(model.status.clone())))
            }
        }
    }
}

/// Maps [`ExceptionFilterField`] variants to their backing
/// [`exception_queue`](crate::infra::storage::entity::exception_queue) columns and
/// supplies the seekset cursor-value extractor. `exception_id` is the surrogate
/// `Uuid` keyset column (mapped to `Value::Uuid`); the wire `type` field maps to
/// the `exception_type` column. `period_id` is nullable (a non-period exception
/// has none), so it carries a `None`-cursor arm like `RefundODataMapper`'s
/// `invoice_id`.
pub struct ExceptionODataMapper;

impl FieldToColumn<ExceptionFilterField> for ExceptionODataMapper {
    type Column = ExceptionColumn;

    fn map_field(field: ExceptionFilterField) -> ExceptionColumn {
        match field {
            ExceptionFilterField::ExceptionId => ExceptionColumn::ExceptionId,
            ExceptionFilterField::ExceptionType => ExceptionColumn::ExceptionType,
            ExceptionFilterField::Status => ExceptionColumn::Status,
            ExceptionFilterField::BusinessRef => ExceptionColumn::BusinessRef,
            ExceptionFilterField::PeriodId => ExceptionColumn::PeriodId,
        }
    }
}

impl ODataFieldMapping<ExceptionFilterField> for ExceptionODataMapper {
    type Entity = ExceptionEntity;

    fn extract_cursor_value(model: &ExceptionModel, field: ExceptionFilterField) -> sea_orm::Value {
        match field {
            ExceptionFilterField::ExceptionId => {
                sea_orm::Value::Uuid(Some(Box::new(model.exception_id)))
            }
            ExceptionFilterField::ExceptionType => {
                sea_orm::Value::String(Some(Box::new(model.exception_type.clone())))
            }
            ExceptionFilterField::Status => {
                sea_orm::Value::String(Some(Box::new(model.status.clone())))
            }
            ExceptionFilterField::BusinessRef => {
                sea_orm::Value::String(Some(Box::new(model.business_ref.clone())))
            }
            // `period_id` is nullable (a non-period exception has none); a `None`
            // cursor value round-trips as a typed NULL (the keyset never seeks past
            // a NULL on the default `exception_id` order, so this is
            // belt-and-braces).
            ExceptionFilterField::PeriodId => match &model.period_id {
                Some(s) => sea_orm::Value::String(Some(Box::new(s.clone()))),
                None => sea_orm::Value::String(None),
            },
        }
    }
}

#[cfg(test)]
#[path = "odata_mapping_tests.rs"]
mod tests;
