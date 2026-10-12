//! `BalanceProjector` — derives per-grain signed deltas from the posted
//! lines and upserts the derived balance caches inside the posting
//! transaction, in a fixed lock-key order (deadlock-freedom), re-asserting
//! the no-negative invariant on the guarded account classes (the DB
//! conditional CHECK from P1 is the backstop).
//!
//! Grains:
//! - `account_balance` `(tenant, account, currency)` — every line;
//! - `ar_payer_balance` `(tenant, payer, account, currency)` — `AR` lines;
//! - `ar_invoice_balance` `(tenant, payer, account, invoice)` — `AR` lines
//!   carrying an `invoice_id`;
//! - `unallocated_balance` `(tenant, payer, currency)` — unapplied cash;
//! - `reusable_credit_subbalance`
//!   `(tenant, payer, currency, credit_grant_event_type)` —
//!   `REUSABLE_CREDIT` lines (the wallet sub-grain);
//! - `tax_subbalance` `(tenant, account, jurisdiction, filing)` —
//!   `TAX_PAYABLE` lines carrying both tax dims.
//!
//! A line's signed delta is `+amount` when its side equals the account's
//! normal side, else `-amount`.

use std::collections::HashMap;

use super::retry::{insert_to_repo, scope_to_repo};
use crate::domain::exact_money::{ExactAmount, ExactError};
use crate::domain::model::RepoError;
use crate::infra::storage::money_text::{decode_money, decode_optional_money, encode_amount};
use bss_ledger_sdk::{AccountClass, Side};
use bss_ledger_sdk::{CurrencySpec, MoneyError, PostedMoney};
use chrono::NaiveDate;
use rust_decimal::Decimal;
use sea_orm::sea_query::Expr;
use sea_orm::{ActiveValue::Set, ColumnTrait, Condition, EntityTrait, QueryFilter};
use toolkit_db::secure::{AccessScope, DbTx, SecureEntityExt, SecureInsertExt, SecureUpdateExt};
use uuid::Uuid;

use crate::domain::model::{NewEntry, NewLine};
use crate::domain::status::AR_STATUS_DISPUTED;
use crate::infra::storage::entity::{
    account_balance, ar_invoice_balance, ar_payer_balance, reusable_credit_subbalance,
    tax_subbalance, unallocated_balance,
};
use time::OffsetDateTime;

/// Projection error.
#[derive(Debug, thiserror::Error)]
pub enum ProjectError {
    /// Validated money, stored-data, or scope error (preserves typed conflicts).
    #[error(transparent)]
    Repo(#[from] RepoError),
    /// A stale cache version requires a fresh whole transaction.
    #[error("concurrent cache modification")]
    Conflict,
    /// A guarded balance would go negative after applying a delta.
    #[error("balance for account {account_id} would go negative ({balance})")]
    NegativeBalance { account_id: Uuid, balance: String },
    /// An account's `normal_side` was not supplied in the lookup map.
    #[error("missing normal_side for account {0}")]
    MissingNormalSide(Uuid),
    /// A `REUSABLE_CREDIT` line reached projection without its wallet sub-grain
    /// bucket (`credit_grant_event_type`). The domain builders always set it, so
    /// this is an invariant breach — projecting it would key a phantom "" sub-
    /// balance, which the DB NOT-NULL CHECK does not catch (it tests NULL, not "").
    #[error("REUSABLE_CREDIT line {0} missing credit_grant_event_type")]
    MissingCreditEventType(Uuid),
    /// Exact arithmetic exceeded its digit budget (an internal invariant breach).
    #[error("balance projector arithmetic: {0}")]
    Arithmetic(#[source] ExactError),
    /// Underlying storage failure.
    #[error("balance projector db error: {0}")]
    Db(String),
}

/// One derived cache mutation, keyed by its grain. `table_rank` orders the
/// six cache tables; the remaining key parts order rows within a table so
/// concurrent posts acquire row locks in a single global order.
#[derive(Clone, Debug, PartialEq, Eq)]
struct GrainDelta {
    table_rank: GrainTable,
    tenant_id: Uuid,
    account_id: Uuid,
    currency: String,
    payer_tenant_id: Uuid,
    invoice_id: String,
    tax_jurisdiction: String,
    tax_filing_period: String,
    account_class: AccountClass,
    normal_side: Side,
    delta: ExactAmount,
    currency_spec: CurrencySpec,
    functional_delta: ExactAmount,
    functional_currency: Option<CurrencySpec>,
    disputed_delta: ExactAmount,
    /// AR-invoice grain only (decision P): the entry's posted-at and the line's
    /// due date, stamped first-write-wins onto `ar_invoice_balance` so the
    /// oldest-first allocation precedence has a stable post date. Other grains
    /// carry the defaults (`posted_at` is unused, `due_date` is `None`).
    posted_at: OffsetDateTime,
    due_date: Option<NaiveDate>,
    /// Reusable-credit grain only: the credit-grant event type that sub-divides
    /// the wallet balance (a PK dim), and the entry's posted-at stamped
    /// first-write-wins onto `reusable_credit_subbalance.first_granted_at` as a
    /// recency marker. Other grains carry the defaults (empty event type,
    /// `first_granted_at` is `None`).
    credit_grant_event_type: String,
    first_granted_at: Option<OffsetDateTime>,
}

/// The canonical lock-order sort key for a [`GrainDelta`]: `(table_rank, tenant,
/// account, currency, payer, invoice, tax_juris, tax_filing, credit_grant_event_type)`.
/// Borrows the row's string dims, so it carries the delta's lifetime.
type GrainSortKey<'a> = (
    GrainTable,
    Uuid,
    Uuid,
    &'a str,
    Uuid,
    &'a str,
    &'a str,
    &'a str,
    &'a str,
);

impl GrainDelta {
    /// Sort by business identity only: non-key axes are constant placeholders.
    /// In particular, currency/scale cannot split an invoice or tax grain, and
    /// resolved account attributes cannot split payer wallet grains.
    fn sort_key(&self) -> GrainSortKey<'_> {
        (
            self.table_rank,
            self.tenant_id,
            if matches!(
                self.table_rank,
                GrainTable::Unallocated | GrainTable::ReusableCredit
            ) {
                Uuid::nil()
            } else {
                self.account_id
            },
            if matches!(self.table_rank, GrainTable::ArInvoice | GrainTable::Tax) {
                ""
            } else {
                &self.currency
            },
            if matches!(self.table_rank, GrainTable::Account | GrainTable::Tax) {
                Uuid::nil()
            } else {
                self.payer_tenant_id
            },
            &self.invoice_id,
            &self.tax_jurisdiction,
            &self.tax_filing_period,
            &self.credit_grant_event_type,
        )
    }
}

/// Cache acquisition order, preserved across all six writers.
/// Workflow repositories must acquire their own grains consistently with this order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum GrainTable {
    Account,
    ArPayer,
    ArInvoice,
    Unallocated,
    ReusableCredit,
    Tax,
}

/// Projects posted lines into the derived balance caches.
#[derive(Clone)]
pub struct BalanceProjector {
    backend: sea_orm::DbBackend,
}

impl BalanceProjector {
    /// Bind error classification to the provider backend.
    #[must_use]
    pub fn new(backend: sea_orm::DbBackend) -> Self {
        Self { backend }
    }

    /// Derive per-grain signed deltas, sort them into the canonical lock
    /// order, and upsert each cache row, re-asserting no-negative on the
    /// guarded classes. `created_seq` stamps `last_entry_seq` on every
    /// touched row.
    ///
    /// # Errors
    /// [`ProjectError::MissingNormalSide`] if a line's account is absent from
    /// `normal_sides`; [`ProjectError::NegativeBalance`] if a guarded balance
    /// would go negative; [`ProjectError::Db`] on a storage failure.
    ///
    pub async fn project(
        &self,
        txn: &DbTx<'_>,
        scope: &AccessScope,
        entry: &NewEntry,
        lines: &[NewLine],
        normal_sides: &HashMap<Uuid, Side>,
        created_seq: i64,
    ) -> Result<(), ProjectError> {
        let grains = derive_grains(entry, lines, normal_sides)?;

        for g in grains {
            match g.table_rank {
                GrainTable::Account => {
                    self.upsert_account_balance(txn, scope, &g, created_seq)
                        .await?;
                }
                GrainTable::ArPayer => self.upsert_ar_payer(txn, scope, &g, created_seq).await?,
                GrainTable::ArInvoice => {
                    self.upsert_ar_invoice(txn, scope, &g, created_seq).await?;
                }
                GrainTable::Unallocated => {
                    self.upsert_unallocated(txn, scope, &g, created_seq).await?;
                }
                GrainTable::ReusableCredit => {
                    self.upsert_reusable_credit(txn, scope, &g, created_seq)
                        .await?;
                }
                GrainTable::Tax => self.upsert_tax(txn, scope, &g, created_seq).await?,
            }
        }
        Ok(())
    }

    /// Read the secured grain and publish one literal, version-guarded post-state.
    async fn upsert_account_balance(
        &self,
        txn: &DbTx<'_>,
        scope: &AccessScope,
        g: &GrainDelta,
        seq: i64,
    ) -> Result<(), ProjectError> {
        use account_balance::{ActiveModel, Column, Entity};
        let key = Condition::all()
            .add(Column::TenantId.eq(g.tenant_id))
            .add(Column::AccountId.eq(g.account_id))
            .add(Column::Currency.eq(g.currency.clone()));
        let existing = Entity::find()
            .filter(key.clone())
            .secure()
            .scope_with(scope)
            .one(txn)
            .await
            .map_err(|e| scope_to_repo(e, self.backend))?;
        let stored = existing
            .as_ref()
            .map(|r| decode_money(&r.balance, &r.currency, r.currency_scale))
            .transpose()?;
        let functional = existing
            .as_ref()
            .map(|r| {
                decode_optional_money(
                    r.functional_balance.as_deref(),
                    r.functional_currency.as_deref(),
                    r.functional_currency_scale,
                )
            })
            .transpose()?
            .flatten();
        if let Some(r) = &existing {
            if r.account_id != g.account_id {
                return Err(cache_account_mismatch(g, r.account_id));
            }
            if r.account_class != g.account_class.as_str()
                || r.normal_side != g.normal_side.as_str()
            {
                return Err(ProjectError::Db(
                    "cache account classification mismatch".into(),
                ));
            }
        }
        let (balance, functional_balance) =
            final_balances(g, stored.as_ref(), functional.as_ref())?;
        if let Some(r) = existing {
            let next = next_version(r.version)?;
            let result = Entity::update_many()
                .secure()
                .scope_with(scope)
                .filter(key.add(Column::Version.eq(r.version)))
                .col_expr(Column::Balance, Expr::value(encode_amount(&balance)))
                .col_expr(Column::Version, Expr::value(next))
                .col_expr(Column::LastEntrySeq, Expr::value(Some(seq)))
                .col_expr(
                    Column::FunctionalBalance,
                    Expr::value(functional_balance.as_ref().map(encode_amount)),
                )
                .exec(txn)
                .await
                .map_err(|e| scope_to_repo(e, self.backend))?;
            require_one(result.rows_affected)?;
        } else {
            let am = ActiveModel {
                tenant_id: Set(g.tenant_id),
                account_id: Set(g.account_id),
                currency: Set(g.currency.clone()),
                currency_scale: Set(i16::from(g.currency_spec.scale())),
                account_class: Set(g.account_class.as_str().to_owned()),
                normal_side: Set(g.normal_side.as_str().to_owned()),
                balance: Set(encode_amount(&balance)),
                functional_balance: Set(functional_balance.as_ref().map(encode_amount)),
                functional_currency: Set(g
                    .functional_currency
                    .as_ref()
                    .map(|c| c.code().to_owned())),
                functional_currency_scale: Set(g
                    .functional_currency
                    .as_ref()
                    .map(|c| i16::from(c.scale()))),
                last_entry_seq: Set(Some(seq)),
                version: Set(0),
            };
            Entity::insert(am.clone())
                .secure()
                .scope_with_model(scope, &am)
                .map_err(|e| scope_to_repo(e, self.backend))?
                .exec(txn)
                .await
                .map_err(|e| insert_to_repo(e, self.backend))?;
        }
        Ok(())
    }
    /// Read the secured grain and publish one literal, version-guarded post-state.
    async fn upsert_ar_payer(
        &self,
        txn: &DbTx<'_>,
        scope: &AccessScope,
        g: &GrainDelta,
        seq: i64,
    ) -> Result<(), ProjectError> {
        use ar_payer_balance::{ActiveModel, Column, Entity};
        let key = Condition::all()
            .add(Column::TenantId.eq(g.tenant_id))
            .add(Column::PayerTenantId.eq(g.payer_tenant_id))
            .add(Column::AccountId.eq(g.account_id))
            .add(Column::Currency.eq(g.currency.clone()));
        let existing = Entity::find()
            .filter(key.clone())
            .secure()
            .scope_with(scope)
            .one(txn)
            .await
            .map_err(|e| scope_to_repo(e, self.backend))?;
        let stored = existing
            .as_ref()
            .map(|r| decode_money(&r.balance, &r.currency, r.currency_scale))
            .transpose()?;
        let functional = existing
            .as_ref()
            .map(|r| {
                decode_optional_money(
                    r.functional_balance.as_deref(),
                    r.functional_currency.as_deref(),
                    r.functional_currency_scale,
                )
            })
            .transpose()?
            .flatten();
        if let Some(r) = &existing
            && r.account_id != g.account_id
        {
            return Err(cache_account_mismatch(g, r.account_id));
        }
        let (balance, functional_balance) =
            final_balances(g, stored.as_ref(), functional.as_ref())?;
        if let Some(r) = existing {
            let next = next_version(r.version)?;
            let result = Entity::update_many()
                .secure()
                .scope_with(scope)
                .filter(key.add(Column::Version.eq(r.version)))
                .col_expr(Column::Balance, Expr::value(encode_amount(&balance)))
                .col_expr(Column::Version, Expr::value(next))
                .col_expr(Column::LastEntrySeq, Expr::value(Some(seq)))
                .col_expr(
                    Column::FunctionalBalance,
                    Expr::value(functional_balance.as_ref().map(encode_amount)),
                )
                .exec(txn)
                .await
                .map_err(|e| scope_to_repo(e, self.backend))?;
            require_one(result.rows_affected)?;
        } else {
            let am = ActiveModel {
                tenant_id: Set(g.tenant_id),
                payer_tenant_id: Set(g.payer_tenant_id),
                account_id: Set(g.account_id),
                currency: Set(g.currency.clone()),
                currency_scale: Set(i16::from(g.currency_spec.scale())),
                balance: Set(encode_amount(&balance)),
                functional_balance: Set(functional_balance.as_ref().map(encode_amount)),
                functional_currency: Set(g
                    .functional_currency
                    .as_ref()
                    .map(|c| c.code().to_owned())),
                functional_currency_scale: Set(g
                    .functional_currency
                    .as_ref()
                    .map(|c| i16::from(c.scale()))),
                last_entry_seq: Set(Some(seq)),
                version: Set(0),
            };
            Entity::insert(am.clone())
                .secure()
                .scope_with_model(scope, &am)
                .map_err(|e| scope_to_repo(e, self.backend))?
                .exec(txn)
                .await
                .map_err(|e| insert_to_repo(e, self.backend))?;
        }
        Ok(())
    }
    /// Read the secured grain and publish one literal, version-guarded post-state.
    async fn upsert_ar_invoice(
        &self,
        txn: &DbTx<'_>,
        scope: &AccessScope,
        g: &GrainDelta,
        seq: i64,
    ) -> Result<(), ProjectError> {
        use ar_invoice_balance::{ActiveModel, Column, Entity};
        let key = Condition::all()
            .add(Column::TenantId.eq(g.tenant_id))
            .add(Column::PayerTenantId.eq(g.payer_tenant_id))
            .add(Column::AccountId.eq(g.account_id))
            .add(Column::InvoiceId.eq(g.invoice_id.clone()));
        let existing = Entity::find()
            .filter(key.clone())
            .secure()
            .scope_with(scope)
            .one(txn)
            .await
            .map_err(|e| scope_to_repo(e, self.backend))?;
        let stored = existing
            .as_ref()
            .map(|r| decode_money(&r.balance, &r.currency, r.currency_scale))
            .transpose()?;
        let functional = existing
            .as_ref()
            .map(|r| {
                decode_optional_money(
                    r.functional_balance.as_deref(),
                    r.functional_currency.as_deref(),
                    r.functional_currency_scale,
                )
            })
            .transpose()?
            .flatten();
        if let Some(r) = &existing
            && r.account_id != g.account_id
        {
            return Err(cache_account_mismatch(g, r.account_id));
        }
        let (balance, functional_balance) =
            final_balances(g, stored.as_ref(), functional.as_ref())?;
        let stored_disputed = existing
            .as_ref()
            .map(|r| decode_money(&r.disputed, &r.currency, r.currency_scale))
            .transpose()?;
        if let (Some(total), Some(disputed)) = (stored.as_ref(), stored_disputed.as_ref())
            && (disputed.amount() < Decimal::ZERO || disputed.amount() > total.amount())
        {
            return Err(
                RepoError::InvalidStoredMoney("disputed balance outside total".into()).into(),
            );
        }
        let disputed = final_amount(
            &g.disputed_delta,
            stored_disputed.as_ref(),
            &g.currency_spec,
        )?;
        if disputed.amount() < Decimal::ZERO || disputed.amount() > balance.amount() {
            return Err(ProjectError::Db(
                "disputed balance must be within total balance".into(),
            ));
        }
        if let Some(r) = existing {
            let next = next_version(r.version)?;
            let result = Entity::update_many()
                .secure()
                .scope_with(scope)
                .filter(key.add(Column::Version.eq(r.version)))
                .col_expr(Column::Balance, Expr::value(encode_amount(&balance)))
                .col_expr(Column::Version, Expr::value(next))
                .col_expr(Column::LastEntrySeq, Expr::value(Some(seq)))
                .col_expr(
                    Column::FunctionalBalance,
                    Expr::value(functional_balance.as_ref().map(encode_amount)),
                )
                .col_expr(Column::Disputed, Expr::value(encode_amount(&disputed)))
                .exec(txn)
                .await
                .map_err(|e| scope_to_repo(e, self.backend))?;
            require_one(result.rows_affected)?;
        } else {
            let am = ActiveModel {
                tenant_id: Set(g.tenant_id),
                payer_tenant_id: Set(g.payer_tenant_id),
                account_id: Set(g.account_id),
                invoice_id: Set(g.invoice_id.clone()),
                currency: Set(g.currency.clone()),
                currency_scale: Set(i16::from(g.currency_spec.scale())),
                balance: Set(encode_amount(&balance)),
                disputed: Set(encode_amount(&disputed)),
                functional_balance: Set(functional_balance.as_ref().map(encode_amount)),
                functional_currency: Set(g
                    .functional_currency
                    .as_ref()
                    .map(|c| c.code().to_owned())),
                functional_currency_scale: Set(g
                    .functional_currency
                    .as_ref()
                    .map(|c| i16::from(c.scale()))),
                original_posted_at: Set(Some(g.posted_at)),
                due_date: Set(g.due_date),
                last_entry_seq: Set(Some(seq)),
                version: Set(0),
            };
            Entity::insert(am.clone())
                .secure()
                .scope_with_model(scope, &am)
                .map_err(|e| scope_to_repo(e, self.backend))?
                .exec(txn)
                .await
                .map_err(|e| insert_to_repo(e, self.backend))?;
        }
        Ok(())
    }
    /// Read the secured grain and publish one literal, version-guarded post-state.
    async fn upsert_unallocated(
        &self,
        txn: &DbTx<'_>,
        scope: &AccessScope,
        g: &GrainDelta,
        seq: i64,
    ) -> Result<(), ProjectError> {
        use unallocated_balance::{ActiveModel, Column, Entity};
        let key = Condition::all()
            .add(Column::TenantId.eq(g.tenant_id))
            .add(Column::PayerTenantId.eq(g.payer_tenant_id))
            .add(Column::Currency.eq(g.currency.clone()));
        let existing = Entity::find()
            .filter(key.clone())
            .secure()
            .scope_with(scope)
            .one(txn)
            .await
            .map_err(|e| scope_to_repo(e, self.backend))?;
        let stored = existing
            .as_ref()
            .map(|r| decode_money(&r.balance, &r.currency, r.currency_scale))
            .transpose()?;
        let functional = existing
            .as_ref()
            .map(|r| {
                decode_optional_money(
                    r.functional_balance.as_deref(),
                    r.functional_currency.as_deref(),
                    r.functional_currency_scale,
                )
            })
            .transpose()?
            .flatten();
        if let Some(r) = &existing
            && r.account_id != g.account_id
        {
            return Err(cache_account_mismatch(g, r.account_id));
        }
        let (balance, functional_balance) =
            final_balances(g, stored.as_ref(), functional.as_ref())?;
        if let Some(r) = existing {
            let next = next_version(r.version)?;
            let result = Entity::update_many()
                .secure()
                .scope_with(scope)
                .filter(key.add(Column::Version.eq(r.version)))
                .col_expr(Column::Balance, Expr::value(encode_amount(&balance)))
                .col_expr(Column::Version, Expr::value(next))
                .col_expr(Column::LastEntrySeq, Expr::value(Some(seq)))
                .col_expr(
                    Column::FunctionalBalance,
                    Expr::value(functional_balance.as_ref().map(encode_amount)),
                )
                .exec(txn)
                .await
                .map_err(|e| scope_to_repo(e, self.backend))?;
            require_one(result.rows_affected)?;
        } else {
            let am = ActiveModel {
                tenant_id: Set(g.tenant_id),
                payer_tenant_id: Set(g.payer_tenant_id),
                account_id: Set(g.account_id),
                currency: Set(g.currency.clone()),
                currency_scale: Set(i16::from(g.currency_spec.scale())),
                balance: Set(encode_amount(&balance)),
                functional_balance: Set(functional_balance.as_ref().map(encode_amount)),
                functional_currency: Set(g
                    .functional_currency
                    .as_ref()
                    .map(|c| c.code().to_owned())),
                functional_currency_scale: Set(g
                    .functional_currency
                    .as_ref()
                    .map(|c| i16::from(c.scale()))),
                last_entry_seq: Set(Some(seq)),
                version: Set(0),
            };
            Entity::insert(am.clone())
                .secure()
                .scope_with_model(scope, &am)
                .map_err(|e| scope_to_repo(e, self.backend))?
                .exec(txn)
                .await
                .map_err(|e| insert_to_repo(e, self.backend))?;
        }
        Ok(())
    }
    /// Read the secured grain and publish one literal, version-guarded post-state.
    async fn upsert_reusable_credit(
        &self,
        txn: &DbTx<'_>,
        scope: &AccessScope,
        g: &GrainDelta,
        seq: i64,
    ) -> Result<(), ProjectError> {
        use reusable_credit_subbalance::{ActiveModel, Column, Entity};
        let key = Condition::all()
            .add(Column::TenantId.eq(g.tenant_id))
            .add(Column::PayerTenantId.eq(g.payer_tenant_id))
            .add(Column::Currency.eq(g.currency.clone()))
            .add(Column::CreditGrantEventType.eq(g.credit_grant_event_type.clone()));
        let existing = Entity::find()
            .filter(key.clone())
            .secure()
            .scope_with(scope)
            .one(txn)
            .await
            .map_err(|e| scope_to_repo(e, self.backend))?;
        let stored = existing
            .as_ref()
            .map(|r| decode_money(&r.balance, &r.currency, r.currency_scale))
            .transpose()?;
        let functional = existing
            .as_ref()
            .map(|r| {
                decode_optional_money(
                    r.functional_balance.as_deref(),
                    r.functional_currency.as_deref(),
                    r.functional_currency_scale,
                )
            })
            .transpose()?
            .flatten();
        if let Some(r) = &existing
            && r.account_id != g.account_id
        {
            return Err(cache_account_mismatch(g, r.account_id));
        }
        let (balance, functional_balance) =
            final_balances(g, stored.as_ref(), functional.as_ref())?;
        if let Some(r) = existing {
            let next = next_version(r.version)?;
            let result = Entity::update_many()
                .secure()
                .scope_with(scope)
                .filter(key.add(Column::Version.eq(r.version)))
                .col_expr(Column::Balance, Expr::value(encode_amount(&balance)))
                .col_expr(Column::Version, Expr::value(next))
                .col_expr(Column::LastEntrySeq, Expr::value(Some(seq)))
                .col_expr(
                    Column::FunctionalBalance,
                    Expr::value(functional_balance.as_ref().map(encode_amount)),
                )
                .exec(txn)
                .await
                .map_err(|e| scope_to_repo(e, self.backend))?;
            require_one(result.rows_affected)?;
        } else {
            let am = ActiveModel {
                tenant_id: Set(g.tenant_id),
                payer_tenant_id: Set(g.payer_tenant_id),
                account_id: Set(g.account_id),
                currency: Set(g.currency.clone()),
                currency_scale: Set(i16::from(g.currency_spec.scale())),
                credit_grant_event_type: Set(g.credit_grant_event_type.clone()),
                first_granted_at: Set(g.first_granted_at),
                balance: Set(encode_amount(&balance)),
                functional_balance: Set(functional_balance.as_ref().map(encode_amount)),
                functional_currency: Set(g
                    .functional_currency
                    .as_ref()
                    .map(|c| c.code().to_owned())),
                functional_currency_scale: Set(g
                    .functional_currency
                    .as_ref()
                    .map(|c| i16::from(c.scale()))),
                last_entry_seq: Set(Some(seq)),
                version: Set(0),
            };
            Entity::insert(am.clone())
                .secure()
                .scope_with_model(scope, &am)
                .map_err(|e| scope_to_repo(e, self.backend))?
                .exec(txn)
                .await
                .map_err(|e| insert_to_repo(e, self.backend))?;
        }
        Ok(())
    }
    /// Read the secured grain and publish one literal, version-guarded post-state.
    async fn upsert_tax(
        &self,
        txn: &DbTx<'_>,
        scope: &AccessScope,
        g: &GrainDelta,
        seq: i64,
    ) -> Result<(), ProjectError> {
        use tax_subbalance::{ActiveModel, Column, Entity};
        let key = Condition::all()
            .add(Column::TenantId.eq(g.tenant_id))
            .add(Column::AccountId.eq(g.account_id))
            .add(Column::TaxJurisdiction.eq(g.tax_jurisdiction.clone()))
            .add(Column::TaxFilingPeriod.eq(g.tax_filing_period.clone()));
        let existing = Entity::find()
            .filter(key.clone())
            .secure()
            .scope_with(scope)
            .one(txn)
            .await
            .map_err(|e| scope_to_repo(e, self.backend))?;
        let stored = existing
            .as_ref()
            .map(|r| decode_money(&r.balance, &r.currency, r.currency_scale))
            .transpose()?;
        let functional = None;
        if let Some(r) = &existing
            && r.account_id != g.account_id
        {
            return Err(cache_account_mismatch(g, r.account_id));
        }
        let (balance, _) = final_balances(g, stored.as_ref(), functional.as_ref())?;
        if let Some(r) = existing {
            let next = next_version(r.version)?;
            let result = Entity::update_many()
                .secure()
                .scope_with(scope)
                .filter(key.add(Column::Version.eq(r.version)))
                .col_expr(Column::Balance, Expr::value(encode_amount(&balance)))
                .col_expr(Column::Version, Expr::value(next))
                .col_expr(Column::LastEntrySeq, Expr::value(Some(seq)))
                .exec(txn)
                .await
                .map_err(|e| scope_to_repo(e, self.backend))?;
            require_one(result.rows_affected)?;
        } else {
            let am = ActiveModel {
                currency: Set(g.currency.clone()),
                currency_scale: Set(i16::from(g.currency_spec.scale())),
                tenant_id: Set(g.tenant_id),
                account_id: Set(g.account_id),
                tax_jurisdiction: Set(g.tax_jurisdiction.clone()),
                tax_filing_period: Set(g.tax_filing_period.clone()),
                balance: Set(encode_amount(&balance)),
                last_entry_seq: Set(Some(seq)),
                version: Set(0),
            };
            Entity::insert(am.clone())
                .secure()
                .scope_with_model(scope, &am)
                .map_err(|e| scope_to_repo(e, self.backend))?
                .exec(txn)
                .await
                .map_err(|e| insert_to_repo(e, self.backend))?;
        }
        Ok(())
    }
}

/// Reject stale writes, including unexpected multiple-row updates.
fn require_one(rows: u64) -> Result<(), ProjectError> {
    if rows == 1 {
        Ok(())
    } else {
        Err(ProjectError::Conflict)
    }
}

/// Versions are counters, never saturating money or wrapping integers.
fn next_version(version: i64) -> Result<i64, ProjectError> {
    version
        .checked_add(1)
        .ok_or_else(|| ProjectError::Db("cache version overflow".into()))
}

/// Preserve named money errors while keeping arithmetic-budget failures internal.
impl From<ExactError> for ProjectError {
    fn from(value: ExactError) -> Self {
        match value {
            ExactError::Money(e) => Self::Repo(RepoError::Money(e)),
            e => Self::Arithmetic(e),
        }
    }
}

/// Validate metadata before arithmetic; scales never create an additional grain.
fn match_currency(a: &CurrencySpec, b: &CurrencySpec) -> Result<(), ProjectError> {
    a.ensure_same(b).map_err(|e| RepoError::Money(e).into())
}

/// Optional functional metadata is immutable, including absence.
fn match_functional(
    a: Option<&CurrencySpec>,
    b: Option<&CurrencySpec>,
) -> Result<(), ProjectError> {
    match (a, b) {
        (Some(a), Some(b)) => match_currency(a, b),
        (None, None) => Ok(()),
        _ => Err(RepoError::Money(MoneyError::CurrencyMismatch).into()),
    }
}

/// Add the original stored value before the sole bounded narrowing step.
fn final_amount(
    delta: &ExactAmount,
    stored: Option<&PostedMoney>,
    currency: &CurrencySpec,
) -> Result<PostedMoney, ProjectError> {
    if let Some(stored) = stored {
        match_currency(currency, stored.currency())?;
    }
    Ok(delta
        .checked_add(&ExactAmount::from_decimal(
            stored.map_or(Decimal::ZERO, PostedMoney::amount),
        ))?
        .into_posted_exact(currency.clone())?)
}

/// The grain's business key, for diagnostics: table, tenant, payer, currency and
/// whichever of invoice, credit-grant event type and tax dims the table keys on.
fn grain_label(g: &GrainDelta) -> String {
    use std::fmt::Write as _;
    let mut label = format!(
        "{:?} tenant {} payer {} currency {}",
        g.table_rank, g.tenant_id, g.payer_tenant_id, g.currency
    );
    for (name, value) in [
        ("invoice", &g.invoice_id),
        ("credit_grant_event_type", &g.credit_grant_event_type),
        ("tax_jurisdiction", &g.tax_jurisdiction),
        ("tax_filing_period", &g.tax_filing_period),
    ] {
        if !value.is_empty() {
            let _ = write!(label, " {name} {value}");
        }
    }
    label
}

/// A cached row whose key omits `account_id` (or matched it) holds another
/// account than the posting line: name the grain and both accounts.
fn cache_account_mismatch(g: &GrainDelta, cached_account: Uuid) -> ProjectError {
    ProjectError::Db(format!(
        "projector: cached row for {} holds account {cached_account}, the posting targets account {}",
        grain_label(g),
        g.account_id
    ))
}

/// Validate both metadata sets before computing correlated post-state values.
/// A stored balance means the row exists; the tax grain carries no functional
/// balance, so its functional metadata is neither checked nor produced.
fn final_balances(
    g: &GrainDelta,
    stored: Option<&PostedMoney>,
    functional: Option<&PostedMoney>,
) -> Result<(PostedMoney, Option<PostedMoney>), ProjectError> {
    let tax = g.table_rank == GrainTable::Tax;
    if let Some(stored) = stored {
        match_currency(&g.currency_spec, stored.currency())?;
    }
    if stored.is_some() && !tax {
        match_functional(
            g.functional_currency.as_ref(),
            functional.map(PostedMoney::currency),
        )?;
    }
    let balance = final_amount(&g.delta, stored, &g.currency_spec)?;
    if (g.account_class.is_guarded() || g.table_rank == GrainTable::ReusableCredit)
        && balance.amount() < Decimal::ZERO
    {
        return Err(ProjectError::NegativeBalance {
            account_id: g.account_id,
            balance: encode_amount(&balance),
        });
    }
    let functional = if tax {
        None
    } else {
        g.functional_currency
            .as_ref()
            .map(|c| final_amount(&g.functional_delta, functional, c))
            .transpose()?
    };
    Ok((balance, functional))
}

/// Derive signed terms and coalesce by actual business identity in deterministic order.
fn derive_grains(
    entry: &NewEntry,
    lines: &[NewLine],
    normal_sides: &HashMap<Uuid, Side>,
) -> Result<Vec<GrainDelta>, ProjectError> {
    let mut grains = Vec::new();
    for line in lines {
        let normal_side = *normal_sides
            .get(&line.account_id)
            .ok_or(ProjectError::MissingNormalSide(line.account_id))?;
        let signed = |money: &PostedMoney| {
            ExactAmount::from_decimal(if line.side == normal_side {
                money.amount()
            } else {
                -money.amount()
            })
        };
        let base = GrainDelta {
            table_rank: GrainTable::Account,
            tenant_id: entry.tenant_id,
            account_id: line.account_id,
            currency: line.money.currency().code().to_owned(),
            payer_tenant_id: line.payer_tenant_id,
            invoice_id: String::new(),
            tax_jurisdiction: String::new(),
            tax_filing_period: String::new(),
            account_class: line.account_class,
            normal_side,
            delta: signed(&line.money),
            currency_spec: line.money.currency().clone(),
            functional_delta: line
                .functional_money
                .as_ref()
                .map_or_else(|| ExactAmount::from_decimal(Decimal::ZERO), signed),
            functional_currency: line.functional_money.as_ref().map(|m| m.currency().clone()),
            disputed_delta: ExactAmount::from_decimal(Decimal::ZERO),
            posted_at: entry.posted_at_utc,
            due_date: None,
            credit_grant_event_type: String::new(),
            first_granted_at: None,
        };
        grains.push(base.clone());
        match line.account_class {
            AccountClass::Ar => {
                grains.push(GrainDelta {
                    table_rank: GrainTable::ArPayer,
                    ..base.clone()
                });
                if let Some(invoice_id) = &line.invoice_id {
                    grains.push(GrainDelta {
                        table_rank: GrainTable::ArInvoice,
                        invoice_id: invoice_id.clone(),
                        disputed_delta: if line.ar_status.as_deref() == Some(AR_STATUS_DISPUTED) {
                            base.delta.clone()
                        } else {
                            ExactAmount::from_decimal(Decimal::ZERO)
                        },
                        due_date: line.due_date,
                        ..base
                    });
                }
            }
            AccountClass::Unallocated => grains.push(GrainDelta {
                table_rank: GrainTable::Unallocated,
                ..base
            }),
            AccountClass::ReusableCredit => {
                let event = line
                    .credit_grant_event_type
                    .clone()
                    .filter(|s| !s.is_empty())
                    .ok_or(ProjectError::MissingCreditEventType(line.line_id))?;
                grains.push(GrainDelta {
                    table_rank: GrainTable::ReusableCredit,
                    credit_grant_event_type: event,
                    first_granted_at: Some(entry.posted_at_utc),
                    ..base
                });
            }
            AccountClass::TaxPayable => {
                if let (Some(j), Some(f)) = (&line.tax_jurisdiction, &line.tax_filing_period) {
                    grains.push(GrainDelta {
                        table_rank: GrainTable::Tax,
                        tax_jurisdiction: j.clone(),
                        tax_filing_period: f.clone(),
                        ..base
                    });
                }
            }
            _ => {}
        }
    }
    grains.sort_by(|a, b| a.sort_key().cmp(&b.sort_key()));
    let mut result: Vec<GrainDelta> = Vec::new();
    for g in grains {
        if let Some(last) = result.last_mut()
            && last.sort_key() == g.sort_key()
        {
            match_currency(&last.currency_spec, &g.currency_spec)?;
            match_functional(
                last.functional_currency.as_ref(),
                g.functional_currency.as_ref(),
            )?;
            if last.account_id != g.account_id
                || last.account_class != g.account_class
                || last.normal_side != g.normal_side
            {
                return Err(ProjectError::Db(format!(
                    "projector: entry lines disagree on account for one grain ({}): \
                     account {} {} {} vs account {} {} {}",
                    grain_label(&g),
                    last.account_id,
                    last.account_class.as_str(),
                    last.normal_side.as_str(),
                    g.account_id,
                    g.account_class.as_str(),
                    g.normal_side.as_str(),
                )));
            }
            last.delta = last.delta.checked_add(&g.delta)?;
            last.disputed_delta = last.disputed_delta.checked_add(&g.disputed_delta)?;
            last.functional_delta = last.functional_delta.checked_add(&g.functional_delta)?;
        } else {
            result.push(g);
        }
    }
    Ok(result)
}

#[cfg(test)]
#[path = "projector_decimal_tests.rs"]
mod projector_decimal_tests;

#[cfg(test)]
#[path = "projector_tests.rs"]
mod projector_tests;
