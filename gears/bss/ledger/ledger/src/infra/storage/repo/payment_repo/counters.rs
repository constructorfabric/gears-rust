//! Exact payment counters and append-only allocation rows on the caller's transaction.
//!
//! No method retries. Every error must escape the transaction so preceding journal,
//! counter and cache writes roll back together. Stored metadata is authoritative.

use bss_ledger_sdk::PostedMoney;
use rust_decimal::Decimal;
use sea_orm::sea_query::{Expr, LockType};
use sea_orm::{ActiveValue::Set, ColumnTrait, Condition, EntityTrait, Order, QuerySelect};
use time::OffsetDateTime;
use toolkit_db::secure::{
    AccessScope, DBRunner, DbTx, SecureEntityExt, SecureInsertExt, SecureUpdateExt,
};
use toolkit_db::{DBProvider, DbError};
use uuid::Uuid;

use crate::domain::exact_money::{ExactAmount, ExactError};
use crate::domain::model::RepoError;
use crate::infra::posting::retry::{db_to_repo, insert_to_repo, scope_to_repo};
use crate::infra::storage::entity::{
    payment_allocation, payment_allocation_refund, payment_settlement,
};
use crate::infra::storage::money_text::{decode_money, encode_amount};

/// Full validated settlement snapshot. Each counter retains stored currency and scale.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SettlementState {
    pub tenant_id: Uuid,
    pub payment_id: String,
    pub version: i64,
    pub settled: PostedMoney,
    pub fee: PostedMoney,
    pub allocated: PostedMoney,
    pub refunded: PostedMoney,
    pub refunded_unallocated: PostedMoney,
    pub clawed_back: PostedMoney,
}

/// Full validated invoice allocation/refund snapshot, or `None` when absent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AllocationRefundState {
    pub tenant_id: Uuid,
    pub payment_id: String,
    pub invoice_id: String,
    pub version: i64,
    pub allocated: PostedMoney,
    pub refunded: PostedMoney,
}

/// An immutable allocation split supplied by the caller.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NewAllocationRow {
    pub tenant_id: Uuid,
    pub allocation_id: Uuid,
    pub payer_tenant_id: Uuid,
    pub payment_id: String,
    pub invoice_id: String,
    pub amount: PostedMoney,
    pub precedence_policy_ref: String,
    pub allocated_at_utc: OffsetDateTime,
}

/// A decoded stored allocation split, including original audit fields. The
/// storage read model; `bss_ledger_sdk::AllocationView` is the public one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoredAllocation {
    pub tenant_id: Uuid,
    pub allocation_id: Uuid,
    pub payer_tenant_id: Uuid,
    pub payment_id: String,
    pub invoice_id: String,
    pub amount: PostedMoney,
    pub precedence_policy_ref: String,
    pub allocated_at_utc: OffsetDateTime,
}

/// SeaORM-backed payment repository. Financial writes require a caller transaction.
#[derive(Clone)]
pub struct PaymentRepo {
    pub(super) db: DBProvider<DbError>,
}

impl PaymentRepo {
    /// Construct a repository using the configured database and backend.
    #[must_use]
    pub fn new(db: DBProvider<DbError>) -> Self {
        Self { db }
    }

    /// Insert a fresh settlement; an unexpected duplicate is not a rebuildable grain race.
    /// All six nonnegative and correlated caps are checked before insertion.
    ///
    /// # Errors
    /// [`RepoError::Money`] when `settled` and `fee` disagree on currency metadata or a zero
    /// cannot be built at that spec; [`RepoError::MoneyOutCapExceeded`] when the seeded
    /// counters violate a settlement cap (a negative value, or a fee above the settled amount);
    /// [`RepoError::Db`] on a scope or storage failure, an unexpected duplicate key included;
    /// [`RepoError::Conflict`] on classified database contention.
    pub async fn seed_settlement(
        &self,
        txn: &DbTx<'_>,
        scope: &AccessScope,
        tenant: Uuid,
        payment_id: &str,
        settled: &PostedMoney,
        fee: &PostedMoney,
    ) -> Result<(), RepoError> {
        matching(settled, fee)?;
        let zero = PostedMoney::try_new(Decimal::ZERO, settled.currency().clone())?;
        let state = SettlementState {
            tenant_id: tenant,
            payment_id: payment_id.to_owned(),
            version: 0,
            settled: settled.clone(),
            fee: fee.clone(),
            allocated: zero.clone(),
            refunded: zero.clone(),
            refunded_unallocated: zero.clone(),
            clawed_back: zero,
        };
        validate_settlement(&state.exact())?;
        let am = payment_settlement::ActiveModel {
            tenant_id: Set(tenant),
            payment_id: Set(payment_id.to_owned()),
            currency: Set(settled.currency().code().to_owned()),
            currency_scale: Set(i16::from(settled.currency().scale())),
            settled: Set(encode_amount(settled)),
            fee: Set(encode_amount(fee)),
            allocated: Set("0".into()),
            refunded: Set("0".into()),
            refunded_unallocated: Set("0".into()),
            clawed_back: Set("0".into()),
            version: Set(0),
        };
        payment_settlement::Entity::insert(am.clone())
            .secure()
            .scope_with_model(scope, &am)
            .map_err(|e| scope_to_repo(e, self.db.db().backend()))?
            .exec(txn)
            .await
            .map_err(|e| scope_to_repo(e, self.db.db().backend()))?;
        Ok(())
    }

    /// Read all settlement counters on the caller's snapshot, using stored metadata.
    ///
    /// # Errors
    /// [`RepoError::Db`] on a scope or storage failure; [`RepoError::Conflict`] on classified
    /// database contention; [`RepoError::InvalidStoredMoney`] when a stored counter is
    /// malformed, the stored version is negative, or the stored counters violate their caps.
    pub async fn read_settlement_in<R: DBRunner>(
        &self,
        runner: &R,
        scope: &AccessScope,
        tenant: Uuid,
        payment_id: &str,
    ) -> Result<Option<SettlementState>, RepoError> {
        self.select_settlement(runner, scope, tenant, payment_id, false)
            .await
    }

    /// Standalone settlement read. Mutable decisions must use the caller-runner read.
    ///
    /// # Errors
    /// [`RepoError::Db`] when no connection can be acquired or on a scope / storage failure;
    /// [`RepoError::Conflict`] on classified database contention;
    /// [`RepoError::InvalidStoredMoney`] when a stored counter is malformed, the stored version
    /// is negative, or the stored counters violate their caps.
    pub async fn read_settlement(
        &self,
        scope: &AccessScope,
        tenant: Uuid,
        payment_id: &str,
    ) -> Result<Option<SettlementState>, RepoError> {
        let conn = self
            .db
            .conn()
            .map_err(|e| db_to_repo(e, self.db.db().backend()))?;
        self.read_settlement_in(&conn, scope, tenant, payment_id)
            .await
    }

    /// Lock the settlement before the rank-1 refund underflow decision on PostgreSQL.
    /// SQLite omits row locks; the final CAS is required on both databases.
    ///
    /// # Errors
    /// [`RepoError::Db`] on a scope or storage failure; [`RepoError::Conflict`] on classified
    /// database contention; [`RepoError::InvalidStoredMoney`] when a stored counter is
    /// malformed, the stored version is negative, or the stored counters violate their caps.
    pub async fn read_settlement_for_update(
        &self,
        txn: &DbTx<'_>,
        scope: &AccessScope,
        tenant: Uuid,
        payment_id: &str,
    ) -> Result<Option<SettlementState>, RepoError> {
        self.select_settlement(txn, scope, tenant, payment_id, true)
            .await
    }

    /// Shared secure select for standalone and transactional snapshots.
    async fn select_settlement<R: DBRunner>(
        &self,
        runner: &R,
        scope: &AccessScope,
        tenant: Uuid,
        payment_id: &str,
        lock: bool,
    ) -> Result<Option<SettlementState>, RepoError> {
        let mut find = payment_settlement::Entity::find();
        if lock && self.db.db().backend() == sea_orm::DbBackend::Postgres {
            find = find.lock(LockType::Update);
        }
        find.secure()
            .scope_with(scope)
            .filter(settlement_key(tenant, payment_id))
            .one(runner)
            .await
            .map_err(|e| scope_to_repo(e, self.db.db().backend()))?
            .map(SettlementState::decode)
            .transpose()
    }

    /// Apply one exact counter delta after validating every correlated post-state cap.
    async fn add_settlement_counter(
        &self,
        txn: &DbTx<'_>,
        scope: &AccessScope,
        tenant: Uuid,
        payment_id: &str,
        counter: SettlementCounter,
        delta: &PostedMoney,
    ) -> Result<(), RepoError> {
        self.add_settlement_deltas(txn, scope, tenant, payment_id, &[(counter, delta)])
            .await
    }

    /// Apply several exact counter deltas with one locked read and one CAS write.
    /// Every delta is checked against the stored metadata, the correlated caps are
    /// validated once on the combined post-state (so the order of the deltas never
    /// trips an intermediate cap), and only the changed columns are written. An
    /// empty list is a no-op.
    ///
    /// # Errors
    /// [`RepoError::Db`] when the payment has no settlement row, or on a scope / storage
    /// failure; [`RepoError::Money`] when a delta disagrees with the stored currency
    /// metadata or an exact result leaves the money contract;
    /// [`RepoError::MoneyOutCapExceeded`] when the combined post-state violates a
    /// settlement cap; [`RepoError::Conflict`] when the observed version is stale, or on
    /// classified database contention; [`RepoError::InvalidStoredMoney`] when the stored
    /// row is malformed.
    pub async fn add_settlement_deltas(
        &self,
        txn: &DbTx<'_>,
        scope: &AccessScope,
        tenant: Uuid,
        payment_id: &str,
        deltas: &[(SettlementCounter, &PostedMoney)],
    ) -> Result<(), RepoError> {
        if deltas.is_empty() {
            return Ok(());
        }
        let state = self
            .read_settlement_for_update(txn, scope, tenant, payment_id)
            .await?
            .ok_or_else(|| {
                RepoError::Db(format!(
                    "payment_settlement absent for ({tenant}, {payment_id}): payment not settled"
                ))
            })?;
        let mut values = state.exact();
        let mut changed: Vec<SettlementCounter> = Vec::with_capacity(deltas.len());
        for (counter, delta) in deltas {
            matching(&state.settled, delta)?;
            let slot = values.get_mut(*counter);
            *slot = slot.checked_add(&exact(delta)).map_err(exact_error)?;
            if !changed.contains(counter) {
                changed.push(*counter);
            }
        }
        validate_settlement(&values)?;
        let writes = changed
            .into_iter()
            .map(|counter| {
                values
                    .get(counter)
                    .clone()
                    .into_posted_exact(state.settled.currency().clone())
                    .map(|value| (counter, value))
                    .map_err(exact_error)
            })
            .collect::<Result<Vec<_>, _>>()?;
        self.write_settlement_counters(txn, scope, &state, &writes)
            .await
    }

    /// Single-column form of [`Self::write_settlement_counters`], kept for the CAS
    /// tests that drive a stale observed state directly.
    #[cfg(test)]
    async fn write_settlement_counter(
        &self,
        txn: &DbTx<'_>,
        scope: &AccessScope,
        state: &SettlementState,
        counter: SettlementCounter,
        value: &PostedMoney,
    ) -> Result<(), RepoError> {
        self.write_settlement_counters(txn, scope, state, &[(counter, value.clone())])
            .await
    }

    /// Write already-validated literals in one CAS guarded by the observed version.
    async fn write_settlement_counters(
        &self,
        txn: &DbTx<'_>,
        scope: &AccessScope,
        state: &SettlementState,
        writes: &[(SettlementCounter, PostedMoney)],
    ) -> Result<(), RepoError> {
        let version = next_version(state.version)?;
        let mut update = payment_settlement::Entity::update_many()
            .secure()
            .scope_with(scope)
            .col_expr(payment_settlement::Column::Version, Expr::value(version));
        for (counter, value) in writes {
            update = update.col_expr(counter.column(), Expr::value(encode_amount(value)));
        }
        let result = update
            .filter(
                settlement_key(state.tenant_id, &state.payment_id)
                    .add(payment_settlement::Column::Version.eq(state.version)),
            )
            .exec(txn)
            .await
            .map_err(|e| scope_to_repo(e, self.db.db().backend()))?;
        require_one(result.rows_affected)
    }

    /// Apply a signed `settled` delta, checking all settlement caps and observed version.
    ///
    /// # Errors
    /// [`RepoError::Db`] when the payment has no settlement row, or on a scope / storage
    /// failure; [`RepoError::Money`] when `delta` disagrees with the stored currency metadata
    /// or the exact result leaves the money contract; [`RepoError::MoneyOutCapExceeded`] when
    /// the post-state violates a settlement cap; [`RepoError::Conflict`] when the observed
    /// version is stale, or on classified database contention;
    /// [`RepoError::InvalidStoredMoney`] when the stored row is malformed.
    pub async fn add_settled(
        &self,
        txn: &DbTx<'_>,
        scope: &AccessScope,
        tenant: Uuid,
        payment_id: &str,
        delta: &PostedMoney,
    ) -> Result<(), RepoError> {
        self.add_settlement_counter(
            txn,
            scope,
            tenant,
            payment_id,
            SettlementCounter::Settled,
            delta,
        )
        .await
    }

    /// Apply a signed `fee` delta, checking all settlement caps and observed version.
    ///
    /// # Errors
    /// [`RepoError::Db`] when the payment has no settlement row, or on a scope / storage
    /// failure; [`RepoError::Money`] when `delta` disagrees with the stored currency metadata
    /// or the exact result leaves the money contract; [`RepoError::MoneyOutCapExceeded`] when
    /// the post-state violates a settlement cap; [`RepoError::Conflict`] when the observed
    /// version is stale, or on classified database contention;
    /// [`RepoError::InvalidStoredMoney`] when the stored row is malformed.
    pub async fn add_fee(
        &self,
        txn: &DbTx<'_>,
        scope: &AccessScope,
        tenant: Uuid,
        payment_id: &str,
        delta: &PostedMoney,
    ) -> Result<(), RepoError> {
        self.add_settlement_counter(
            txn,
            scope,
            tenant,
            payment_id,
            SettlementCounter::Fee,
            delta,
        )
        .await
    }

    /// Apply a signed `allocated` delta, checking all settlement caps and observed version.
    ///
    /// # Errors
    /// [`RepoError::Db`] when the payment has no settlement row, or on a scope / storage
    /// failure; [`RepoError::Money`] when `delta` disagrees with the stored currency metadata
    /// or the exact result leaves the money contract; [`RepoError::MoneyOutCapExceeded`] when
    /// the post-state violates a settlement cap; [`RepoError::Conflict`] when the observed
    /// version is stale, or on classified database contention;
    /// [`RepoError::InvalidStoredMoney`] when the stored row is malformed.
    pub async fn add_allocated(
        &self,
        txn: &DbTx<'_>,
        scope: &AccessScope,
        tenant: Uuid,
        payment_id: &str,
        delta: &PostedMoney,
    ) -> Result<(), RepoError> {
        self.add_settlement_counter(
            txn,
            scope,
            tenant,
            payment_id,
            SettlementCounter::Allocated,
            delta,
        )
        .await
    }

    /// Apply a signed `refunded` delta, checking all settlement caps and observed version.
    ///
    /// # Errors
    /// [`RepoError::Db`] when the payment has no settlement row, or on a scope / storage
    /// failure; [`RepoError::Money`] when `delta` disagrees with the stored currency metadata
    /// or the exact result leaves the money contract; [`RepoError::MoneyOutCapExceeded`] when
    /// the post-state violates a settlement cap; [`RepoError::Conflict`] when the observed
    /// version is stale, or on classified database contention;
    /// [`RepoError::InvalidStoredMoney`] when the stored row is malformed.
    pub async fn add_refunded(
        &self,
        txn: &DbTx<'_>,
        scope: &AccessScope,
        tenant: Uuid,
        payment_id: &str,
        delta: &PostedMoney,
    ) -> Result<(), RepoError> {
        self.add_settlement_counter(
            txn,
            scope,
            tenant,
            payment_id,
            SettlementCounter::Refunded,
            delta,
        )
        .await
    }

    /// Apply a signed `refunded_unallocated` delta, checking all settlement caps and observed version.
    ///
    /// # Errors
    /// [`RepoError::Db`] when the payment has no settlement row, or on a scope / storage
    /// failure; [`RepoError::Money`] when `delta` disagrees with the stored currency metadata
    /// or the exact result leaves the money contract; [`RepoError::MoneyOutCapExceeded`] when
    /// the post-state violates a settlement cap; [`RepoError::Conflict`] when the observed
    /// version is stale, or on classified database contention;
    /// [`RepoError::InvalidStoredMoney`] when the stored row is malformed.
    pub async fn add_refunded_unallocated(
        &self,
        txn: &DbTx<'_>,
        scope: &AccessScope,
        tenant: Uuid,
        payment_id: &str,
        delta: &PostedMoney,
    ) -> Result<(), RepoError> {
        self.add_settlement_counter(
            txn,
            scope,
            tenant,
            payment_id,
            SettlementCounter::RefundedUnallocated,
            delta,
        )
        .await
    }

    /// Apply a signed `clawed_back` delta, checking all settlement caps and observed version.
    ///
    /// # Errors
    /// [`RepoError::Db`] when the payment has no settlement row, or on a scope / storage
    /// failure; [`RepoError::Money`] when `delta` disagrees with the stored currency metadata
    /// or the exact result leaves the money contract; [`RepoError::MoneyOutCapExceeded`] when
    /// the post-state violates a settlement cap; [`RepoError::Conflict`] when the observed
    /// version is stale, or on classified database contention;
    /// [`RepoError::InvalidStoredMoney`] when the stored row is malformed.
    pub async fn add_clawed_back(
        &self,
        txn: &DbTx<'_>,
        scope: &AccessScope,
        tenant: Uuid,
        payment_id: &str,
        delta: &PostedMoney,
    ) -> Result<(), RepoError> {
        self.add_settlement_counter(
            txn,
            scope,
            tenant,
            payment_id,
            SettlementCounter::ClawedBack,
            delta,
        )
        .await
    }

    /// Read both allocation/refund counters on the supplied snapshot.
    ///
    /// # Errors
    /// [`RepoError::Db`] on a scope or storage failure; [`RepoError::Conflict`] on classified
    /// database contention; [`RepoError::InvalidStoredMoney`] when a stored counter is
    /// malformed, the stored version is negative, or the stored counters violate their caps.
    pub async fn read_allocation_refund_in<R: DBRunner>(
        &self,
        runner: &R,
        scope: &AccessScope,
        tenant: Uuid,
        payment_id: &str,
        invoice_id: &str,
    ) -> Result<Option<AllocationRefundState>, RepoError> {
        self.select_allocation_refund(runner, scope, tenant, payment_id, invoice_id, false)
            .await
    }

    /// Standalone allocation/refund read, preserving absence without a metadata-less zero.
    ///
    /// # Errors
    /// [`RepoError::Db`] when no connection can be acquired or on a scope / storage failure;
    /// [`RepoError::Conflict`] on classified database contention;
    /// [`RepoError::InvalidStoredMoney`] when a stored counter is malformed, the stored version
    /// is negative, or the stored counters violate their caps.
    pub async fn read_allocation_refund(
        &self,
        scope: &AccessScope,
        tenant: Uuid,
        payment_id: &str,
        invoice_id: &str,
    ) -> Result<Option<AllocationRefundState>, RepoError> {
        let conn = self
            .db
            .conn()
            .map_err(|e| db_to_repo(e, self.db.db().backend()))?;
        self.read_allocation_refund_in(&conn, scope, tenant, payment_id, invoice_id)
            .await
    }

    /// Lock the complete allocation/refund row after the settlement row, if present.
    ///
    /// # Errors
    /// [`RepoError::Db`] on a scope or storage failure; [`RepoError::Conflict`] on classified
    /// database contention; [`RepoError::InvalidStoredMoney`] when a stored counter is
    /// malformed, the stored version is negative, or the stored counters violate their caps.
    pub async fn read_allocation_refund_for_update(
        &self,
        txn: &DbTx<'_>,
        scope: &AccessScope,
        tenant: Uuid,
        payment_id: &str,
        invoice_id: &str,
    ) -> Result<Option<AllocationRefundState>, RepoError> {
        self.select_allocation_refund(txn, scope, tenant, payment_id, invoice_id, true)
            .await
    }

    /// Shared scoped allocation/refund select; money never leaves as raw text.
    async fn select_allocation_refund<R: DBRunner>(
        &self,
        runner: &R,
        scope: &AccessScope,
        tenant: Uuid,
        payment_id: &str,
        invoice_id: &str,
        lock: bool,
    ) -> Result<Option<AllocationRefundState>, RepoError> {
        let mut find = payment_allocation_refund::Entity::find();
        if lock && self.db.db().backend() == sea_orm::DbBackend::Postgres {
            find = find.lock(LockType::Update);
        }
        find.secure()
            .scope_with(scope)
            .filter(allocation_refund_key(tenant, payment_id, invoice_id))
            .one(runner)
            .await
            .map_err(|e| scope_to_repo(e, self.db.db().backend()))?
            .map(AllocationRefundState::decode)
            .transpose()
    }

    /// Reserve or release a per-invoice refund. A missing allocation remains an error.
    ///
    /// # Errors
    /// [`RepoError::Db`] when the invoice was never allocated from this payment, or on a scope
    /// / storage failure; [`RepoError::Money`] when `delta` disagrees with the stored currency
    /// metadata or the exact result leaves the money contract;
    /// [`RepoError::MoneyOutCapExceeded`] when the refund would exceed the allocation or go
    /// negative; [`RepoError::Conflict`] when the observed version is stale, or on classified
    /// database contention; [`RepoError::InvalidStoredMoney`] when the stored row is malformed.
    pub async fn add_allocation_refund_refunded(
        &self,
        txn: &DbTx<'_>,
        scope: &AccessScope,
        tenant: Uuid,
        payment_id: &str,
        invoice_id: &str,
        delta: &PostedMoney,
    ) -> Result<(), RepoError> {
        let state = self
            .read_allocation_refund_for_update(txn, scope, tenant, payment_id, invoice_id)
            .await?
            .ok_or_else(|| {
                RepoError::Db(format!(
                    "payment_allocation_refund absent for ({tenant}, {payment_id}, {invoice_id}): \
                     invoice never allocated"
                ))
            })?;
        self.update_allocation_refund(txn, scope, &state, AllocationRefundCounter::Refunded, delta)
            .await
    }

    /// Add or release allocated value, inserting a missing grain under its existing key.
    /// A competing missing-grain insert aborts the transaction as typed Conflict.
    ///
    /// # Errors
    /// [`RepoError::Money`] when `delta` disagrees with the stored currency metadata or the
    /// exact result leaves the money contract; [`RepoError::MoneyOutCapExceeded`] when the
    /// allocation would go negative or drop below its refunds; [`RepoError::Conflict`] when the
    /// observed version is stale, a competing missing-grain insert won the key, or on
    /// classified database contention; [`RepoError::Db`] on a scope or storage failure;
    /// [`RepoError::InvalidStoredMoney`] when the stored row is malformed.
    pub async fn bump_allocation_refund(
        &self,
        txn: &DbTx<'_>,
        scope: &AccessScope,
        tenant: Uuid,
        payment_id: &str,
        invoice_id: &str,
        delta: &PostedMoney,
    ) -> Result<(), RepoError> {
        if let Some(state) = self
            .read_allocation_refund_for_update(txn, scope, tenant, payment_id, invoice_id)
            .await?
        {
            return self
                .update_allocation_refund(
                    txn,
                    scope,
                    &state,
                    AllocationRefundCounter::Allocated,
                    delta,
                )
                .await;
        }
        self.insert_allocation_refund(txn, scope, tenant, payment_id, invoice_id, delta)
            .await
    }

    /// Add allocated value to several invoices of one payment: one locked read of
    /// the existing grains (`invoice_id IN (...)`, in invoice order), then a
    /// version CAS per existing grain and an insert per missing one, instead of a
    /// locked read per invoice. Splits naming the same invoice twice fall back to
    /// one [`Self::bump_allocation_refund`] each, so every step sees the previous one.
    ///
    /// # Errors
    /// As [`Self::bump_allocation_refund`], for the first split that fails.
    pub async fn bump_allocation_refunds(
        &self,
        txn: &DbTx<'_>,
        scope: &AccessScope,
        tenant: Uuid,
        payment_id: &str,
        splits: &[(&str, &PostedMoney)],
    ) -> Result<(), RepoError> {
        let invoices: std::collections::BTreeSet<&str> =
            splits.iter().map(|(invoice, _)| *invoice).collect();
        if invoices.len() != splits.len() {
            for (invoice, delta) in splits {
                self.bump_allocation_refund(txn, scope, tenant, payment_id, invoice, delta)
                    .await?;
            }
            return Ok(());
        }
        if invoices.is_empty() {
            return Ok(());
        }
        let mut find = payment_allocation_refund::Entity::find();
        if self.db.db().backend() == sea_orm::DbBackend::Postgres {
            find = find.lock(LockType::Update);
        }
        let mut existing: std::collections::HashMap<String, AllocationRefundState> = find
            .secure()
            .scope_with(scope)
            .filter(
                Condition::all()
                    .add(payment_allocation_refund::Column::TenantId.eq(tenant))
                    .add(payment_allocation_refund::Column::PaymentId.eq(payment_id))
                    .add(payment_allocation_refund::Column::InvoiceId.is_in(invoices)),
            )
            .order_by(payment_allocation_refund::Column::InvoiceId, Order::Asc)
            .all(txn)
            .await
            .map_err(|e| scope_to_repo(e, self.db.db().backend()))?
            .into_iter()
            .map(|row| AllocationRefundState::decode(row).map(|s| (s.invoice_id.clone(), s)))
            .collect::<Result<_, _>>()?;
        for (invoice, delta) in splits {
            match existing.remove(*invoice) {
                Some(state) => {
                    self.update_allocation_refund(
                        txn,
                        scope,
                        &state,
                        AllocationRefundCounter::Allocated,
                        delta,
                    )
                    .await?;
                }
                None => {
                    self.insert_allocation_refund(txn, scope, tenant, payment_id, invoice, delta)
                        .await?;
                }
            }
        }
        Ok(())
    }

    /// Insert a validated missing grain; its primary key is the only unique identity.
    async fn insert_allocation_refund(
        &self,
        txn: &DbTx<'_>,
        scope: &AccessScope,
        tenant: Uuid,
        payment_id: &str,
        invoice_id: &str,
        allocated: &PostedMoney,
    ) -> Result<(), RepoError> {
        validate_allocation_refund(&exact(allocated), &ExactAmount::from_decimal(Decimal::ZERO))?;
        let am = payment_allocation_refund::ActiveModel {
            currency: Set(allocated.currency().code().into()),
            currency_scale: Set(i16::from(allocated.currency().scale())),
            tenant_id: Set(tenant),
            payment_id: Set(payment_id.into()),
            invoice_id: Set(invoice_id.into()),
            allocated: Set(encode_amount(allocated)),
            refunded: Set("0".into()),
            version: Set(0),
        };
        payment_allocation_refund::Entity::insert(am.clone())
            .secure()
            .scope_with_model(scope, &am)
            .map_err(|e| scope_to_repo(e, self.db.db().backend()))?
            .exec(txn)
            .await
            .map_err(|e| insert_to_repo(e, self.db.db().backend()))?;
        Ok(())
    }

    /// Add `delta` to the chosen counter, validate both correlated counters,
    /// then write the chosen one in a literal CAS.
    async fn update_allocation_refund(
        &self,
        txn: &DbTx<'_>,
        scope: &AccessScope,
        state: &AllocationRefundState,
        counter: AllocationRefundCounter,
        delta: &PostedMoney,
    ) -> Result<(), RepoError> {
        matching(&state.allocated, delta)?;
        let mut allocated = exact(&state.allocated);
        let mut refunded = exact(&state.refunded);
        let target = match counter {
            AllocationRefundCounter::Allocated => &mut allocated,
            AllocationRefundCounter::Refunded => &mut refunded,
        };
        *target = target.checked_add(&exact(delta)).map_err(exact_error)?;
        validate_allocation_refund(&allocated, &refunded)?;
        let value = match counter {
            AllocationRefundCounter::Allocated => allocated,
            AllocationRefundCounter::Refunded => refunded,
        }
        .into_posted_exact(state.allocated.currency().clone())
        .map_err(exact_error)?;
        let result = payment_allocation_refund::Entity::update_many()
            .secure()
            .scope_with(scope)
            .col_expr(counter.column(), Expr::value(encode_amount(&value)))
            .col_expr(
                payment_allocation_refund::Column::Version,
                Expr::value(next_version(state.version)?),
            )
            .filter(
                allocation_refund_key(state.tenant_id, &state.payment_id, &state.invoice_id)
                    .add(payment_allocation_refund::Column::Version.eq(state.version)),
            )
            .exec(txn)
            .await
            .map_err(|e| scope_to_repo(e, self.db.db().backend()))?;
        require_one(result.rows_affected)
    }

    /// Append positive allocation splits with immutable policy and audit fields.
    ///
    /// # Errors
    /// [`RepoError::MoneyOutCapExceeded`] when a row's amount is not strictly positive;
    /// [`RepoError::Db`] on a scope or storage failure; [`RepoError::Conflict`] on classified
    /// database contention.
    pub async fn insert_allocation_rows(
        &self,
        txn: &DbTx<'_>,
        scope: &AccessScope,
        rows: &[NewAllocationRow],
    ) -> Result<(), RepoError> {
        for row in rows {
            require_positive(&row.amount)?;
        }
        for row in rows {
            let am = payment_allocation::ActiveModel {
                tenant_id: Set(row.tenant_id),
                allocation_id: Set(row.allocation_id),
                invoice_id: Set(row.invoice_id.clone()),
                payer_tenant_id: Set(row.payer_tenant_id),
                payment_id: Set(row.payment_id.clone()),
                amount: Set(encode_amount(&row.amount)),
                currency: Set(row.amount.currency().code().into()),
                currency_scale: Set(i16::from(row.amount.currency().scale())),
                precedence_policy_ref: Set(row.precedence_policy_ref.clone()),
                allocated_at_utc: Set(row.allocated_at_utc),
            };
            payment_allocation::Entity::insert(am.clone())
                .secure()
                .scope_with_model(scope, &am)
                .map_err(|e| scope_to_repo(e, self.db.db().backend()))?
                .exec(txn)
                .await
                .map_err(|e| scope_to_repo(e, self.db.db().backend()))?;
        }
        Ok(())
    }

    /// Read immutable allocations on the supplied runner, retaining invoice ordering.
    ///
    /// # Errors
    /// [`RepoError::Db`] on a scope or storage failure; [`RepoError::Conflict`] on classified
    /// database contention; [`RepoError::InvalidStoredMoney`] when a stored amount is malformed
    /// or a stored allocation is not strictly positive.
    pub async fn list_payment_allocations_in<R: DBRunner>(
        &self,
        runner: &R,
        scope: &AccessScope,
        tenant: Uuid,
        payment_id: &str,
    ) -> Result<Vec<StoredAllocation>, RepoError> {
        payment_allocation::Entity::find()
            .secure()
            .scope_with(scope)
            .filter(
                Condition::all()
                    .add(payment_allocation::Column::TenantId.eq(tenant))
                    .add(payment_allocation::Column::PaymentId.eq(payment_id)),
            )
            .order_by(payment_allocation::Column::InvoiceId, Order::Asc)
            .all(runner)
            .await
            .map_err(|e| scope_to_repo(e, self.db.db().backend()))?
            .into_iter()
            .map(StoredAllocation::decode)
            .collect()
    }

    /// Standalone allocation list, returning decoded money rather than entity text.
    ///
    /// # Errors
    /// [`RepoError::Db`] when no connection can be acquired or on a scope / storage failure;
    /// [`RepoError::Conflict`] on classified database contention;
    /// [`RepoError::InvalidStoredMoney`] when a stored amount is malformed or a stored
    /// allocation is not strictly positive.
    pub async fn list_payment_allocations(
        &self,
        scope: &AccessScope,
        tenant: Uuid,
        payment_id: &str,
    ) -> Result<Vec<StoredAllocation>, RepoError> {
        let conn = self
            .db
            .conn()
            .map_err(|e| db_to_repo(e, self.db.db().backend()))?;
        self.list_payment_allocations_in(&conn, scope, tenant, payment_id)
            .await
    }
}

/// The two correlated per-invoice counters of a `payment_allocation_refund` row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AllocationRefundCounter {
    /// The value allocated to the invoice from the payment.
    Allocated,
    /// The value refunded against that allocation.
    Refunded,
}

impl AllocationRefundCounter {
    /// Concrete literal update column.
    fn column(self) -> payment_allocation_refund::Column {
        match self {
            Self::Allocated => payment_allocation_refund::Column::Allocated,
            Self::Refunded => payment_allocation_refund::Column::Refunded,
        }
    }
}

/// The six correlated settlement counters.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SettlementCounter {
    /// The settled gross.
    Settled,
    /// The withheld PSP fee.
    Fee,
    /// The total allocated to invoices.
    Allocated,
    /// The total refunded against allocations.
    Refunded,
    /// The total refunded out of the unallocated pool.
    RefundedUnallocated,
    /// The total clawed back by lost chargebacks.
    ClawedBack,
}
impl SettlementCounter {
    /// Concrete literal update column.
    fn column(self) -> payment_settlement::Column {
        match self {
            Self::Settled => payment_settlement::Column::Settled,
            Self::Fee => payment_settlement::Column::Fee,
            Self::Allocated => payment_settlement::Column::Allocated,
            Self::Refunded => payment_settlement::Column::Refunded,
            Self::RefundedUnallocated => payment_settlement::Column::RefundedUnallocated,
            Self::ClawedBack => payment_settlement::Column::ClawedBack,
        }
    }
}

impl SettlementState {
    /// Decode and reject corrupt existing state before any mutation.
    fn decode(row: payment_settlement::Model) -> Result<Self, RepoError> {
        valid_version(row.version)?;
        let money = |text: &str| decode_money(text, &row.currency, row.currency_scale);
        let state = Self {
            tenant_id: row.tenant_id,
            payment_id: row.payment_id.clone(),
            version: row.version,
            settled: money(&row.settled)?,
            fee: money(&row.fee)?,
            allocated: money(&row.allocated)?,
            refunded: money(&row.refunded)?,
            refunded_unallocated: money(&row.refunded_unallocated)?,
            clawed_back: money(&row.clawed_back)?,
        };
        validate_settlement(&state.exact()).map_err(stored_invariant)?;
        Ok(state)
    }
    /// Lift all counters without bounded intermediate narrowing.
    fn exact(&self) -> SettlementTotals {
        SettlementTotals {
            settled: exact(&self.settled),
            fee: exact(&self.fee),
            allocated: exact(&self.allocated),
            refunded: exact(&self.refunded),
            refunded_unallocated: exact(&self.refunded_unallocated),
            clawed_back: exact(&self.clawed_back),
        }
    }
}

/// The six settlement counters as unbounded exact values, one named field per
/// [`SettlementCounter`]: a counter is reached by name, never by position.
#[derive(Clone, Debug, PartialEq, Eq)]
struct SettlementTotals {
    settled: ExactAmount,
    fee: ExactAmount,
    allocated: ExactAmount,
    refunded: ExactAmount,
    refunded_unallocated: ExactAmount,
    clawed_back: ExactAmount,
}

impl SettlementTotals {
    /// The value of `counter`.
    fn get(&self, counter: SettlementCounter) -> &ExactAmount {
        match counter {
            SettlementCounter::Settled => &self.settled,
            SettlementCounter::Fee => &self.fee,
            SettlementCounter::Allocated => &self.allocated,
            SettlementCounter::Refunded => &self.refunded,
            SettlementCounter::RefundedUnallocated => &self.refunded_unallocated,
            SettlementCounter::ClawedBack => &self.clawed_back,
        }
    }

    /// The value of `counter`, for an in-place delta.
    fn get_mut(&mut self, counter: SettlementCounter) -> &mut ExactAmount {
        match counter {
            SettlementCounter::Settled => &mut self.settled,
            SettlementCounter::Fee => &mut self.fee,
            SettlementCounter::Allocated => &mut self.allocated,
            SettlementCounter::Refunded => &mut self.refunded,
            SettlementCounter::RefundedUnallocated => &mut self.refunded_unallocated,
            SettlementCounter::ClawedBack => &mut self.clawed_back,
        }
    }
}
impl AllocationRefundState {
    /// Restore matching money metadata and validate existing correlated caps.
    fn decode(row: payment_allocation_refund::Model) -> Result<Self, RepoError> {
        valid_version(row.version)?;
        let state = Self {
            tenant_id: row.tenant_id,
            payment_id: row.payment_id,
            invoice_id: row.invoice_id,
            version: row.version,
            allocated: decode_money(&row.allocated, &row.currency, row.currency_scale)?,
            refunded: decode_money(&row.refunded, &row.currency, row.currency_scale)?,
        };
        validate_allocation_refund(&exact(&state.allocated), &exact(&state.refunded))
            .map_err(stored_invariant)?;
        Ok(state)
    }
}
impl StoredAllocation {
    /// Decode immutable history without a live registry lookup.
    fn decode(row: payment_allocation::Model) -> Result<Self, RepoError> {
        let amount = decode_money(&row.amount, &row.currency, row.currency_scale)?;
        require_positive(&amount).map_err(stored_invariant)?;
        Ok(Self {
            tenant_id: row.tenant_id,
            allocation_id: row.allocation_id,
            payer_tenant_id: row.payer_tenant_id,
            payment_id: row.payment_id,
            invoice_id: row.invoice_id,
            amount,
            precedence_policy_ref: row.precedence_policy_ref,
            allocated_at_utc: row.allocated_at_utc,
        })
    }
}

/// Shared real settlement key; scale is never an identity axis.
fn settlement_key(tenant: Uuid, payment: &str) -> Condition {
    Condition::all()
        .add(payment_settlement::Column::TenantId.eq(tenant))
        .add(payment_settlement::Column::PaymentId.eq(payment))
}
/// Shared real per-invoice allocation key.
fn allocation_refund_key(tenant: Uuid, payment: &str, invoice: &str) -> Condition {
    Condition::all()
        .add(payment_allocation_refund::Column::TenantId.eq(tenant))
        .add(payment_allocation_refund::Column::PaymentId.eq(payment))
        .add(payment_allocation_refund::Column::InvoiceId.eq(invoice))
}
/// Validate metadata before exact arithmetic, even for a zero delta.
fn matching(left: &PostedMoney, right: &PostedMoney) -> Result<(), RepoError> {
    Ok(left.currency().ensure_same(right.currency())?)
}
/// Preserve every input decimal digit in an unrounded exact intermediate.
fn exact(value: &PostedMoney) -> ExactAmount {
    ExactAmount::from_decimal(value.amount())
}
/// Translate numeric contract failures without pretending they are cap failures.
fn exact_error(error: ExactError) -> RepoError {
    match error {
        ExactError::Money(e) => RepoError::Money(e),
        other => RepoError::Db(other.to_string()),
    }
}
/// Fail closed on corrupt persisted invariants, never retry or report an input cap.
fn stored_invariant(error: RepoError) -> RepoError {
    RepoError::InvalidStoredMoney(error.to_string())
}
/// An exact comparison whose operands may exceed the posted amount bound.
fn exceeds(left: &ExactAmount, right: &ExactAmount) -> bool {
    left > right
}
/// Preserve gross allocation and both independent cash-out/headroom caps.
fn validate_settlement(v: &SettlementTotals) -> Result<(), RepoError> {
    let SettlementTotals {
        settled,
        fee,
        allocated,
        refunded,
        refunded_unallocated,
        clawed_back,
    } = v;
    if [
        settled,
        fee,
        allocated,
        refunded,
        refunded_unallocated,
        clawed_back,
    ]
    .into_iter()
    .any(ExactAmount::is_negative)
        || exceeds(allocated, settled)
        || exceeds(fee, settled)
        || exceeds(refunded, settled)
        || exceeds(
            &allocated
                .checked_add(refunded_unallocated)
                .map_err(exact_error)?,
            settled,
        )
        || exceeds(
            &refunded.checked_add(clawed_back).map_err(exact_error)?,
            settled,
        )
    {
        return Err(RepoError::MoneyOutCapExceeded(
            "payment_settlement counter cap or underflow".into(),
        ));
    }
    Ok(())
}
/// Preserve the allocation_refund marker used by the refund business adapter.
fn validate_allocation_refund(
    allocated: &ExactAmount,
    refunded: &ExactAmount,
) -> Result<(), RepoError> {
    if allocated.is_negative() || refunded.is_negative() || exceeds(refunded, allocated) {
        return Err(RepoError::MoneyOutCapExceeded(
            "allocation_refund counter cap or underflow".into(),
        ));
    }
    Ok(())
}
/// Allocation journal splits are strictly positive, unlike mutable signed deltas.
fn require_positive(amount: &PostedMoney) -> Result<(), RepoError> {
    if amount.amount() <= Decimal::ZERO {
        return Err(RepoError::MoneyOutCapExceeded(
            "allocation amount must be positive".into(),
        ));
    }
    Ok(())
}
/// Reject corrupt negative versions before reading a financial snapshot.
fn valid_version(version: i64) -> Result<(), RepoError> {
    if version < 0 {
        return Err(RepoError::InvalidStoredMoney(
            "negative payment counter version".into(),
        ));
    }
    Ok(())
}
/// Version wrap is an invariant failure, never saturation or a retryable conflict.
fn next_version(version: i64) -> Result<i64, RepoError> {
    valid_version(version)?;
    version
        .checked_add(1)
        .ok_or_else(|| RepoError::Db("payment counter version exhausted".into()))
}
/// Every CAS miss must leave the enclosing transaction immediately.
fn require_one(rows: u64) -> Result<(), RepoError> {
    if rows != 1 {
        return Err(RepoError::Conflict("stale payment counter version".into()));
    }
    Ok(())
}

#[cfg(test)]
#[path = "counter_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "counters_batch_tests.rs"]
mod batch_tests;
