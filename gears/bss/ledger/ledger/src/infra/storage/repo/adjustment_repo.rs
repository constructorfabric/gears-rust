//! Canonical adjustment records and exact exposure counters on scoped caller transactions.
//! Errors must escape the caller transaction; repositories never retry.

use bss_ledger_sdk::{AccountClass, CurrencySpec, PostedMoney, Side, SourceDocType};

use crate::domain::exact_money::{ExactAmount, ExactError};
use crate::infra::posting::retry::{db_to_repo, scope_to_repo};
use crate::infra::storage::money_text::{decode_money, decode_optional_money, encode_amount};
use rust_decimal::Decimal;
use sea_orm::sea_query::Expr;
use sea_orm::{ActiveValue::Set, ColumnTrait, Condition, EntityTrait};
use toolkit_db::secure::{
    AccessScope, DBRunner, DbTx, SecureEntityExt, SecureInsertExt, SecureOnConflict,
    SecureUpdateExt,
};
use toolkit_db::{DBProvider, DbError};
use uuid::Uuid;

use toolkit_db::odata::sea_orm_filter::{LimitCfg, paginate_odata};
use toolkit_odata::{ODataQuery, Page, SortDir};

use crate::domain::model::RepoError;
use crate::infra::storage::entity::{
    ar_invoice_balance, credit_note, debit_note, invoice_exposure, journal_entry, journal_line,
    refund,
};
use crate::infra::storage::odata_mapping::{
    CreditNoteODataMapper, DebitNoteODataMapper, RefundODataMapper,
};
use crate::infra::storage::repo::journal_repo::{
    OdataPageError, map_odata_err, query_with_default_order,
};
use crate::odata::{CreditNoteFilterField, DebitNoteFilterField, RefundFilterField};
use time::OffsetDateTime;

/// Immutable credit note: inclusive-tax amount and ex-tax split parts share one spec.
pub struct NewCreditNote {
    pub tenant_id: Uuid,
    pub credit_note_id: String,
    pub origin_invoice_id: String,
    pub origin_invoice_item_ref: Option<String>,
    pub revenue_stream: String,
    pub amount: PostedMoney,
    pub recognized_part: PostedMoney,
    pub deferred_part: PostedMoney,
    pub split_basis_ref: Option<String>,
    pub reason_code: String,
    pub created_at_utc: OffsetDateTime,
}

/// Immutable debit note: inclusive-tax amount and ex-tax split parts share one spec.
pub struct NewDebitNote {
    pub tenant_id: Uuid,
    pub debit_note_id: String,
    pub origin_invoice_id: String,
    pub amount: PostedMoney,
    pub recognized_part: PostedMoney,
    pub deferred_part: PostedMoney,
    pub created_at_utc: OffsetDateTime,
}

/// Immutable cash refund amount with the existing phase and clearing lifecycle.
/// Natural identity is (tenant, psp_refund_id, phase); invoice_id follows its pattern.
pub struct NewRefund {
    pub tenant_id: Uuid,
    pub refund_id: String,
    pub psp_refund_id: String,
    pub phase: String,
    pub pattern: String,
    pub payment_id: String,
    pub invoice_id: Option<String>,
    pub amount: PostedMoney,
    pub clearing_state: String,
    pub relates_to_refund_id: Option<String>,
    pub reverses_entry_id: Option<Uuid>,
    pub created_at_utc: OffsetDateTime,
}

/// Validated credit_note record, retaining historical currency metadata.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CreditNoteView {
    pub tenant_id: Uuid,
    pub credit_note_id: String,
    pub origin_invoice_id: String,
    pub origin_invoice_item_ref: Option<String>,
    pub revenue_stream: String,
    pub amount: PostedMoney,
    pub recognized_part: PostedMoney,
    pub deferred_part: PostedMoney,
    pub split_basis_ref: Option<String>,
    pub reason_code: String,
    pub created_at_utc: OffsetDateTime,
}
/// Decode canonical stored money before exposing this record to callers.
fn decode_credit_note(m: credit_note::Model) -> Result<CreditNoteView, RepoError> {
    let amount = decode_money(&m.amount, &m.currency, m.currency_scale)?;
    let recognized_part = decode_money(&m.recognized_part, &m.currency, m.currency_scale)?;
    let deferred_part = decode_money(&m.deferred_part, &m.currency, m.currency_scale)?;
    stored_nonnegative(&amount)?;
    stored_nonnegative(&recognized_part)?;
    stored_nonnegative(&deferred_part)?;
    let value = CreditNoteView {
        tenant_id: m.tenant_id,
        credit_note_id: m.credit_note_id,
        origin_invoice_id: m.origin_invoice_id,
        origin_invoice_item_ref: m.origin_invoice_item_ref,
        revenue_stream: m.revenue_stream,
        amount,
        recognized_part,
        deferred_part,
        split_basis_ref: m.split_basis_ref,
        reason_code: m.reason_code,
        created_at_utc: m.created_at_utc,
    };
    Ok(value)
}
/// Validated debit_note record, retaining historical currency metadata.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DebitNoteView {
    pub tenant_id: Uuid,
    pub debit_note_id: String,
    pub origin_invoice_id: String,
    pub amount: PostedMoney,
    pub recognized_part: PostedMoney,
    pub deferred_part: PostedMoney,
    pub created_at_utc: OffsetDateTime,
}
/// Decode canonical stored money before exposing this record to callers.
fn decode_debit_note(m: debit_note::Model) -> Result<DebitNoteView, RepoError> {
    let amount = decode_money(&m.amount, &m.currency, m.currency_scale)?;
    let recognized_part = decode_money(&m.recognized_part, &m.currency, m.currency_scale)?;
    let deferred_part = decode_money(&m.deferred_part, &m.currency, m.currency_scale)?;
    stored_nonnegative(&amount)?;
    stored_nonnegative(&recognized_part)?;
    stored_nonnegative(&deferred_part)?;
    let value = DebitNoteView {
        tenant_id: m.tenant_id,
        debit_note_id: m.debit_note_id,
        origin_invoice_id: m.origin_invoice_id,
        amount,
        recognized_part,
        deferred_part,
        created_at_utc: m.created_at_utc,
    };
    Ok(value)
}
/// Validated refund record, retaining historical currency metadata.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RefundView {
    pub tenant_id: Uuid,
    pub refund_id: String,
    pub psp_refund_id: String,
    pub phase: String,
    pub pattern: String,
    pub payment_id: String,
    pub invoice_id: Option<String>,
    pub amount: PostedMoney,
    pub clearing_state: String,
    pub relates_to_refund_id: Option<String>,
    pub reverses_entry_id: Option<Uuid>,
    pub created_at_utc: OffsetDateTime,
    pub version: i64,
}
/// Decode canonical stored money before exposing this record to callers.
fn decode_refund(m: refund::Model) -> Result<RefundView, RepoError> {
    let amount = decode_money(&m.amount, &m.currency, m.currency_scale)?;
    stored_nonnegative(&amount)?;
    checked_version(m.version)?;
    let value = RefundView {
        tenant_id: m.tenant_id,
        refund_id: m.refund_id,
        psp_refund_id: m.psp_refund_id,
        phase: m.phase,
        pattern: m.pattern,
        payment_id: m.payment_id,
        invoice_id: m.invoice_id,
        amount,
        clearing_state: m.clearing_state,
        relates_to_refund_id: m.relates_to_refund_id,
        reverses_entry_id: m.reverses_entry_id,
        created_at_utc: m.created_at_utc,
        version: m.version,
    };
    Ok(value)
}
/// Validated invoice_exposure record, retaining historical currency metadata.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExposureView {
    pub tenant_id: Uuid,
    pub invoice_id: String,
    pub original_total: PostedMoney,
    pub debit_note_total: PostedMoney,
    pub credit_note_total: PostedMoney,
    pub version: i64,
}
/// Decode canonical stored money before exposing this record to callers.
fn decode_invoice_exposure(m: invoice_exposure::Model) -> Result<ExposureView, RepoError> {
    let original_total = decode_money(&m.original_total, &m.currency, m.currency_scale)?;
    let debit_note_total = decode_money(&m.debit_note_total, &m.currency, m.currency_scale)?;
    let credit_note_total = decode_money(&m.credit_note_total, &m.currency, m.currency_scale)?;
    checked_version(m.version)?;
    let value = ExposureView {
        tenant_id: m.tenant_id,
        invoice_id: m.invoice_id,
        original_total,
        debit_note_total,
        credit_note_total,
        version: m.version,
    };
    validate_exposure(&value)?;
    Ok(value)
}

/// Scoped adjustment storage; mutations use one caller-owned transaction attempt.
#[derive(Clone)]
pub struct AdjustmentRepo {
    db: DBProvider<DbError>,
}

impl AdjustmentRepo {
    /// Construct the repository with the configured backend for typed conflict classification.
    #[must_use]
    pub fn new(db: DBProvider<DbError>) -> Self {
        Self { db }
    }

    /// Fold original INVOICE_POST AR exactly; floor the net at zero in the explicit currency.
    ///
    /// # Errors
    /// [`RepoError::Db`] when no connection can be acquired or on a scope / storage failure;
    /// [`RepoError::Conflict`] on classified database contention.
    /// [`RepoError::InvalidStoredMoney`] when a stored amount is malformed or off its own
    /// stored currency contract. [`RepoError::Money`] when a stored AR line's currency or
    /// scale differs from `currency` (the caller's mismatch against the invoice, not
    /// corruption), or when the exact sum leaves the money contract.
    pub async fn read_posted_ar_incl_tax_out_of_txn(
        &self,
        scope: &AccessScope,
        tenant: Uuid,
        origin_invoice_id: &str,
        currency: &CurrencySpec,
    ) -> Result<PostedMoney, RepoError> {
        let conn = self
            .db
            .conn()
            .map_err(|e| db_to_repo(e, self.db.db().backend()))?;
        self.read_posted_ar_incl_tax_in(&conn, scope, tenant, origin_invoice_id, currency)
            .await
    }

    /// Fold original INVOICE_POST AR exactly; floor the net at zero in the explicit currency.
    ///
    /// # Errors
    /// [`RepoError::Db`] on a scope or storage failure; [`RepoError::Conflict`] on classified
    /// database contention. [`RepoError::InvalidStoredMoney`] when a stored amount is malformed
    /// or off its own stored currency contract. [`RepoError::Money`] when a stored AR line's
    /// currency or scale differs from `currency` (the caller's mismatch against the invoice,
    /// not corruption), or when the exact sum leaves the money contract.
    pub async fn read_posted_ar_incl_tax_in<R: DBRunner>(
        &self,
        runner: &R,
        scope: &AccessScope,
        tenant: Uuid,
        origin_invoice_id: &str,
        currency: &CurrencySpec,
    ) -> Result<PostedMoney, RepoError> {
        let entry_ids: Vec<Uuid> = journal_entry::Entity::find()
            .secure()
            .scope_with(scope)
            .filter(
                Condition::all()
                    .add(journal_entry::Column::TenantId.eq(tenant))
                    .add(
                        journal_entry::Column::SourceDocType
                            .eq(SourceDocType::InvoicePost.as_str()),
                    )
                    .add(journal_entry::Column::SourceBusinessId.eq(origin_invoice_id)),
            )
            .all(runner)
            .await
            .map_err(|e| scope_to_repo(e, self.db.db().backend()))?
            .into_iter()
            .map(|e| e.entry_id)
            .collect();
        if entry_ids.is_empty() {
            return Ok(PostedMoney::try_new(Decimal::ZERO, currency.clone())?);
        }

        let lines = journal_line::Entity::find()
            .secure()
            .scope_with(scope)
            .filter(
                Condition::all()
                    .add(journal_line::Column::TenantId.eq(tenant))
                    .add(journal_line::Column::AccountClass.eq(AccountClass::Ar.as_str()))
                    .add(journal_line::Column::InvoiceId.eq(origin_invoice_id))
                    .add(journal_line::Column::EntryId.is_in(entry_ids)),
            )
            .all(runner)
            .await
            .map_err(|e| scope_to_repo(e, self.db.db().backend()))?;
        let mut total = ExactAmount::from_decimal(Decimal::ZERO);
        for line in lines {
            let money = decode_money(&line.amount, &line.currency, line.currency_scale)?;
            caller_spec(&money, currency)?;
            decode_optional_money(
                line.functional_amount.as_deref(),
                line.functional_currency.as_deref(),
                line.functional_currency_scale,
            )?;
            stored_nonnegative(&money)?;
            let amount = exact(&money);
            total = if line.side == Side::Debit.as_str() {
                total.checked_add(&amount)
            } else if line.side == Side::Credit.as_str() {
                total.checked_sub(&amount)
            } else {
                return Err(RepoError::InvalidStoredMoney("invalid AR side".into()));
            }
            .map_err(exact_error)?;
        }
        if total.is_negative() {
            total = ExactAmount::from_decimal(Decimal::ZERO);
        }
        total
            .into_posted_exact(currency.clone())
            .map_err(exact_error)
    }

    /// Check scoped INVOICE_POST existence without leaking another tenant’s invoice.
    ///
    /// # Errors
    /// [`RepoError::Db`] when no connection can be acquired or on a scope / storage failure;
    /// [`RepoError::Conflict`] on classified database contention.
    pub async fn posted_invoice_exists_out_of_txn(
        &self,
        scope: &AccessScope,
        tenant: Uuid,
        origin_invoice_id: &str,
    ) -> Result<bool, RepoError> {
        let conn = self
            .db
            .conn()
            .map_err(|e| db_to_repo(e, self.db.db().backend()))?;
        self.posted_invoice_exists_in(&conn, scope, tenant, origin_invoice_id)
            .await
    }

    /// Check scoped INVOICE_POST existence without leaking another tenant’s invoice.
    ///
    /// # Errors
    /// [`RepoError::Db`] on a scope or storage failure; [`RepoError::Conflict`] on classified
    /// database contention.
    pub async fn posted_invoice_exists_in<R: DBRunner>(
        &self,
        runner: &R,
        scope: &AccessScope,
        tenant: Uuid,
        origin_invoice_id: &str,
    ) -> Result<bool, RepoError> {
        let found = journal_entry::Entity::find()
            .secure()
            .scope_with(scope)
            .filter(
                Condition::all()
                    .add(journal_entry::Column::TenantId.eq(tenant))
                    .add(
                        journal_entry::Column::SourceDocType
                            .eq(SourceDocType::InvoicePost.as_str()),
                    )
                    .add(journal_entry::Column::SourceBusinessId.eq(origin_invoice_id)),
            )
            .one(runner)
            .await
            .map_err(|e| scope_to_repo(e, self.db.db().backend()))?;
        Ok(found.is_some())
    }

    /// Fold positive open-AR contributions exactly, validating every row against the explicit spec.
    ///
    /// # Errors
    /// [`RepoError::Db`] when no connection can be acquired or on a scope / storage failure;
    /// [`RepoError::Conflict`] on classified database contention.
    /// [`RepoError::InvalidStoredMoney`] when a stored amount is malformed or off its own
    /// stored currency contract. [`RepoError::Money`] when a stored AR row's currency or
    /// scale differs from `currency` (the caller's mismatch against the invoice, not
    /// corruption), or when the exact sum leaves the money contract.
    pub async fn read_open_ar_for_invoice_out_of_txn(
        &self,
        scope: &AccessScope,
        tenant: Uuid,
        origin_invoice_id: &str,
        currency: &CurrencySpec,
    ) -> Result<PostedMoney, RepoError> {
        let conn = self
            .db
            .conn()
            .map_err(|e| db_to_repo(e, self.db.db().backend()))?;
        self.read_open_ar_for_invoice_in(&conn, scope, tenant, origin_invoice_id, currency)
            .await
    }

    /// Fold positive open-AR contributions exactly, validating every row against the explicit spec.
    ///
    /// # Errors
    /// [`RepoError::Db`] on a scope or storage failure; [`RepoError::Conflict`] on classified
    /// database contention. [`RepoError::InvalidStoredMoney`] when a stored amount is malformed
    /// or off its own stored currency contract. [`RepoError::Money`] when a stored AR row's
    /// currency or scale differs from `currency` (the caller's mismatch against the invoice,
    /// not corruption), or when the exact sum leaves the money contract.
    pub async fn read_open_ar_for_invoice_in<R: DBRunner>(
        &self,
        runner: &R,
        scope: &AccessScope,
        tenant: Uuid,
        origin_invoice_id: &str,
        currency: &CurrencySpec,
    ) -> Result<PostedMoney, RepoError> {
        let rows = ar_invoice_balance::Entity::find()
            .secure()
            .scope_with(scope)
            .filter(
                Condition::all()
                    .add(ar_invoice_balance::Column::TenantId.eq(tenant))
                    .add(ar_invoice_balance::Column::InvoiceId.eq(origin_invoice_id)),
            )
            .all(runner)
            .await
            .map_err(|e| scope_to_repo(e, self.db.db().backend()))?;
        let mut total = ExactAmount::from_decimal(Decimal::ZERO);
        for row in rows {
            let money = decode_money(&row.balance, &row.currency, row.currency_scale)?;
            caller_spec(&money, currency)?;
            decode_money(&row.disputed, &row.currency, row.currency_scale)?;
            decode_optional_money(
                row.functional_balance.as_deref(),
                row.functional_currency.as_deref(),
                row.functional_currency_scale,
            )?;
            if money.amount() > Decimal::ZERO {
                total = total.checked_add(&exact(&money)).map_err(exact_error)?;
            }
        }
        total
            .into_posted_exact(currency.clone())
            .map_err(exact_error)
    }

    /// Return a scoped validated record, or None when absent or inaccessible.
    ///
    /// # Errors
    /// [`RepoError::Db`] when no connection can be acquired or on a scope / storage failure;
    /// [`RepoError::Conflict`] on classified database contention.
    /// [`RepoError::InvalidStoredMoney`] when a stored amount is malformed or off its currency
    /// contract.
    pub async fn read_exposure_out_of_txn(
        &self,
        scope: &AccessScope,
        tenant: Uuid,
        invoice_id: &str,
    ) -> Result<Option<ExposureView>, RepoError> {
        let conn = self
            .db
            .conn()
            .map_err(|e| db_to_repo(e, self.db.db().backend()))?;
        self.read_exposure_in(&conn, scope, tenant, invoice_id)
            .await
    }

    /// Return a scoped validated record, or None when absent or inaccessible.
    ///
    /// # Errors
    /// [`RepoError::Db`] on a scope or storage failure; [`RepoError::Conflict`] on classified
    /// database contention. [`RepoError::InvalidStoredMoney`] when a stored amount is malformed
    /// or off its currency contract.
    pub async fn read_exposure_in<R: DBRunner>(
        &self,
        runner: &R,
        scope: &AccessScope,
        tenant: Uuid,
        invoice_id: &str,
    ) -> Result<Option<ExposureView>, RepoError> {
        invoice_exposure::Entity::find()
            .secure()
            .scope_with(scope)
            .filter(
                Condition::all()
                    .add(invoice_exposure::Column::TenantId.eq(tenant))
                    .add(invoice_exposure::Column::InvoiceId.eq(invoice_id)),
            )
            .one(runner)
            .await
            .map_err(|e| scope_to_repo(e, self.db.db().backend()))?
            .map(decode_invoice_exposure)
            .transpose()
    }

    /// Seed once; a conflict preserves all historical values and validates the winning spec.
    ///
    /// # Errors
    /// [`RepoError::MoneyOutCapExceeded`] when `original_total` is negative;
    /// [`RepoError::Conflict`] on classified database contention; [`RepoError::Money`] when the
    /// winning row's currency metadata differs from `original_total`; [`RepoError::Db`] on a
    /// scope or storage failure; [`RepoError::InvalidStoredMoney`] when the stored row is
    /// malformed.
    pub async fn seed_exposure_first_touch(
        &self,
        txn: &DbTx<'_>,
        scope: &AccessScope,
        tenant: Uuid,
        invoice_id: &str,
        original_total: &PostedMoney,
    ) -> Result<(), RepoError> {
        nonnegative(original_total)?;
        let am = invoice_exposure::ActiveModel {
            tenant_id: Set(tenant),
            invoice_id: Set(invoice_id.to_owned()),
            currency: Set(original_total.currency().code().to_owned()),
            currency_scale: Set(i16::from(original_total.currency().scale())),
            original_total: Set(encode_amount(original_total)),
            debit_note_total: Set("0".into()),
            credit_note_total: Set("0".into()),
            version: Set(0),
        };
        let on_conflict = SecureOnConflict::<invoice_exposure::Entity>::columns([
            invoice_exposure::Column::TenantId,
            invoice_exposure::Column::InvoiceId,
        ])
        .value(
            invoice_exposure::Column::OriginalTotal,
            Expr::col((
                invoice_exposure::Entity,
                invoice_exposure::Column::OriginalTotal,
            )),
        )
        .map_err(|e| scope_to_repo(e, self.db.db().backend()))?;

        // The self-assigning DO UPDATE makes RETURNING yield the winning row on
        // both a fresh insert and a conflict, so no read-back is needed.
        let winner = invoice_exposure::Entity::insert(am.clone())
            .secure()
            .scope_with_model(scope, &am)
            .map_err(|e| scope_to_repo(e, self.db.db().backend()))?
            .on_conflict(on_conflict)
            .exec_with_returning(txn)
            .await
            .map_err(|e| scope_to_repo(e, self.db.db().backend()))?;
        let winner = decode_invoice_exposure(winner)?;
        matching(&winner.original_total, original_total)?;
        Ok(())
    }

    /// Add an inclusive-tax credit amount after exact headroom validation and version CAS.
    ///
    /// # Errors
    /// [`RepoError::Db`] when the invoice exposure was never seeded, or on a scope / storage
    /// failure; [`RepoError::Money`] when `delta` disagrees with the stored currency metadata or
    /// the exact result leaves the money contract; [`RepoError::MoneyOutCapExceeded`] when the
    /// new totals breach the headroom invariant; [`RepoError::Conflict`] when the observed
    /// version is stale, or on classified database contention; [`RepoError::InvalidStoredMoney`]
    /// when the stored row is malformed or its version overflows.
    pub async fn add_credit_note_total(
        &self,
        txn: &DbTx<'_>,
        scope: &AccessScope,
        tenant: Uuid,
        invoice_id: &str,
        delta: &PostedMoney,
    ) -> Result<(), RepoError> {
        self.change_exposure(
            txn,
            scope,
            tenant,
            invoice_id,
            delta,
            ExposureTotal::CreditNotes,
        )
        .await
    }

    /// Add an inclusive-tax debit amount, widening headroom through version CAS.
    ///
    /// # Errors
    /// [`RepoError::Db`] when the invoice exposure was never seeded, or on a scope / storage
    /// failure; [`RepoError::Money`] when `delta` disagrees with the stored currency metadata or
    /// the exact result leaves the money contract; [`RepoError::MoneyOutCapExceeded`] when the
    /// new totals breach the headroom invariant; [`RepoError::Conflict`] when the observed
    /// version is stale, or on classified database contention; [`RepoError::InvalidStoredMoney`]
    /// when the stored row is malformed or its version overflows.
    pub async fn add_debit_note_total(
        &self,
        txn: &DbTx<'_>,
        scope: &AccessScope,
        tenant: Uuid,
        invoice_id: &str,
        delta: &PostedMoney,
    ) -> Result<(), RepoError> {
        self.change_exposure(
            txn,
            scope,
            tenant,
            invoice_id,
            delta,
            ExposureTotal::DebitNotes,
        )
        .await
    }

    /// Insert an immutable inclusive-tax note; split parts share its spec and exclude tax.
    ///
    /// # Errors
    /// [`RepoError::Money`] when the split parts disagree with the note's currency metadata;
    /// [`RepoError::MoneyOutCapExceeded`] when the amount or a split part is negative;
    /// [`RepoError::Db`] on a scope or storage failure; [`RepoError::Conflict`] on classified
    /// database contention.
    pub async fn insert_credit_note(
        &self,
        txn: &DbTx<'_>,
        scope: &AccessScope,
        note: &NewCreditNote,
    ) -> Result<(), RepoError> {
        validate_note(&note.amount, &note.recognized_part, &note.deferred_part)?;
        let am = credit_note::ActiveModel {
            tenant_id: Set(note.tenant_id),
            credit_note_id: Set(note.credit_note_id.clone()),
            origin_invoice_id: Set(note.origin_invoice_id.clone()),
            origin_invoice_item_ref: Set(note.origin_invoice_item_ref.clone()),
            revenue_stream: Set(note.revenue_stream.clone()),
            currency: Set(note.amount.currency().code().to_owned()),
            currency_scale: Set(i16::from(note.amount.currency().scale())),
            amount: Set(encode_amount(&note.amount)),
            recognized_part: Set(encode_amount(&note.recognized_part)),
            deferred_part: Set(encode_amount(&note.deferred_part)),
            split_basis_ref: Set(note.split_basis_ref.clone()),
            reason_code: Set(note.reason_code.clone()),
            created_at_utc: Set(note.created_at_utc),
        };
        credit_note::Entity::insert(am.clone())
            .secure()
            .scope_with_model(scope, &am)
            .map_err(|e| scope_to_repo(e, self.db.db().backend()))?
            .exec(txn)
            .await
            .map_err(|e| scope_to_repo(e, self.db.db().backend()))?;
        Ok(())
    }

    /// Insert an immutable inclusive-tax note; split parts share its spec and exclude tax.
    ///
    /// # Errors
    /// [`RepoError::Money`] when the split parts disagree with the note's currency metadata;
    /// [`RepoError::MoneyOutCapExceeded`] when the amount or a split part is negative;
    /// [`RepoError::Db`] on a scope or storage failure; [`RepoError::Conflict`] on classified
    /// database contention.
    pub async fn insert_debit_note(
        &self,
        txn: &DbTx<'_>,
        scope: &AccessScope,
        note: &NewDebitNote,
    ) -> Result<(), RepoError> {
        validate_note(&note.amount, &note.recognized_part, &note.deferred_part)?;
        let am = debit_note::ActiveModel {
            tenant_id: Set(note.tenant_id),
            debit_note_id: Set(note.debit_note_id.clone()),
            origin_invoice_id: Set(note.origin_invoice_id.clone()),
            currency: Set(note.amount.currency().code().to_owned()),
            currency_scale: Set(i16::from(note.amount.currency().scale())),
            amount: Set(encode_amount(&note.amount)),
            recognized_part: Set(encode_amount(&note.recognized_part)),
            deferred_part: Set(encode_amount(&note.deferred_part)),
            created_at_utc: Set(note.created_at_utc),
        };
        debit_note::Entity::insert(am.clone())
            .secure()
            .scope_with_model(scope, &am)
            .map_err(|e| scope_to_repo(e, self.db.db().backend()))?
            .exec(txn)
            .await
            .map_err(|e| scope_to_repo(e, self.db.db().backend()))?;
        Ok(())
    }

    /// Insert immutable refund money, retaining phase, pattern and natural-key constraints.
    ///
    /// # Errors
    /// [`RepoError::Db`] on a scope, UNIQUE/PK collision, or storage failure;
    /// [`RepoError::Conflict`] on classified database contention.
    pub async fn insert_refund(
        &self,
        txn: &DbTx<'_>,
        scope: &AccessScope,
        rf: &NewRefund,
    ) -> Result<(), RepoError> {
        nonnegative(&rf.amount)?;
        let am = refund::ActiveModel {
            tenant_id: Set(rf.tenant_id),
            refund_id: Set(rf.refund_id.clone()),
            psp_refund_id: Set(rf.psp_refund_id.clone()),
            phase: Set(rf.phase.clone()),
            pattern: Set(rf.pattern.clone()),
            payment_id: Set(rf.payment_id.clone()),
            invoice_id: Set(rf.invoice_id.clone()),
            currency: Set(rf.amount.currency().code().to_owned()),
            currency_scale: Set(i16::from(rf.amount.currency().scale())),
            amount: Set(encode_amount(&rf.amount)),
            clearing_state: Set(rf.clearing_state.clone()),
            relates_to_refund_id: Set(rf.relates_to_refund_id.clone()),
            reverses_entry_id: Set(rf.reverses_entry_id),
            created_at_utc: Set(rf.created_at_utc),
            version: Set(0),
        };
        refund::Entity::insert(am.clone())
            .secure()
            .scope_with_model(scope, &am)
            .map_err(|e| scope_to_repo(e, self.db.db().backend()))?
            .exec(txn)
            .await
            .map_err(|e| scope_to_repo(e, self.db.db().backend()))?;
        Ok(())
    }

    /// Return a scoped validated record, or None when absent or inaccessible.
    ///
    /// # Errors
    /// [`RepoError::Db`] when no connection can be acquired or on a scope / storage failure;
    /// [`RepoError::Conflict`] on classified database contention.
    /// [`RepoError::InvalidStoredMoney`] when a stored amount is malformed or off its currency
    /// contract.
    pub async fn read_refund_out_of_txn(
        &self,
        scope: &AccessScope,
        tenant: Uuid,
        refund_id: &str,
    ) -> Result<Option<RefundView>, RepoError> {
        let conn = self
            .db
            .conn()
            .map_err(|e| db_to_repo(e, self.db.db().backend()))?;
        self.read_refund_in(&conn, scope, tenant, refund_id).await
    }

    /// Return a scoped validated record, or None when absent or inaccessible.
    ///
    /// # Errors
    /// [`RepoError::Db`] on a scope or storage failure; [`RepoError::Conflict`] on classified
    /// database contention. [`RepoError::InvalidStoredMoney`] when a stored amount is malformed
    /// or off its currency contract.
    pub async fn read_refund_in<R: DBRunner>(
        &self,
        runner: &R,
        scope: &AccessScope,
        tenant: Uuid,
        refund_id: &str,
    ) -> Result<Option<RefundView>, RepoError> {
        refund::Entity::find()
            .secure()
            .scope_with(scope)
            .filter(
                Condition::all()
                    .add(refund::Column::TenantId.eq(tenant))
                    .add(refund::Column::RefundId.eq(refund_id)),
            )
            .one(runner)
            .await
            .map_err(|e| scope_to_repo(e, self.db.db().backend()))?
            .map(decode_refund)
            .transpose()
    }

    /// Return a scoped, ordered page with every money value decoded from stored metadata.
    ///
    /// # Errors
    /// [`OdataPageError::Db`] on a storage / connection failure; [`OdataPageError::Odata`] on a
    /// malformed `$filter` / `$orderby` / cursor (the caller projects it to a canonical 400).
    pub async fn list_refunds(
        &self,
        scope: &AccessScope,
        tenant: Uuid,
        query: &ODataQuery,
    ) -> Result<Page<RefundView>, OdataPageError> {
        let conn = self
            .db
            .conn()
            .map_err(|e| OdataPageError::Db(format!("conn: {e}")))?;
        self.list_refunds_in(&conn, scope, tenant, query).await
    }

    /// Return a scoped, ordered page with every money value decoded from stored metadata.
    ///
    /// # Errors
    /// [`OdataPageError::Db`] on a storage failure or when a stored amount is malformed;
    /// [`OdataPageError::Odata`] on a malformed `$filter` / `$orderby` / cursor (the caller
    /// projects it to a canonical 400).
    pub async fn list_refunds_in<R: DBRunner>(
        &self,
        runner: &R,
        scope: &AccessScope,
        tenant: Uuid,
        query: &ODataQuery,
    ) -> Result<Page<RefundView>, OdataPageError> {
        let base_select = refund::Entity::find()
            .secure()
            .scope_with(scope)
            .filter(Condition::all().add(refund::Column::TenantId.eq(tenant)));
        let query = query_with_default_order(query, "refund_id");
        let page = paginate_odata::<
            RefundFilterField,
            RefundODataMapper,
            refund::Entity,
            refund::Model,
            _,
            _,
        >(
            base_select,
            runner,
            &query,
            ("refund_id", SortDir::Asc),
            LimitCfg {
                default: 25,
                max: 200,
            },
            |m| m,
        )
        .await
        .map_err(map_odata_err)?;
        let items = page
            .items
            .into_iter()
            .map(decode_refund)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| OdataPageError::Db(e.to_string()))?;
        Ok(Page {
            items,
            page_info: page.page_info,
        })
    }

    /// Return a scoped validated record, or None when absent or inaccessible.
    ///
    /// # Errors
    /// [`RepoError::Db`] when no connection can be acquired or on a scope / storage failure;
    /// [`RepoError::Conflict`] on classified database contention.
    /// [`RepoError::InvalidStoredMoney`] when a stored amount is malformed or off its currency
    /// contract.
    pub async fn read_credit_note_out_of_txn(
        &self,
        scope: &AccessScope,
        tenant: Uuid,
        credit_note_id: &str,
    ) -> Result<Option<CreditNoteView>, RepoError> {
        let conn = self
            .db
            .conn()
            .map_err(|e| db_to_repo(e, self.db.db().backend()))?;
        self.read_credit_note_in(&conn, scope, tenant, credit_note_id)
            .await
    }

    /// Return a scoped validated record, or None when absent or inaccessible.
    ///
    /// # Errors
    /// [`RepoError::Db`] on a scope or storage failure; [`RepoError::Conflict`] on classified
    /// database contention. [`RepoError::InvalidStoredMoney`] when a stored amount is malformed
    /// or off its currency contract.
    pub async fn read_credit_note_in<R: DBRunner>(
        &self,
        runner: &R,
        scope: &AccessScope,
        tenant: Uuid,
        credit_note_id: &str,
    ) -> Result<Option<CreditNoteView>, RepoError> {
        credit_note::Entity::find()
            .secure()
            .scope_with(scope)
            .filter(
                Condition::all()
                    .add(credit_note::Column::TenantId.eq(tenant))
                    .add(credit_note::Column::CreditNoteId.eq(credit_note_id)),
            )
            .one(runner)
            .await
            .map_err(|e| scope_to_repo(e, self.db.db().backend()))?
            .map(decode_credit_note)
            .transpose()
    }

    /// Return a scoped, ordered page with every money value decoded from stored metadata.
    ///
    /// # Errors
    /// [`OdataPageError::Db`] on a storage / connection failure; [`OdataPageError::Odata`] on a
    /// malformed `$filter` / `$orderby` / cursor (the caller projects it to a canonical 400).
    pub async fn list_credit_notes(
        &self,
        scope: &AccessScope,
        tenant: Uuid,
        query: &ODataQuery,
    ) -> Result<Page<CreditNoteView>, OdataPageError> {
        let conn = self
            .db
            .conn()
            .map_err(|e| OdataPageError::Db(format!("conn: {e}")))?;
        self.list_credit_notes_in(&conn, scope, tenant, query).await
    }

    /// Return a scoped, ordered page with every money value decoded from stored metadata.
    ///
    /// # Errors
    /// [`OdataPageError::Db`] on a storage failure or when a stored amount is malformed;
    /// [`OdataPageError::Odata`] on a malformed `$filter` / `$orderby` / cursor (the caller
    /// projects it to a canonical 400).
    pub async fn list_credit_notes_in<R: DBRunner>(
        &self,
        runner: &R,
        scope: &AccessScope,
        tenant: Uuid,
        query: &ODataQuery,
    ) -> Result<Page<CreditNoteView>, OdataPageError> {
        let base_select = credit_note::Entity::find()
            .secure()
            .scope_with(scope)
            .filter(Condition::all().add(credit_note::Column::TenantId.eq(tenant)));
        let query = query_with_default_order(query, "credit_note_id");
        let page = paginate_odata::<
            CreditNoteFilterField,
            CreditNoteODataMapper,
            credit_note::Entity,
            credit_note::Model,
            _,
            _,
        >(
            base_select,
            runner,
            &query,
            ("credit_note_id", SortDir::Asc),
            LimitCfg {
                default: 25,
                max: 200,
            },
            |m| m,
        )
        .await
        .map_err(map_odata_err)?;
        let items = page
            .items
            .into_iter()
            .map(decode_credit_note)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| OdataPageError::Db(e.to_string()))?;
        Ok(Page {
            items,
            page_info: page.page_info,
        })
    }

    /// Return a scoped validated record, or None when absent or inaccessible.
    ///
    /// # Errors
    /// [`RepoError::Db`] when no connection can be acquired or on a scope / storage failure;
    /// [`RepoError::Conflict`] on classified database contention.
    /// [`RepoError::InvalidStoredMoney`] when a stored amount is malformed or off its currency
    /// contract.
    pub async fn read_debit_note_out_of_txn(
        &self,
        scope: &AccessScope,
        tenant: Uuid,
        debit_note_id: &str,
    ) -> Result<Option<DebitNoteView>, RepoError> {
        let conn = self
            .db
            .conn()
            .map_err(|e| db_to_repo(e, self.db.db().backend()))?;
        self.read_debit_note_in(&conn, scope, tenant, debit_note_id)
            .await
    }

    /// Return a scoped validated record, or None when absent or inaccessible.
    ///
    /// # Errors
    /// [`RepoError::Db`] on a scope or storage failure; [`RepoError::Conflict`] on classified
    /// database contention. [`RepoError::InvalidStoredMoney`] when a stored amount is malformed
    /// or off its currency contract.
    pub async fn read_debit_note_in<R: DBRunner>(
        &self,
        runner: &R,
        scope: &AccessScope,
        tenant: Uuid,
        debit_note_id: &str,
    ) -> Result<Option<DebitNoteView>, RepoError> {
        debit_note::Entity::find()
            .secure()
            .scope_with(scope)
            .filter(
                Condition::all()
                    .add(debit_note::Column::TenantId.eq(tenant))
                    .add(debit_note::Column::DebitNoteId.eq(debit_note_id)),
            )
            .one(runner)
            .await
            .map_err(|e| scope_to_repo(e, self.db.db().backend()))?
            .map(decode_debit_note)
            .transpose()
    }

    /// Return a scoped, ordered page with every money value decoded from stored metadata.
    ///
    /// # Errors
    /// [`OdataPageError::Db`] on a storage / connection failure; [`OdataPageError::Odata`] on a
    /// malformed `$filter` / `$orderby` / cursor (the caller projects it to a canonical 400).
    pub async fn list_debit_notes(
        &self,
        scope: &AccessScope,
        tenant: Uuid,
        query: &ODataQuery,
    ) -> Result<Page<DebitNoteView>, OdataPageError> {
        let conn = self
            .db
            .conn()
            .map_err(|e| OdataPageError::Db(format!("conn: {e}")))?;
        self.list_debit_notes_in(&conn, scope, tenant, query).await
    }

    /// Return a scoped, ordered page with every money value decoded from stored metadata.
    ///
    /// # Errors
    /// [`OdataPageError::Db`] on a storage failure or when a stored amount is malformed;
    /// [`OdataPageError::Odata`] on a malformed `$filter` / `$orderby` / cursor (the caller
    /// projects it to a canonical 400).
    pub async fn list_debit_notes_in<R: DBRunner>(
        &self,
        runner: &R,
        scope: &AccessScope,
        tenant: Uuid,
        query: &ODataQuery,
    ) -> Result<Page<DebitNoteView>, OdataPageError> {
        let base_select = debit_note::Entity::find()
            .secure()
            .scope_with(scope)
            .filter(Condition::all().add(debit_note::Column::TenantId.eq(tenant)));
        let query = query_with_default_order(query, "debit_note_id");
        let page = paginate_odata::<
            DebitNoteFilterField,
            DebitNoteODataMapper,
            debit_note::Entity,
            debit_note::Model,
            _,
            _,
        >(
            base_select,
            runner,
            &query,
            ("debit_note_id", SortDir::Asc),
            LimitCfg {
                default: 25,
                max: 200,
            },
            |m| m,
        )
        .await
        .map_err(map_odata_err)?;
        let items = page
            .items
            .into_iter()
            .map(decode_debit_note)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| OdataPageError::Db(e.to_string()))?;
        Ok(Page {
            items,
            page_info: page.page_info,
        })
    }

    /// Return a scoped validated record, or None when absent or inaccessible.
    ///
    /// # Errors
    /// [`RepoError::Db`] when no connection can be acquired or on a scope / storage failure;
    /// [`RepoError::Conflict`] on classified database contention.
    /// [`RepoError::InvalidStoredMoney`] when a stored amount is malformed or off its currency
    /// contract.
    pub async fn read_refund_by_psp_phase(
        &self,
        scope: &AccessScope,
        tenant: Uuid,
        psp_refund_id: &str,
        phase: &str,
    ) -> Result<Option<RefundView>, RepoError> {
        let conn = self
            .db
            .conn()
            .map_err(|e| db_to_repo(e, self.db.db().backend()))?;
        self.read_refund_by_psp_phase_in(&conn, scope, tenant, psp_refund_id, phase)
            .await
    }

    /// Return a scoped validated record, or None when absent or inaccessible.
    ///
    /// # Errors
    /// [`RepoError::Db`] on a scope or storage failure; [`RepoError::Conflict`] on classified
    /// database contention. [`RepoError::InvalidStoredMoney`] when a stored amount is malformed
    /// or off its currency contract.
    pub async fn read_refund_by_psp_phase_in<R: DBRunner>(
        &self,
        runner: &R,
        scope: &AccessScope,
        tenant: Uuid,
        psp_refund_id: &str,
        phase: &str,
    ) -> Result<Option<RefundView>, RepoError> {
        refund::Entity::find()
            .secure()
            .scope_with(scope)
            .filter(
                Condition::all()
                    .add(refund::Column::TenantId.eq(tenant))
                    .add(refund::Column::PspRefundId.eq(psp_refund_id))
                    .add(refund::Column::Phase.eq(phase)),
            )
            .one(runner)
            .await
            .map_err(|e| scope_to_repo(e, self.db.db().backend()))?
            .map(decode_refund)
            .transpose()
    }
}

/// Which running total of an invoice's exposure a note moves.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ExposureTotal {
    /// `credit_note_total`: credit notes narrow the headroom.
    CreditNotes,
    /// `debit_note_total`: debit notes widen it.
    DebitNotes,
}

impl ExposureTotal {
    /// The stored total this note kind moves.
    fn of(self, view: &ExposureView) -> &PostedMoney {
        match self {
            Self::CreditNotes => &view.credit_note_total,
            Self::DebitNotes => &view.debit_note_total,
        }
    }

    /// The stored total this note kind moves, for replacement.
    fn of_mut(self, view: &mut ExposureView) -> &mut PostedMoney {
        match self {
            Self::CreditNotes => &mut view.credit_note_total,
            Self::DebitNotes => &mut view.debit_note_total,
        }
    }

    /// The column that stores this total.
    fn column(self) -> invoice_exposure::Column {
        match self {
            Self::CreditNotes => invoice_exposure::Column::CreditNoteTotal,
            Self::DebitNotes => invoice_exposure::Column::DebitNoteTotal,
        }
    }
}

impl AdjustmentRepo {
    /// Read, validate, calculate and CAS once. The caller owns rollback and retries.
    async fn change_exposure(
        &self,
        txn: &DbTx<'_>,
        scope: &AccessScope,
        tenant: Uuid,
        invoice_id: &str,
        delta: &PostedMoney,
        total: ExposureTotal,
    ) -> Result<(), RepoError> {
        let state = self
            .read_exposure_in(txn, scope, tenant, invoice_id)
            .await?
            .ok_or_else(|| {
                RepoError::Db(format!(
                    "invoice_exposure ({tenant}, {invoice_id}) not seeded"
                ))
            })?;
        matching(&state.original_total, delta)?;
        let changed = exact(total.of(&state))
            .checked_add(&exact(delta))
            .map_err(exact_error)?;
        let original = exact(&state.original_total);
        let (debits, credits) = match total {
            ExposureTotal::CreditNotes => (exact(&state.debit_note_total), changed.clone()),
            ExposureTotal::DebitNotes => (changed.clone(), exact(&state.credit_note_total)),
        };
        validate_totals(&original, &debits, &credits)?;
        let narrowed = changed
            .into_posted_exact(delta.currency().clone())
            .map_err(exact_error)?;
        let mut next = state.clone();
        *total.of_mut(&mut next) = narrowed;
        self.write_exposure(txn, scope, &state, &next, total).await
    }

    /// Literal write guarded by the exact version read on this attempt.
    async fn write_exposure(
        &self,
        txn: &DbTx<'_>,
        scope: &AccessScope,
        observed: &ExposureView,
        next: &ExposureView,
        total: ExposureTotal,
    ) -> Result<(), RepoError> {
        let version = checked_version(observed.version)?
            .checked_add(1)
            .ok_or_else(|| RepoError::InvalidStoredMoney("exposure version overflow".into()))?;
        let result = invoice_exposure::Entity::update_many()
            .secure()
            .scope_with(scope)
            .col_expr(total.column(), Expr::value(encode_amount(total.of(next))))
            .col_expr(invoice_exposure::Column::Version, Expr::value(version))
            .filter(
                Condition::all()
                    .add(invoice_exposure::Column::TenantId.eq(observed.tenant_id))
                    .add(invoice_exposure::Column::InvoiceId.eq(&observed.invoice_id))
                    .add(invoice_exposure::Column::Version.eq(observed.version)),
            )
            .exec(txn)
            .await
            .map_err(|e| scope_to_repo(e, self.db.db().backend()))?;
        if result.rows_affected != 1 {
            return Err(RepoError::Conflict("exposure version changed".into()));
        }
        Ok(())
    }
}

/// Validate compatible caller money without interpreting stored history through a registry.
fn matching(a: &PostedMoney, b: &PostedMoney) -> Result<(), RepoError> {
    Ok(a.currency().ensure_same(b.currency())?)
}
/// Compare a stored AR amount with the caller's currency and scale: a different
/// currency or scale is the caller's mismatch against the invoice, not corrupt
/// stored data.
fn caller_spec(money: &PostedMoney, spec: &CurrencySpec) -> Result<(), RepoError> {
    Ok(money.currency().ensure_same(spec)?)
}
/// Validate adjustment arithmetic and preserve the repository error category.
fn nonnegative(money: &PostedMoney) -> Result<(), RepoError> {
    if money.amount() < Decimal::ZERO {
        return Err(RepoError::MoneyOutCapExceeded(
            "negative adjustment amount".into(),
        ));
    }
    Ok(())
}
/// Validate adjustment arithmetic and preserve the repository error category.
fn stored_nonnegative(money: &PostedMoney) -> Result<(), RepoError> {
    if money.amount() < Decimal::ZERO {
        return Err(RepoError::InvalidStoredMoney(
            "negative stored adjustment amount".into(),
        ));
    }
    Ok(())
}
/// Parts exclude tax, so their sum deliberately need not equal the inclusive total.
fn validate_note(
    amount: &PostedMoney,
    recognized: &PostedMoney,
    deferred: &PostedMoney,
) -> Result<(), RepoError> {
    matching(amount, recognized)?;
    matching(amount, deferred)?;
    nonnegative(amount)?;
    nonnegative(recognized)?;
    nonnegative(deferred)
}
/// Validate adjustment arithmetic and preserve the repository error category.
fn exact(money: &PostedMoney) -> ExactAmount {
    ExactAmount::from_decimal(money.amount())
}
/// Validate adjustment arithmetic and preserve the repository error category.
fn exact_error(error: ExactError) -> RepoError {
    match error {
        ExactError::Money(e) => RepoError::Money(e),
        e => RepoError::Db(format!("exact adjustment: {e}")),
    }
}
/// Validate adjustment arithmetic and preserve the repository error category.
fn checked_version(version: i64) -> Result<i64, RepoError> {
    if version < 0 {
        return Err(RepoError::InvalidStoredMoney(
            "negative adjustment version".into(),
        ));
    }
    Ok(version)
}
/// Reject impossible persisted totals as corruption before considering any new delta.
fn validate_exposure(state: &ExposureView) -> Result<(), RepoError> {
    validate_totals(
        &exact(&state.original_total),
        &exact(&state.debit_note_total),
        &exact(&state.credit_note_total),
    )
    .map_err(|error| RepoError::InvalidStoredMoney(format!("invalid stored exposure: {error}")))
}
/// Validate adjustment arithmetic and preserve the repository error category.
fn validate_totals(
    original: &ExactAmount,
    debit: &ExactAmount,
    credit: &ExactAmount,
) -> Result<(), RepoError> {
    let ceiling = original.checked_add(debit).map_err(exact_error)?;
    if original.is_negative() || debit.is_negative() || credit.is_negative() || *credit > ceiling {
        return Err(RepoError::MoneyOutCapExceeded(
            "invoice exposure headroom exceeded".into(),
        ));
    }
    Ok(())
}

#[cfg(test)]
#[path = "adjustment_repo_tests.rs"]
mod tests;
