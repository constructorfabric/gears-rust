//! Scoped payment candidate, carry and revaluation reads using immutable stored money.
//! Every read has a caller-runner variant; no repository method owns a retry budget.

#[path = "payment_repo/counters.rs"]
mod counters;
pub use counters::{
    AllocationRefundState, NewAllocationRow, PaymentRepo, SettlementCounter, SettlementState,
    StoredAllocation,
};

use crate::domain::exact_money::{ExactAmount, ExactError};
use crate::domain::model::RepoError;
use crate::domain::payment::precedence::PrecedenceStrategy;
use crate::infra::posting::idempotency::STATUS_POSTED;
use crate::infra::posting::retry::{db_to_repo, scope_to_repo};
use crate::infra::storage::entity::{
    account_balance, ar_invoice_balance, idempotency_dedup, reusable_credit_subbalance,
    tenant_precedence_policy, unallocated_balance,
};
use crate::infra::storage::money_text::{decode_money, decode_optional_money};
use bss_ledger_sdk::{PostedMoney, SourceDocType};
use rust_decimal::Decimal;
use sea_orm::{ColumnTrait, Condition, EntityTrait, Order};
use time::OffsetDateTime;
use toolkit_db::secure::{AccessScope, DBRunner, SecureEntityExt};
use uuid::Uuid;

/// An actual AR row, retaining its complete key, observed version and stored money.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpenArInvoice {
    pub tenant_id: Uuid,
    pub payer_tenant_id: Uuid,
    pub account_id: Uuid,
    pub invoice_id: String,
    pub version: i64,
    pub balance: PostedMoney,
    pub disputed: PostedMoney,
    pub functional_balance: Option<PostedMoney>,
    pub original_posted_at: Option<OffsetDateTime>,
}

/// Actual wallet subgrain. Account is immutable resource metadata, not a key axis.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CreditSubgrainView {
    pub tenant_id: Uuid,
    pub payer_tenant_id: Uuid,
    pub account_id: Uuid,
    pub credit_grant_event_type: String,
    pub first_granted_at: Option<OffsetDateTime>,
    pub version: i64,
    pub available: PostedMoney,
    pub functional_balance: Option<PostedMoney>,
}

/// The unique (tenant, payer, currency) pool and its immutable account metadata.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnallocatedCarried {
    pub tenant_id: Uuid,
    pub payer_tenant_id: Uuid,
    pub account_id: Uuid,
    pub version: i64,
    pub balance: PostedMoney,
    pub functional_balance: Option<PostedMoney>,
}

/// One actual (tenant, account, currency) row; absence is returned separately.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CarriedBalance {
    pub tenant_id: Uuid,
    pub account_id: Uuid,
    pub version: i64,
    pub balance: PostedMoney,
    pub functional_balance: Option<PostedMoney>,
}

/// Exact invoice total across accounts, with every contributing row and its version.
/// There is deliberately no aggregate version or arbitrarily selected account.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ArInvoiceCarried {
    pub balance: PostedMoney,
    pub functional_balance: Option<PostedMoney>,
    pub rows: Vec<OpenArInvoice>,
}

/// One positive cross-currency row to remeasure, with actual identity and version.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RevaluationGrain {
    pub tenant_id: Uuid,
    pub payer_tenant_id: Uuid,
    pub account_id: Uuid,
    pub invoice_id: Option<String>,
    pub credit_grant_event_type: Option<String>,
    pub version: i64,
    pub balance: PostedMoney,
    pub functional_balance: PostedMoney,
}

/// Reject invalid persisted row versions before publishing financial state.
fn version(value: i64) -> Result<i64, RepoError> {
    if value < 0 {
        return Err(RepoError::InvalidStoredMoney(
            "negative balance version".into(),
        ));
    }
    Ok(value)
}

/// Decode the full AR state before any monetary filtering.
fn invoice(m: ar_invoice_balance::Model) -> Result<OpenArInvoice, RepoError> {
    let balance = decode_money(&m.balance, &m.currency, m.currency_scale)?;
    let disputed = decode_money(&m.disputed, &m.currency, m.currency_scale)?;
    if balance.amount() < Decimal::ZERO
        || disputed.amount() < Decimal::ZERO
        || disputed.amount() > balance.amount()
    {
        return Err(RepoError::InvalidStoredMoney(
            "invalid stored AR caps".into(),
        ));
    }
    Ok(OpenArInvoice {
        tenant_id: m.tenant_id,
        payer_tenant_id: m.payer_tenant_id,
        account_id: m.account_id,
        invoice_id: m.invoice_id,
        version: version(m.version)?,
        balance,
        disputed,
        functional_balance: decode_optional_money(
            m.functional_balance.as_deref(),
            m.functional_currency.as_deref(),
            m.functional_currency_scale,
        )?,
        original_posted_at: m.original_posted_at,
    })
}

/// Decode the unique pool without inventing an account key dimension.
fn unallocated(m: unallocated_balance::Model) -> Result<UnallocatedCarried, RepoError> {
    let balance = decode_money(&m.balance, &m.currency, m.currency_scale)?;
    nonnegative(&balance)?;
    Ok(UnallocatedCarried {
        tenant_id: m.tenant_id,
        payer_tenant_id: m.payer_tenant_id,
        account_id: m.account_id,
        version: version(m.version)?,
        balance,
        functional_balance: decode_optional_money(
            m.functional_balance.as_deref(),
            m.functional_currency.as_deref(),
            m.functional_currency_scale,
        )?,
    })
}

/// Decode a credit row while preserving grant precedence metadata.
fn credit(m: reusable_credit_subbalance::Model) -> Result<CreditSubgrainView, RepoError> {
    let available = decode_money(&m.balance, &m.currency, m.currency_scale)?;
    nonnegative(&available)?;
    Ok(CreditSubgrainView {
        tenant_id: m.tenant_id,
        payer_tenant_id: m.payer_tenant_id,
        account_id: m.account_id,
        credit_grant_event_type: m.credit_grant_event_type,
        first_granted_at: m.first_granted_at,
        version: version(m.version)?,
        available,
        functional_balance: decode_optional_money(
            m.functional_balance.as_deref(),
            m.functional_currency.as_deref(),
            m.functional_currency_scale,
        )?,
    })
}

/// Preserve the cache's existing nonnegative transaction policy.
fn nonnegative(money: &PostedMoney) -> Result<(), RepoError> {
    if money.amount() < Decimal::ZERO {
        return Err(RepoError::InvalidStoredMoney(
            "negative guarded balance".into(),
        ));
    }
    Ok(())
}

/// Preserve final bounded money errors while treating exact resource failures as internal.
fn exact_error(error: ExactError) -> RepoError {
    match error {
        ExactError::Money(e) => RepoError::Money(e),
        e => RepoError::Db(format!("exact carried total: {e}")),
    }
}

/// Sum all actual rows before narrowing either total, validating every stored spec.
fn invoice_total(rows: Vec<OpenArInvoice>) -> Result<Option<ArInvoiceCarried>, RepoError> {
    let Some(first) = rows.first() else {
        return Ok(None);
    };
    let currency = first.balance.currency().clone();
    let functional_currency = first
        .functional_balance
        .as_ref()
        .map(|v| v.currency().clone());
    let mut balance = ExactAmount::from_decimal(Decimal::ZERO);
    let mut functional = ExactAmount::from_decimal(Decimal::ZERO);
    for row in &rows {
        if row.balance.currency() != &currency
            || row
                .functional_balance
                .as_ref()
                .map(bss_ledger_sdk::PostedMoney::currency)
                != functional_currency.as_ref()
        {
            return Err(RepoError::InvalidStoredMoney(
                "inconsistent stored invoice total metadata".into(),
            ));
        }
        balance = balance
            .checked_add(&ExactAmount::from_decimal(row.balance.amount()))
            .map_err(exact_error)?;
        if let Some(value) = &row.functional_balance {
            functional = functional
                .checked_add(&ExactAmount::from_decimal(value.amount()))
                .map_err(exact_error)?;
        }
    }
    Ok(Some(ArInvoiceCarried {
        balance: balance.into_posted_exact(currency).map_err(exact_error)?,
        functional_balance: functional_currency
            .map(|c| functional.into_posted_exact(c).map_err(exact_error))
            .transpose()?,
        rows,
    }))
}

impl PaymentRepo {
    /// Scoped standalone read; stored corruption and infrastructure errors are preserved.
    ///
    /// # Errors
    /// [`RepoError::Db`] when no connection can be acquired or on a scope / storage failure;
    /// [`RepoError::Conflict`] on classified database contention.
    /// [`RepoError::InvalidStoredMoney`] when a stored amount is malformed or off its currency
    /// contract.
    pub async fn list_open_ar_invoices(
        &self,
        scope: &AccessScope,
        tenant: Uuid,
        payer: Uuid,
        currency: &str,
    ) -> Result<Vec<OpenArInvoice>, RepoError> {
        let conn = self
            .db
            .conn()
            .map_err(|e| db_to_repo(e, self.db.db().backend()))?;
        self.list_open_ar_invoices_in(&conn, scope, tenant, payer, currency)
            .await
    }
    /// Read on the caller's attempt runner. No independent retry or connection.
    ///
    /// # Errors
    /// [`RepoError::Db`] on a scope or storage failure; [`RepoError::Conflict`] on classified
    /// database contention. [`RepoError::InvalidStoredMoney`] when a stored amount is malformed
    /// or off its currency contract.
    pub async fn list_open_ar_invoices_in<R: DBRunner>(
        &self,
        runner: &R,
        scope: &AccessScope,
        tenant: Uuid,
        payer: Uuid,
        currency: &str,
    ) -> Result<Vec<OpenArInvoice>, RepoError> {
        let rows = ar_invoice_balance::Entity::find()
            .secure()
            .scope_with(scope)
            .filter(
                Condition::all()
                    .add(ar_invoice_balance::Column::TenantId.eq(tenant))
                    .add(ar_invoice_balance::Column::PayerTenantId.eq(payer))
                    .add(ar_invoice_balance::Column::Currency.eq(currency)),
            )
            .order_by(ar_invoice_balance::Column::OriginalPostedAt, Order::Asc)
            .order_by(ar_invoice_balance::Column::InvoiceId, Order::Asc)
            .all(runner)
            .await
            .map_err(|e| scope_to_repo(e, self.db.db().backend()))?;
        let rows = rows
            .into_iter()
            .map(invoice)
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows
            .into_iter()
            .filter(|r| r.balance.amount() > Decimal::ZERO)
            .collect())
    }
    /// Scoped standalone read; stored corruption and infrastructure errors are preserved.
    ///
    /// # Errors
    /// [`RepoError::Db`] when no connection can be acquired or on a scope / storage failure;
    /// [`RepoError::Conflict`] on classified database contention.
    /// [`RepoError::InvalidStoredMoney`] when a stored amount is malformed or off its currency
    /// contract.
    pub async fn list_credit_subgrains(
        &self,
        scope: &AccessScope,
        tenant: Uuid,
        payer: Uuid,
        currency: &str,
    ) -> Result<Vec<CreditSubgrainView>, RepoError> {
        let conn = self
            .db
            .conn()
            .map_err(|e| db_to_repo(e, self.db.db().backend()))?;
        self.list_credit_subgrains_in(&conn, scope, tenant, payer, currency)
            .await
    }
    /// Read on the caller's attempt runner. No independent retry or connection.
    ///
    /// # Errors
    /// [`RepoError::Db`] on a scope or storage failure; [`RepoError::Conflict`] on classified
    /// database contention. [`RepoError::InvalidStoredMoney`] when a stored amount is malformed
    /// or off its currency contract.
    pub async fn list_credit_subgrains_in<R: DBRunner>(
        &self,
        runner: &R,
        scope: &AccessScope,
        tenant: Uuid,
        payer: Uuid,
        currency: &str,
    ) -> Result<Vec<CreditSubgrainView>, RepoError> {
        let rows = reusable_credit_subbalance::Entity::find()
            .secure()
            .scope_with(scope)
            .filter(
                Condition::all()
                    .add(reusable_credit_subbalance::Column::TenantId.eq(tenant))
                    .add(reusable_credit_subbalance::Column::PayerTenantId.eq(payer))
                    .add(reusable_credit_subbalance::Column::Currency.eq(currency)),
            )
            .order_by(
                reusable_credit_subbalance::Column::FirstGrantedAt,
                Order::Asc,
            )
            .order_by(
                reusable_credit_subbalance::Column::CreditGrantEventType,
                Order::Asc,
            )
            .all(runner)
            .await
            .map_err(|e| scope_to_repo(e, self.db.db().backend()))?;
        let rows = rows
            .into_iter()
            .map(credit)
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows
            .into_iter()
            .filter(|r| r.available.amount() > Decimal::ZERO)
            .collect())
    }
    /// Scoped standalone read; stored corruption and infrastructure errors are preserved.
    ///
    /// # Errors
    /// [`RepoError::Db`] when no connection can be acquired or on a scope / storage failure;
    /// [`RepoError::Conflict`] on classified database contention.
    /// [`RepoError::InvalidStoredMoney`] when a stored amount is malformed or off its currency
    /// contract.
    pub async fn read_unallocated(
        &self,
        scope: &AccessScope,
        tenant: Uuid,
        payer: Uuid,
        currency: &str,
    ) -> Result<Option<PostedMoney>, RepoError> {
        let conn = self
            .db
            .conn()
            .map_err(|e| db_to_repo(e, self.db.db().backend()))?;
        self.read_unallocated_in(&conn, scope, tenant, payer, currency)
            .await
    }
    /// Read on the caller's attempt runner. No independent retry or connection.
    ///
    /// # Errors
    /// [`RepoError::Db`] on a scope or storage failure; [`RepoError::Conflict`] on classified
    /// database contention. [`RepoError::InvalidStoredMoney`] when a stored amount is malformed
    /// or off its currency contract.
    pub async fn read_unallocated_in<R: DBRunner>(
        &self,
        runner: &R,
        scope: &AccessScope,
        tenant: Uuid,
        payer: Uuid,
        currency: &str,
    ) -> Result<Option<PostedMoney>, RepoError> {
        Ok(self
            .read_unallocated_carried_in(runner, scope, tenant, payer, currency)
            .await?
            .map(|r| r.balance))
    }
    /// Scoped standalone read; stored corruption and infrastructure errors are preserved.
    ///
    /// # Errors
    /// [`RepoError::Db`] when no connection can be acquired or on a scope / storage failure;
    /// [`RepoError::Conflict`] on classified database contention.
    /// [`RepoError::InvalidStoredMoney`] when a stored amount is malformed or off its currency
    /// contract.
    pub async fn read_unallocated_carried(
        &self,
        scope: &AccessScope,
        tenant: Uuid,
        payer: Uuid,
        currency: &str,
    ) -> Result<Option<UnallocatedCarried>, RepoError> {
        let conn = self
            .db
            .conn()
            .map_err(|e| db_to_repo(e, self.db.db().backend()))?;
        self.read_unallocated_carried_in(&conn, scope, tenant, payer, currency)
            .await
    }
    /// Read on the caller's attempt runner. No independent retry or connection.
    ///
    /// # Errors
    /// [`RepoError::Db`] on a scope or storage failure; [`RepoError::Conflict`] on classified
    /// database contention. [`RepoError::InvalidStoredMoney`] when a stored amount is malformed
    /// or off its currency contract.
    pub async fn read_unallocated_carried_in<R: DBRunner>(
        &self,
        runner: &R,
        scope: &AccessScope,
        tenant: Uuid,
        payer: Uuid,
        currency: &str,
    ) -> Result<Option<UnallocatedCarried>, RepoError> {
        let rows = unallocated_balance::Entity::find()
            .secure()
            .scope_with(scope)
            .filter(
                Condition::all()
                    .add(unallocated_balance::Column::TenantId.eq(tenant))
                    .add(unallocated_balance::Column::PayerTenantId.eq(payer))
                    .add(unallocated_balance::Column::Currency.eq(currency)),
            )
            .one(runner)
            .await
            .map_err(|e| scope_to_repo(e, self.db.db().backend()))?;
        rows.map(unallocated).transpose()
    }
    /// Scoped standalone read; stored corruption and infrastructure errors are preserved.
    ///
    /// # Errors
    /// [`RepoError::Db`] when no connection can be acquired or on a scope / storage failure;
    /// [`RepoError::Conflict`] on classified database contention.
    /// [`RepoError::InvalidStoredMoney`] when a stored amount is malformed or off its currency
    /// contract.
    pub async fn read_account_carried(
        &self,
        scope: &AccessScope,
        tenant: Uuid,
        account_id: Uuid,
        currency: &str,
    ) -> Result<Option<CarriedBalance>, RepoError> {
        let conn = self
            .db
            .conn()
            .map_err(|e| db_to_repo(e, self.db.db().backend()))?;
        self.read_account_carried_in(&conn, scope, tenant, account_id, currency)
            .await
    }
    /// Read on the caller's attempt runner. No independent retry or connection.
    ///
    /// # Errors
    /// [`RepoError::Db`] on a scope or storage failure; [`RepoError::Conflict`] on classified
    /// database contention. [`RepoError::InvalidStoredMoney`] when a stored amount is malformed
    /// or off its currency contract.
    pub async fn read_account_carried_in<R: DBRunner>(
        &self,
        runner: &R,
        scope: &AccessScope,
        tenant: Uuid,
        account_id: Uuid,
        currency: &str,
    ) -> Result<Option<CarriedBalance>, RepoError> {
        let rows = account_balance::Entity::find()
            .secure()
            .scope_with(scope)
            .filter(
                Condition::all()
                    .add(account_balance::Column::TenantId.eq(tenant))
                    .add(account_balance::Column::AccountId.eq(account_id))
                    .add(account_balance::Column::Currency.eq(currency)),
            )
            .one(runner)
            .await
            .map_err(|e| scope_to_repo(e, self.db.db().backend()))?;
        rows.map(|m| {
            let (balance, functional_balance) = super::journal_repo::balance_money(&m)?;
            Ok(CarriedBalance {
                tenant_id: m.tenant_id,
                account_id: m.account_id,
                version: version(m.version)?,
                balance,
                functional_balance,
            })
        })
        .transpose()
    }
    /// Scoped standalone read; stored corruption and infrastructure errors are preserved.
    ///
    /// # Errors
    /// [`RepoError::Db`] when no connection can be acquired or on a scope / storage failure;
    /// [`RepoError::Conflict`] on classified database contention.
    /// [`RepoError::InvalidStoredMoney`] when a stored amount is malformed or off its currency
    /// contract.
    pub async fn read_ar_invoice_carried(
        &self,
        scope: &AccessScope,
        tenant: Uuid,
        payer: Uuid,
        invoice_id: &str,
        currency: &str,
    ) -> Result<Option<ArInvoiceCarried>, RepoError> {
        let conn = self
            .db
            .conn()
            .map_err(|e| db_to_repo(e, self.db.db().backend()))?;
        self.read_ar_invoice_carried_in(&conn, scope, tenant, payer, invoice_id, currency)
            .await
    }
    /// Read on the caller's attempt runner. No independent retry or connection.
    ///
    /// # Errors
    /// [`RepoError::Db`] on a scope or storage failure; [`RepoError::Conflict`] on classified
    /// database contention. [`RepoError::InvalidStoredMoney`] when a stored amount is malformed
    /// or off its currency contract.
    pub async fn read_ar_invoice_carried_in<R: DBRunner>(
        &self,
        runner: &R,
        scope: &AccessScope,
        tenant: Uuid,
        payer: Uuid,
        invoice_id: &str,
        currency: &str,
    ) -> Result<Option<ArInvoiceCarried>, RepoError> {
        let rows = ar_invoice_balance::Entity::find()
            .secure()
            .scope_with(scope)
            .filter(
                Condition::all()
                    .add(ar_invoice_balance::Column::TenantId.eq(tenant))
                    .add(ar_invoice_balance::Column::PayerTenantId.eq(payer))
                    .add(ar_invoice_balance::Column::Currency.eq(currency))
                    .add(ar_invoice_balance::Column::InvoiceId.eq(invoice_id)),
            )
            .order_by(ar_invoice_balance::Column::AccountId, Order::Asc)
            .all(runner)
            .await
            .map_err(|e| scope_to_repo(e, self.db.db().backend()))?;
        invoice_total(
            rows.into_iter()
                .map(invoice)
                .collect::<Result<Vec<_>, _>>()?,
        )
    }
    /// Scoped standalone read; stored corruption and infrastructure errors are preserved.
    ///
    /// # Errors
    /// [`RepoError::Db`] when no connection can be acquired or on a scope / storage failure;
    /// [`RepoError::Conflict`] on classified database contention.
    /// [`RepoError::InvalidStoredMoney`] when a stored amount is malformed or off its currency
    /// contract.
    pub async fn list_ar_invoices_to_revalue(
        &self,
        scope: &AccessScope,
        tenant: Uuid,
    ) -> Result<Vec<RevaluationGrain>, RepoError> {
        let conn = self
            .db
            .conn()
            .map_err(|e| db_to_repo(e, self.db.db().backend()))?;
        self.list_ar_invoices_to_revalue_in(&conn, scope, tenant)
            .await
    }
    /// Read on the caller's attempt runner. No independent retry or connection.
    ///
    /// # Errors
    /// [`RepoError::Db`] on a scope or storage failure; [`RepoError::Conflict`] on classified
    /// database contention. [`RepoError::InvalidStoredMoney`] when a stored amount is malformed
    /// or off its currency contract.
    pub async fn list_ar_invoices_to_revalue_in<R: DBRunner>(
        &self,
        runner: &R,
        scope: &AccessScope,
        tenant: Uuid,
    ) -> Result<Vec<RevaluationGrain>, RepoError> {
        let rows = ar_invoice_balance::Entity::find()
            .secure()
            .scope_with(scope)
            .filter(Condition::all().add(ar_invoice_balance::Column::TenantId.eq(tenant)))
            .order_by(ar_invoice_balance::Column::InvoiceId, Order::Asc)
            .all(runner)
            .await
            .map_err(|e| scope_to_repo(e, self.db.db().backend()))?;
        let rows = rows
            .into_iter()
            .map(invoice)
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows
            .into_iter()
            .filter(|r| r.balance.amount() > Decimal::ZERO)
            .filter_map(|r| {
                r.functional_balance
                    .map(|functional_balance| RevaluationGrain {
                        tenant_id: r.tenant_id,
                        payer_tenant_id: r.payer_tenant_id,
                        account_id: r.account_id,
                        invoice_id: Some(r.invoice_id),
                        credit_grant_event_type: None,
                        version: r.version,
                        balance: r.balance,
                        functional_balance,
                    })
            })
            .collect())
    }
    /// Scoped standalone read; stored corruption and infrastructure errors are preserved.
    ///
    /// # Errors
    /// [`RepoError::Db`] when no connection can be acquired or on a scope / storage failure;
    /// [`RepoError::Conflict`] on classified database contention.
    /// [`RepoError::InvalidStoredMoney`] when a stored amount is malformed or off its currency
    /// contract.
    pub async fn list_unallocated_to_revalue(
        &self,
        scope: &AccessScope,
        tenant: Uuid,
    ) -> Result<Vec<RevaluationGrain>, RepoError> {
        let conn = self
            .db
            .conn()
            .map_err(|e| db_to_repo(e, self.db.db().backend()))?;
        self.list_unallocated_to_revalue_in(&conn, scope, tenant)
            .await
    }
    /// Read on the caller's attempt runner. No independent retry or connection.
    ///
    /// # Errors
    /// [`RepoError::Db`] on a scope or storage failure; [`RepoError::Conflict`] on classified
    /// database contention. [`RepoError::InvalidStoredMoney`] when a stored amount is malformed
    /// or off its currency contract.
    pub async fn list_unallocated_to_revalue_in<R: DBRunner>(
        &self,
        runner: &R,
        scope: &AccessScope,
        tenant: Uuid,
    ) -> Result<Vec<RevaluationGrain>, RepoError> {
        let rows = unallocated_balance::Entity::find()
            .secure()
            .scope_with(scope)
            .filter(Condition::all().add(unallocated_balance::Column::TenantId.eq(tenant)))
            .order_by(unallocated_balance::Column::PayerTenantId, Order::Asc)
            .order_by(unallocated_balance::Column::Currency, Order::Asc)
            .all(runner)
            .await
            .map_err(|e| scope_to_repo(e, self.db.db().backend()))?;
        let rows = rows
            .into_iter()
            .map(unallocated)
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows
            .into_iter()
            .filter(|r| r.balance.amount() > Decimal::ZERO)
            .filter_map(|r| {
                r.functional_balance
                    .map(|functional_balance| RevaluationGrain {
                        tenant_id: r.tenant_id,
                        payer_tenant_id: r.payer_tenant_id,
                        account_id: r.account_id,
                        invoice_id: None,
                        credit_grant_event_type: None,
                        version: r.version,
                        balance: r.balance,
                        functional_balance,
                    })
            })
            .collect())
    }
    /// Scoped standalone read; stored corruption and infrastructure errors are preserved.
    ///
    /// # Errors
    /// [`RepoError::Db`] when no connection can be acquired or on a scope / storage failure;
    /// [`RepoError::Conflict`] on classified database contention.
    /// [`RepoError::InvalidStoredMoney`] when a stored amount is malformed or off its currency
    /// contract.
    pub async fn list_reusable_credit_to_revalue(
        &self,
        scope: &AccessScope,
        tenant: Uuid,
    ) -> Result<Vec<RevaluationGrain>, RepoError> {
        let conn = self
            .db
            .conn()
            .map_err(|e| db_to_repo(e, self.db.db().backend()))?;
        self.list_reusable_credit_to_revalue_in(&conn, scope, tenant)
            .await
    }
    /// Read on the caller's attempt runner. No independent retry or connection.
    ///
    /// # Errors
    /// [`RepoError::Db`] on a scope or storage failure; [`RepoError::Conflict`] on classified
    /// database contention. [`RepoError::InvalidStoredMoney`] when a stored amount is malformed
    /// or off its currency contract.
    pub async fn list_reusable_credit_to_revalue_in<R: DBRunner>(
        &self,
        runner: &R,
        scope: &AccessScope,
        tenant: Uuid,
    ) -> Result<Vec<RevaluationGrain>, RepoError> {
        let rows = reusable_credit_subbalance::Entity::find()
            .secure()
            .scope_with(scope)
            .filter(Condition::all().add(reusable_credit_subbalance::Column::TenantId.eq(tenant)))
            .order_by(
                reusable_credit_subbalance::Column::PayerTenantId,
                Order::Asc,
            )
            .order_by(reusable_credit_subbalance::Column::Currency, Order::Asc)
            .order_by(
                reusable_credit_subbalance::Column::CreditGrantEventType,
                Order::Asc,
            )
            .all(runner)
            .await
            .map_err(|e| scope_to_repo(e, self.db.db().backend()))?;
        let rows = rows
            .into_iter()
            .map(credit)
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows
            .into_iter()
            .filter(|r| r.available.amount() > Decimal::ZERO)
            .filter_map(|r| {
                r.functional_balance
                    .map(|functional_balance| RevaluationGrain {
                        tenant_id: r.tenant_id,
                        payer_tenant_id: r.payer_tenant_id,
                        account_id: r.account_id,
                        invoice_id: None,
                        credit_grant_event_type: Some(r.credit_grant_event_type),
                        version: r.version,
                        balance: r.available,
                        functional_balance,
                    })
            })
            .collect())
    }
    /// Scoped standalone read; stored corruption and infrastructure errors are preserved.
    ///
    /// # Errors
    /// [`RepoError::Db`] when no connection can be acquired or on a scope / storage failure;
    /// [`RepoError::Conflict`] on classified database contention.
    pub async fn lookup_finalized_post(
        &self,
        scope: &AccessScope,
        tenant: Uuid,
        source_doc_type: SourceDocType,
        business_id: &str,
    ) -> Result<Option<(Uuid, String)>, RepoError> {
        let conn = self
            .db
            .conn()
            .map_err(|e| db_to_repo(e, self.db.db().backend()))?;
        self.lookup_finalized_post_in(&conn, scope, tenant, source_doc_type, business_id)
            .await
    }
    /// Read on the caller's attempt runner. No independent retry or connection.
    ///
    /// # Errors
    /// [`RepoError::Db`] on a scope or storage failure; [`RepoError::Conflict`] on classified
    /// database contention.
    pub async fn lookup_finalized_post_in<R: DBRunner>(
        &self,
        runner: &R,
        scope: &AccessScope,
        tenant: Uuid,
        source_doc_type: SourceDocType,
        business_id: &str,
    ) -> Result<Option<(Uuid, String)>, RepoError> {
        // Only a finalized (POSTED) row carries an authoritative prior entry id; a
        // still-CLAIMED row is an in-flight concurrent post (and a QUEUED row is a
        // pending deferred apply) — fall through and let the engine's in-txn claim
        // serialize against it. The prior entry id is paired with the stored
        // request `payload_hash` so the caller's replay short-circuit can reject a
        // same-key / different-payload reuse instead of replaying it.
        Ok(self
            .lookup_dedup_status_in(runner, scope, tenant, source_doc_type, business_id)
            .await?
            .and_then(|(status, entry_id, payload_hash)| {
                if status == STATUS_POSTED {
                    entry_id.map(|id| (id, payload_hash))
                } else {
                    None
                }
            }))
    }
    /// Scoped standalone read; stored corruption and infrastructure errors are preserved.
    ///
    /// # Errors
    /// [`RepoError::Db`] when no connection can be acquired or on a scope / storage failure;
    /// [`RepoError::Conflict`] on classified database contention.
    pub async fn lookup_dedup_status(
        &self,
        scope: &AccessScope,
        tenant: Uuid,
        source_doc_type: SourceDocType,
        business_id: &str,
    ) -> Result<Option<(String, Option<Uuid>, String)>, RepoError> {
        let conn = self
            .db
            .conn()
            .map_err(|e| db_to_repo(e, self.db.db().backend()))?;
        self.lookup_dedup_status_in(&conn, scope, tenant, source_doc_type, business_id)
            .await
    }
    /// Read on the caller's attempt runner. No independent retry or connection.
    ///
    /// # Errors
    /// [`RepoError::Db`] on a scope or storage failure; [`RepoError::Conflict`] on classified
    /// database contention.
    pub async fn lookup_dedup_status_in<R: DBRunner>(
        &self,
        runner: &R,
        scope: &AccessScope,
        tenant: Uuid,
        source_doc_type: SourceDocType,
        business_id: &str,
    ) -> Result<Option<(String, Option<Uuid>, String)>, RepoError> {
        let row = idempotency_dedup::Entity::find()
            .secure()
            .scope_with(scope)
            .filter(
                Condition::all()
                    .add(idempotency_dedup::Column::TenantId.eq(tenant))
                    .add(idempotency_dedup::Column::Flow.eq(source_doc_type.as_str()))
                    .add(idempotency_dedup::Column::BusinessId.eq(business_id)),
            )
            .one(runner)
            .await
            .map_err(|e| scope_to_repo(e, self.db.db().backend()))?;
        Ok(row.map(|r| (r.status, r.result_entry_id, r.payload_hash)))
    }
    /// Scoped standalone read; stored corruption and infrastructure errors are preserved.
    ///
    /// # Errors
    /// [`RepoError::Db`] when no connection can be acquired, on a scope / storage failure, or
    /// when a stored `strategy` is not a known policy id (an invariant breach — the column is
    /// only ever written from [`PrecedenceStrategy::policy_ref`]); [`RepoError::Conflict`] on
    /// classified database contention.
    pub async fn read_effective_policy(
        &self,
        scope: &AccessScope,
        tenant: Uuid,
        at: OffsetDateTime,
    ) -> Result<Option<(PrecedenceStrategy, i64)>, RepoError> {
        let conn = self
            .db
            .conn()
            .map_err(|e| db_to_repo(e, self.db.db().backend()))?;
        self.read_effective_policy_in(&conn, scope, tenant, at)
            .await
    }
    /// Read on the caller's attempt runner. No independent retry or connection.
    ///
    /// # Errors
    /// [`RepoError::Db`] on a scope / storage failure, or when a stored `strategy` is not a
    /// known policy id (an invariant breach — the column is only ever written from
    /// [`PrecedenceStrategy::policy_ref`]); [`RepoError::Conflict`] on classified database
    /// contention.
    pub async fn read_effective_policy_in<R: DBRunner>(
        &self,
        runner: &R,
        scope: &AccessScope,
        tenant: Uuid,
        at: OffsetDateTime,
    ) -> Result<Option<(PrecedenceStrategy, i64)>, RepoError> {
        let row = tenant_precedence_policy::Entity::find()
            .secure()
            .scope_with(scope)
            .filter(
                Condition::all()
                    .add(tenant_precedence_policy::Column::TenantId.eq(tenant))
                    .add(tenant_precedence_policy::Column::EffectiveFrom.lte(at)),
            )
            .order_by(tenant_precedence_policy::Column::EffectiveFrom, Order::Desc)
            .order_by(tenant_precedence_policy::Column::Version, Order::Desc)
            .one(runner)
            .await
            .map_err(|e| scope_to_repo(e, self.db.db().backend()))?;
        let Some(row) = row else {
            return Ok(None);
        };
        let strategy = PrecedenceStrategy::parse(&row.strategy).ok_or_else(|| {
            RepoError::Db(format!(
                "unknown stored precedence strategy {:?} for tenant {tenant} version {}",
                row.strategy, row.version
            ))
        })?;
        Ok(Some((strategy, row.version)))
    }
}

#[cfg(test)]
#[path = "payment_repo/query_tests.rs"]
mod query_tests;
