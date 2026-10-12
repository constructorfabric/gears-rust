//! Exact dispute current state. Writes run on the caller's transaction, at lock
//! rank zero (before settlement). Every conflict must abort the whole attempt.
use bss_ledger_sdk::PostedMoney;
use rust_decimal::Decimal;
use sea_orm::sea_query::Expr;
use sea_orm::{ActiveValue::Set, ColumnTrait, Condition, EntityTrait};
use toolkit_db::odata::sea_orm_filter::{LimitCfg, paginate_odata};
use toolkit_db::secure::{
    AccessScope, DBRunner, DbTx, SecureEntityExt, SecureInsertExt, SecureUpdateExt,
};
use toolkit_db::{DBProvider, DbError};
use toolkit_odata::{ODataQuery, Page, SortDir};
use uuid::Uuid;

use crate::domain::model::RepoError;
use crate::domain::payment::dispute_state::{
    DisputePhase, DisputeTransitionError, DisputeVariant, ObservedDispute, check_open,
    check_outcome,
};
use crate::infra::posting::retry::{db_to_repo, insert_to_repo, scope_to_repo};
use crate::infra::storage::entity::dispute;
use crate::infra::storage::money_text::{decode_money, encode_amount};
use crate::infra::storage::odata_mapping::DisputeODataMapper;
use crate::infra::storage::repo::journal_repo::{
    OdataPageError, map_odata_err, query_with_default_order,
};
use crate::odata::DisputeFilterField;

/// A validated snapshot carrying the row's original currency and scale.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DisputeState {
    pub tenant_id: Uuid,
    pub dispute_id: String,
    pub payment_id: String,
    pub variant: DisputeVariant,
    pub last_phase: DisputePhase,
    pub cycle: i32,
    pub disputed_amount: PostedMoney,
    pub cash_hold: PostedMoney,
    pub version: i64,
}

impl DisputeState {
    /// The state the dispute state machine checks a transition against.
    #[must_use]
    pub fn observed(&self) -> ObservedDispute<'_> {
        ObservedDispute {
            payment_id: &self.payment_id,
            last_phase: self.last_phase,
            cycle: self.cycle,
        }
    }
}

/// SeaORM-backed dispute current-state repository.
#[derive(Clone)]
pub struct DisputeRepo {
    db: DBProvider<DbError>,
}

impl DisputeRepo {
    /// Bind the configured backend for real driver error classification.
    #[must_use]
    pub fn new(db: DBProvider<DbError>) -> Self {
        Self { db }
    }

    /// Open cycle one, or reopen a terminal WON/LOST row at the next cycle: the
    /// domain state machine ([`check_open`]) judges the in-transaction row, and
    /// the write is a CAS on that row's observed phase, cycle and version.
    /// Payment identity and money metadata cannot change; the new opening fact
    /// selects its variant. No independent retry occurs.
    ///
    /// # Errors
    /// [`RepoError::DisputeNotOpen`] when the state machine refuses the opening (the
    /// payment identity changes, a reopen does not follow a WON/LOST row at the next
    /// cycle, or a first opening is not cycle 1);
    /// [`RepoError::Money`] when the amounts disagree on currency metadata;
    /// [`RepoError::MoneyOutCapExceeded`] unless `0 <= cash_hold <= disputed_amount`;
    /// [`RepoError::Conflict`] when the observed row changed underneath, a concurrent insert
    /// won the key, or on classified database contention; [`RepoError::Db`] on a scope or
    /// storage failure; [`RepoError::InvalidStoredMoney`] when the stored row is malformed.
    #[expect(
        clippy::too_many_arguments,
        reason = "one opening fact: transaction, scope, dispute identity, cycle and both amounts"
    )]
    pub async fn dispute_upsert(
        &self,
        txn: &DbTx<'_>,
        scope: &AccessScope,
        tenant: Uuid,
        dispute_id: &str,
        payment_id: &str,
        variant: DisputeVariant,
        cycle: i32,
        disputed_amount: &PostedMoney,
        cash_hold: &PostedMoney,
    ) -> Result<(), RepoError> {
        let previous = self.read_dispute_in(txn, scope, tenant, dispute_id).await?;
        check_open(
            dispute_id,
            previous.as_ref().map(DisputeState::observed),
            payment_id,
            cycle,
        )
        .map_err(transition_error)?;
        let next = DisputeState {
            tenant_id: tenant,
            dispute_id: dispute_id.into(),
            payment_id: payment_id.into(),
            variant,
            last_phase: DisputePhase::Opened,
            cycle,
            disputed_amount: disputed_amount.clone(),
            cash_hold: cash_hold.clone(),
            version: 0,
        };
        if let Some(previous) = previous {
            matching(&previous.disputed_amount, disputed_amount)?;
            matching(disputed_amount, cash_hold)?;
            validate_amounts(disputed_amount, cash_hold)?;
            self.replace_observed(txn, scope, &previous, &next).await
        } else {
            matching(disputed_amount, cash_hold)?;
            validate_amounts(disputed_amount, cash_hold)?;
            self.insert_open(txn, scope, &next).await
        }
    }

    /// Plain intended-key insert. A real concurrent insertion aborts this attempt.
    async fn insert_open(
        &self,
        txn: &DbTx<'_>,
        scope: &AccessScope,
        row: &DisputeState,
    ) -> Result<(), RepoError> {
        let am = dispute::ActiveModel {
            tenant_id: Set(row.tenant_id),
            dispute_id: Set(row.dispute_id.clone()),
            payment_id: Set(row.payment_id.clone()),
            currency: Set(row.disputed_amount.currency().code().into()),
            currency_scale: Set(i16::from(row.disputed_amount.currency().scale())),
            variant: Set(row.variant.as_str().into()),
            last_phase: Set(row.last_phase.as_str().into()),
            cycle: Set(row.cycle),
            disputed_amount: Set(encode_amount(&row.disputed_amount)),
            cash_hold: Set(encode_amount(&row.cash_hold)),
            version: Set(0),
        };
        dispute::Entity::insert(am.clone())
            .secure()
            .scope_with_model(scope, &am)
            .map_err(|e| scope_to_repo(e, self.db.db().backend()))?
            .exec(txn)
            .await
            .map_err(|e| insert_to_repo(e, self.db.db().backend()))?;
        Ok(())
    }

    /// Resolve the observed OPENED cycle, retaining its stored hold even if a
    /// settlement return has since reduced the payment's net cash. The domain
    /// state machine ([`check_outcome`]) judges the in-transaction row; the write
    /// is a CAS on that row's observed phase, cycle and version.
    ///
    /// # Errors
    /// [`RepoError::DisputeNotOpen`] when the state machine refuses the outcome (no
    /// dispute exists, the stored row is not OPENED at `cycle`, or `last_phase` is
    /// not an outcome); [`RepoError::Money`] when `disputed_amount`
    /// disagrees with the stored currency metadata; [`RepoError::MoneyOutCapExceeded`] when the
    /// outcome amount drops below the stored hold; [`RepoError::Conflict`] when the observed
    /// state / version changed underneath, or on classified database contention;
    /// [`RepoError::Db`] on a scope or storage failure; [`RepoError::InvalidStoredMoney`] when
    /// the stored row is malformed.
    #[expect(
        clippy::too_many_arguments,
        reason = "one outcome fact: transaction, scope, dispute identity, phase, cycle and amount"
    )]
    pub async fn dispute_advance(
        &self,
        txn: &DbTx<'_>,
        scope: &AccessScope,
        tenant: Uuid,
        dispute_id: &str,
        last_phase: DisputePhase,
        cycle: i32,
        disputed_amount: &PostedMoney,
    ) -> Result<(), RepoError> {
        let previous = self.read_dispute_in(txn, scope, tenant, dispute_id).await?;
        check_outcome(
            dispute_id,
            previous.as_ref().map(DisputeState::observed),
            last_phase,
            cycle,
        )
        .map_err(transition_error)?;
        let Some(previous) = previous else {
            // `check_outcome` refuses a missing dispute; kept total, never a panic.
            return Err(RepoError::DisputeNotOpen(format!(
                "dispute {dispute_id} is missing"
            )));
        };
        matching(&previous.disputed_amount, disputed_amount)?;
        validate_amounts(disputed_amount, &previous.cash_hold)?;
        let next = DisputeState {
            last_phase,
            disputed_amount: disputed_amount.clone(),
            ..previous.clone()
        };
        self.replace_observed(txn, scope, &previous, &next).await
    }

    /// Literal CAS using observed phase, cycle and version. A miss after a valid
    /// read is contention, never a synthetic database error or phase rejection.
    async fn replace_observed(
        &self,
        txn: &DbTx<'_>,
        scope: &AccessScope,
        previous: &DisputeState,
        next: &DisputeState,
    ) -> Result<(), RepoError> {
        let version = previous
            .version
            .checked_add(1)
            .ok_or_else(|| RepoError::Db("dispute version exhausted".into()))?;
        let result = dispute::Entity::update_many()
            .secure()
            .scope_with(scope)
            .col_expr(dispute::Column::Variant, Expr::value(next.variant.as_str()))
            .col_expr(
                dispute::Column::LastPhase,
                Expr::value(next.last_phase.as_str()),
            )
            .col_expr(dispute::Column::Cycle, Expr::value(next.cycle))
            .col_expr(
                dispute::Column::DisputedAmount,
                Expr::value(encode_amount(&next.disputed_amount)),
            )
            .col_expr(
                dispute::Column::CashHold,
                Expr::value(encode_amount(&next.cash_hold)),
            )
            .col_expr(dispute::Column::Version, Expr::value(version))
            .filter(
                key(previous.tenant_id, &previous.dispute_id)
                    .add(dispute::Column::Version.eq(previous.version))
                    .add(dispute::Column::LastPhase.eq(previous.last_phase.as_str()))
                    .add(dispute::Column::Cycle.eq(previous.cycle)),
            )
            .exec(txn)
            .await
            .map_err(|e| scope_to_repo(e, self.db.db().backend()))?;
        if result.rows_affected != 1 {
            return Err(RepoError::Conflict("stale dispute state/version".into()));
        }
        Ok(())
    }

    /// Scoped read on the caller's financial snapshot, using stored metadata.
    ///
    /// # Errors
    /// [`RepoError::Db`] on a scope or storage failure; [`RepoError::Conflict`] on classified
    /// database contention. [`RepoError::InvalidStoredMoney`] when a stored amount is malformed
    /// or off its currency contract.
    pub async fn read_dispute_in<R: DBRunner>(
        &self,
        runner: &R,
        scope: &AccessScope,
        tenant: Uuid,
        dispute_id: &str,
    ) -> Result<Option<DisputeState>, RepoError> {
        dispute::Entity::find()
            .secure()
            .scope_with(scope)
            .filter(key(tenant, dispute_id))
            .one(runner)
            .await
            .map_err(|e| scope_to_repo(e, self.db.db().backend()))?
            .map(decode)
            .transpose()
    }
    /// Standalone twin of the caller-runner read.
    ///
    /// # Errors
    /// [`RepoError::Db`] when no connection can be acquired or on a scope / storage failure;
    /// [`RepoError::Conflict`] on classified database contention.
    /// [`RepoError::InvalidStoredMoney`] when a stored amount is malformed or off its currency
    /// contract.
    pub async fn read_dispute(
        &self,
        scope: &AccessScope,
        tenant: Uuid,
        dispute_id: &str,
    ) -> Result<Option<DisputeState>, RepoError> {
        let conn = self
            .db
            .conn()
            .map_err(|e| db_to_repo(e, self.db.db().backend()))?;
        self.read_dispute_in(&conn, scope, tenant, dispute_id).await
    }

    /// Read an OPENED dispute for the payment. The schema does not guarantee one
    /// such row per payment; this preserves the existing existence-read contract.
    ///
    /// # Errors
    /// [`RepoError::Db`] on a scope or storage failure; [`RepoError::Conflict`] on classified
    /// database contention. [`RepoError::InvalidStoredMoney`] when a stored amount is malformed
    /// or off its currency contract.
    pub async fn read_open_dispute_for_payment_in<R: DBRunner>(
        &self,
        runner: &R,
        scope: &AccessScope,
        tenant: Uuid,
        payment_id: &str,
    ) -> Result<Option<DisputeState>, RepoError> {
        dispute::Entity::find()
            .secure()
            .scope_with(scope)
            .filter(
                Condition::all()
                    .add(dispute::Column::TenantId.eq(tenant))
                    .add(dispute::Column::PaymentId.eq(payment_id))
                    .add(dispute::Column::LastPhase.eq(DisputePhase::Opened.as_str())),
            )
            .one(runner)
            .await
            .map_err(|e| scope_to_repo(e, self.db.db().backend()))?
            .map(decode)
            .transpose()
    }
    /// Standalone twin of the caller-runner open-dispute read.
    ///
    /// # Errors
    /// [`RepoError::Db`] when no connection can be acquired or on a scope / storage failure;
    /// [`RepoError::Conflict`] on classified database contention.
    /// [`RepoError::InvalidStoredMoney`] when a stored amount is malformed or off its currency
    /// contract.
    pub async fn read_open_dispute_for_payment(
        &self,
        scope: &AccessScope,
        tenant: Uuid,
        payment_id: &str,
    ) -> Result<Option<DisputeState>, RepoError> {
        let conn = self
            .db
            .conn()
            .map_err(|e| db_to_repo(e, self.db.db().backend()))?;
        self.read_open_dispute_for_payment_in(&conn, scope, tenant, payment_id)
            .await
    }

    /// Typed OData page preserving nonmonetary filters, ordering and cursors.
    ///
    /// # Errors
    /// [`OdataPageError::Db`] on a storage / connection failure; [`OdataPageError::Odata`] on a
    /// malformed `$filter` / `$orderby` / cursor (the caller projects it to a canonical 400).
    pub async fn list_disputes(
        &self,
        scope: &AccessScope,
        tenant: Uuid,
        query: &ODataQuery,
    ) -> Result<Page<DisputeState>, OdataPageError> {
        let conn = self
            .db
            .conn()
            .map_err(|e| OdataPageError::Db(e.to_string()))?;
        self.list_disputes_in(&conn, scope, tenant, query).await
    }
    /// Paginate on a caller runner and decode every returned monetary row.
    ///
    /// # Errors
    /// [`OdataPageError::Db`] on a storage failure or when a stored amount is malformed;
    /// [`OdataPageError::Odata`] on a malformed `$filter` / `$orderby` / cursor (the caller
    /// projects it to a canonical 400).
    pub async fn list_disputes_in<R: DBRunner>(
        &self,
        runner: &R,
        scope: &AccessScope,
        tenant: Uuid,
        query: &ODataQuery,
    ) -> Result<Page<DisputeState>, OdataPageError> {
        let base = dispute::Entity::find()
            .secure()
            .scope_with(scope)
            .filter(dispute::Column::TenantId.eq(tenant).into());
        let query = query_with_default_order(query, "dispute_id");
        let page = paginate_odata::<
            DisputeFilterField,
            DisputeODataMapper,
            dispute::Entity,
            dispute::Model,
            _,
            _,
        >(
            base,
            runner,
            &query,
            ("dispute_id", SortDir::Asc),
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
            .map(decode)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| OdataPageError::Db(e.to_string()))?;
        Ok(Page::new(items, page.page_info))
    }
}

/// Preserve the actual composite business key without adding a money axis.
fn key(tenant: Uuid, dispute_id: &str) -> Condition {
    Condition::all()
        .add(dispute::Column::TenantId.eq(tenant))
        .add(dispute::Column::DisputeId.eq(dispute_id))
}
/// A refused transition aborts the attempt as a dispute-state rejection.
fn transition_error(error: DisputeTransitionError) -> RepoError {
    RepoError::DisputeNotOpen(error.to_string())
}
/// Compare metadata even for zero values.
fn matching(left: &PostedMoney, right: &PostedMoney) -> Result<(), RepoError> {
    Ok(left.currency().ensure_same(right.currency())?)
}
/// Comparisons are exact on the validated decimal carrier; no sums or narrowing.
fn validate_amounts(amount: &PostedMoney, hold: &PostedMoney) -> Result<(), RepoError> {
    if amount.amount() < Decimal::ZERO
        || hold.amount() < Decimal::ZERO
        || hold.amount() > amount.amount()
    {
        return Err(RepoError::MoneyOutCapExceeded(
            "dispute requires 0 <= cash_hold <= disputed_amount".into(),
        ));
    }
    Ok(())
}
/// Validate every stored field before exposing a business snapshot.
fn decode(row: dispute::Model) -> Result<DisputeState, RepoError> {
    let disputed_amount = decode_money(&row.disputed_amount, &row.currency, row.currency_scale)?;
    let cash_hold = decode_money(&row.cash_hold, &row.currency, row.currency_scale)?;
    validate_amounts(&disputed_amount, &cash_hold)
        .map_err(|e| RepoError::InvalidStoredMoney(e.to_string()))?;
    let invalid =
        || RepoError::InvalidStoredMoney("invalid dispute phase/variant/cycle/version".into());
    let variant = DisputeVariant::parse(&row.variant).ok_or_else(invalid)?;
    let last_phase = DisputePhase::parse(&row.last_phase).ok_or_else(invalid)?;
    if row.cycle < 1 || row.version < 0 {
        return Err(invalid());
    }
    Ok(DisputeState {
        tenant_id: row.tenant_id,
        dispute_id: row.dispute_id,
        payment_id: row.payment_id,
        variant,
        last_phase,
        cycle: row.cycle,
        disputed_amount,
        cash_hold,
        version: row.version,
    })
}

#[cfg(test)]
#[path = "dispute_repo/tests.rs"]
mod tests;

#[cfg(test)]
#[path = "dispute_repo_detail_tests.rs"]
mod detail_tests;
