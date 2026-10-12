//! Canonical recognition persistence. Mutable rows use caller-transaction reads and literal CAS.
//! Due snapshots are advisory; callers rebuild all decisions in the posting attempt.

use std::collections::HashMap;

use crate::domain::exact_money::{ExactAmount, ExactError};
use crate::infra::posting::retry::{db_to_repo, scope_to_repo};
use crate::infra::storage::money_text::{decode_money, encode_amount};
use bss_ledger_sdk::{AccountClass, CurrencySpec, PostedMoney, Side, SourceDocType};
use rust_decimal::Decimal;

use sea_orm::sea_query::Expr;
use sea_orm::{ActiveValue::Set, ColumnTrait, Condition, EntityTrait, Order};
use toolkit_db::secure::{
    AccessScope, DBRunner, DbTx, SecureEntityExt, SecureInsertExt, SecureUpdateExt,
};
use toolkit_db::{DBProvider, DbError};
use uuid::Uuid;

use crate::domain::model::RepoError;
use crate::domain::status::{
    PERIOD_STATUS_OPEN, RUN_STATUS_DONE, RUN_STATUS_FAILED, RUN_STATUS_RUNNING,
    SCHEDULE_STATUS_ACTIVE, SCHEDULE_STATUS_CANCELLED, SCHEDULE_STATUS_COMPLETED,
    SCHEDULE_STATUS_REPLACED, SEGMENT_STATUS_DONE, SEGMENT_STATUS_PENDING, SEGMENT_STATUS_QUEUED,
};
use toolkit_db::odata::sea_orm_filter::{LimitCfg, paginate_odata};
use toolkit_odata::{ODataQuery, Page, SortDir};

use crate::infra::storage::entity::{
    fiscal_period, journal_entry, journal_line, recognition_run, recognition_schedule,
    recognition_segment,
};
use crate::infra::storage::odata_mapping::RecognitionRunODataMapper;
use crate::infra::storage::repo::journal_repo::{
    OdataPageError, map_odata_err, query_with_default_order,
};
use crate::odata::RecognitionRunFilterField;
use time::OffsetDateTime;

/// Validated immutable metadata plus the correlated schedule amounts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScheduleState {
    pub tenant_id: Uuid,
    pub schedule_id: String,
    pub payer_tenant_id: Uuid,
    pub source_invoice_id: String,
    pub source_invoice_item_ref: String,
    pub po_allocation_group: Option<String>,
    pub subscription_ref: Option<String>,
    pub revenue_stream: String,
    pub total_deferred: PostedMoney,
    pub recognized: PostedMoney,
    pub policy_ref: String,
    pub ssp_snapshot_ref: Option<String>,
    pub vc_estimate_ref: Option<String>,
    pub vc_method_ref: Option<String>,
    pub status: String,
    pub version: i64,
}
/// Validated segment money, phase, and shared mutation token.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SegmentState {
    pub tenant_id: Uuid,
    pub schedule_id: String,
    pub segment_no: i32,
    pub period_id: String,
    pub amount: PostedMoney,
    pub version: i64,
    pub status: String,
    pub recognized_at: Option<OffsetDateTime>,
    pub run_id: Option<Uuid>,
}
/// New schedule input in major units.
pub struct NewSchedule {
    pub tenant_id: Uuid,
    pub schedule_id: String,
    pub payer_tenant_id: Uuid,
    pub source_invoice_id: String,
    pub source_invoice_item_ref: String,
    pub po_allocation_group: Option<String>,
    pub subscription_ref: Option<String>,
    pub revenue_stream: String,
    pub total_deferred: PostedMoney,
    pub policy_ref: String,
    pub ssp_snapshot_ref: Option<String>,
    pub vc_estimate_ref: Option<String>,
    pub vc_method_ref: Option<String>,
}

/// New planned segment with explicit validated currency metadata.
pub struct NewSegment {
    pub tenant_id: Uuid,
    pub schedule_id: String,
    pub segment_no: i32,
    pub period_id: String,
    pub amount: PostedMoney,
}

/// Replacement retains the same business-key dimensions and explicit existing lineage version.
pub struct ReplacementSchedule {
    pub tenant_id: Uuid,
    pub schedule_id: String,
    pub payer_tenant_id: Uuid,
    pub source_invoice_id: String,
    pub source_invoice_item_ref: String,
    pub po_allocation_group: Option<String>,
    pub subscription_ref: Option<String>,
    pub revenue_stream: String,
    pub total_deferred: PostedMoney,
    pub policy_ref: String,
    pub ssp_snapshot_ref: Option<String>,
    pub vc_estimate_ref: Option<String>,
    pub vc_method_ref: Option<String>,
    pub version: i64,
}

/// Advisory due money snapshot; never use it as authoritative posting-attempt state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DuePendingSegment {
    pub schedule_id: String,
    pub segment_no: i32,
    pub period_id: String,
    pub amount: PostedMoney,
    pub revenue_stream: String,
    pub total_deferred: PostedMoney,
    pub recognized: PostedMoney,
}

/// Net recognized revenue in one actual-period/stream/currency grain.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecognizedStreamEntry {
    pub period_id: String,
    pub revenue_stream: String,
    pub recognized: PostedMoney,
}

/// Scoped canonical schedule, segment, run, and recognition-report persistence.
#[derive(Clone)]
pub struct RecognitionRepo {
    db: DBProvider<DbError>,
}

impl RecognitionRepo {
    /// Bind backend-aware error classification to the repository provider.
    #[must_use]
    pub fn new(db: DBProvider<DbError>) -> Self {
        Self { db }
    }

    /// Insert a fresh ACTIVE schedule with canonical total, recognized zero, and version zero. Unique business-key collisions retain the existing storage-error behavior.
    ///
    /// # Errors
    /// [`RepoError::MoneyOutCapExceeded`] when `total_deferred` is negative; [`RepoError::Db`]
    /// on a scope or storage failure (an at-most-one-live collision included);
    /// [`RepoError::Conflict`] on classified database contention.
    pub async fn insert_schedule(
        &self,
        txn: &DbTx<'_>,
        scope: &AccessScope,
        schedule: &NewSchedule,
    ) -> Result<(), RepoError> {
        nonnegative(&schedule.total_deferred)?;
        let am = recognition_schedule::ActiveModel {
            tenant_id: Set(schedule.tenant_id),
            schedule_id: Set(schedule.schedule_id.clone()),
            payer_tenant_id: Set(schedule.payer_tenant_id),
            source_invoice_id: Set(schedule.source_invoice_id.clone()),
            source_invoice_item_ref: Set(schedule.source_invoice_item_ref.clone()),
            po_allocation_group: Set(schedule.po_allocation_group.clone()),
            subscription_ref: Set(schedule.subscription_ref.clone()),
            revenue_stream: Set(schedule.revenue_stream.clone()),
            currency: Set(schedule.total_deferred.currency().code().to_owned()),
            currency_scale: Set(i16::from(schedule.total_deferred.currency().scale())),
            total_deferred: Set(encode_amount(&schedule.total_deferred)),
            recognized: Set("0".to_owned()),
            policy_ref: Set(schedule.policy_ref.clone()),
            ssp_snapshot_ref: Set(schedule.ssp_snapshot_ref.clone()),
            vc_estimate_ref: Set(schedule.vc_estimate_ref.clone()),
            vc_method_ref: Set(schedule.vc_method_ref.clone()),
            status: Set(SCHEDULE_STATUS_ACTIVE.to_owned()),
            version: Set(0),
        };
        recognition_schedule::Entity::insert(am.clone())
            .secure()
            .scope_with_model(scope, &am)
            .map_err(|e| scope_to_repo(e, self.db.db().backend()))?
            .exec(txn)
            .await
            .map_err(|e| scope_to_repo(e, self.db.db().backend()))?;
        Ok(())
    }

    /// Insert immutable segment keys with version zero; validate each amount against its parent. The caller rolls back the entire batch on any error.
    ///
    /// # Errors
    /// [`RepoError::MoneyOutCapExceeded`] when a segment amount is negative;
    /// [`RepoError::RecognitionPolicyConflict`] when a segment's schedule is absent or
    /// inaccessible; [`RepoError::Money`] when a segment disagrees with its schedule's currency
    /// metadata; [`RepoError::Db`] on a scope or storage failure (a duplicate key included);
    /// [`RepoError::Conflict`] on classified database contention;
    /// [`RepoError::InvalidStoredMoney`] when the parent schedule row is malformed.
    pub async fn insert_segments(
        &self,
        txn: &DbTx<'_>,
        scope: &AccessScope,
        segments: &[NewSegment],
    ) -> Result<(), RepoError> {
        // A plan's segments share one schedule: read and validate each parent once.
        let mut parents: std::collections::HashMap<(Uuid, String), ScheduleState> =
            std::collections::HashMap::new();
        for seg in segments {
            nonnegative(&seg.amount)?;
            let parent = match parents.entry((seg.tenant_id, seg.schedule_id.clone())) {
                std::collections::hash_map::Entry::Occupied(entry) => entry.into_mut(),
                std::collections::hash_map::Entry::Vacant(entry) => entry.insert(
                    self.required_schedule(txn, scope, seg.tenant_id, &seg.schedule_id)
                        .await?,
                ),
            };
            same_spec(&seg.amount, &parent.total_deferred)?;
            let am = recognition_segment::ActiveModel {
                currency: Set(seg.amount.currency().code().to_owned()),
                currency_scale: Set(i16::from(seg.amount.currency().scale())),
                tenant_id: Set(seg.tenant_id),
                schedule_id: Set(seg.schedule_id.clone()),
                segment_no: Set(seg.segment_no),
                period_id: Set(seg.period_id.clone()),
                amount: Set(encode_amount(&seg.amount)),
                version: Set(0),
                status: Set(SEGMENT_STATUS_PENDING.to_owned()),
                recognized_at: Set(None),
                run_id: Set(None),
            };
            recognition_segment::Entity::insert(am.clone())
                .secure()
                .scope_with_model(scope, &am)
                .map_err(|e| scope_to_repo(e, self.db.db().backend()))?
                .exec(txn)
                .await
                .map_err(|e| scope_to_repo(e, self.db.db().backend()))?;
        }
        Ok(())
    }

    /// Scoped caller-runner lookup of a schedule with validated correlated amounts.
    ///
    /// # Errors
    /// [`RepoError::Db`] on a scope or storage failure; [`RepoError::Conflict`] on classified
    /// database contention. [`RepoError::InvalidStoredMoney`] when a stored amount is malformed
    /// or off its currency contract.
    pub async fn read_schedule_in_txn<R: DBRunner>(
        &self,
        txn: &R,
        scope: &AccessScope,
        tenant: Uuid,
        schedule_id: &str,
    ) -> Result<Option<ScheduleState>, RepoError> {
        let row = recognition_schedule::Entity::find()
            .secure()
            .scope_with(scope)
            .filter(
                Condition::all()
                    .add(recognition_schedule::Column::TenantId.eq(tenant))
                    .add(recognition_schedule::Column::ScheduleId.eq(schedule_id)),
            )
            .one(txn)
            .await
            .map_err(|e| scope_to_repo(e, self.db.db().backend()))?;
        row.map(decode_schedule).transpose()
    }

    /// Resolve the ACTIVE successor at the existing business key and exact lineage version.
    ///
    /// # Errors
    /// [`RepoError::Db`] on a scope or storage failure; [`RepoError::Conflict`] on classified
    /// database contention. [`RepoError::InvalidStoredMoney`] when a stored amount is malformed
    /// or off its currency contract.
    pub async fn read_active_successor_in_txn<R: DBRunner>(
        &self,
        txn: &R,
        scope: &AccessScope,
        tenant: Uuid,
        source_invoice_id: &str,
        source_invoice_item_ref: &str,
        revenue_stream: &str,
        version: i64,
    ) -> Result<Option<ScheduleState>, RepoError> {
        let row = recognition_schedule::Entity::find()
            .secure()
            .scope_with(scope)
            .filter(
                Condition::all()
                    .add(recognition_schedule::Column::TenantId.eq(tenant))
                    .add(recognition_schedule::Column::SourceInvoiceId.eq(source_invoice_id))
                    .add(
                        recognition_schedule::Column::SourceInvoiceItemRef
                            .eq(source_invoice_item_ref),
                    )
                    .add(recognition_schedule::Column::RevenueStream.eq(revenue_stream))
                    .add(recognition_schedule::Column::Version.eq(version))
                    .add(recognition_schedule::Column::Status.eq(SCHEDULE_STATUS_ACTIVE)),
            )
            .one(txn)
            .await
            .map_err(|e| scope_to_repo(e, self.db.db().backend()))?;
        row.map(decode_schedule).transpose()
    }

    /// Read the one ACTIVE schedule at its existing invoice/item/stream business key.
    ///
    /// # Errors
    /// [`RepoError::Db`] on a scope or storage failure; [`RepoError::Conflict`] on classified
    /// database contention. [`RepoError::InvalidStoredMoney`] when a stored amount is malformed
    /// or off its currency contract.
    pub async fn read_active_schedule_in_txn<R: DBRunner>(
        &self,
        txn: &R,
        scope: &AccessScope,
        tenant: Uuid,
        source_invoice_id: &str,
        source_invoice_item_ref: &str,
        revenue_stream: &str,
    ) -> Result<Option<ScheduleState>, RepoError> {
        let row = recognition_schedule::Entity::find()
            .secure()
            .scope_with(scope)
            .filter(
                Condition::all()
                    .add(recognition_schedule::Column::TenantId.eq(tenant))
                    .add(recognition_schedule::Column::SourceInvoiceId.eq(source_invoice_id))
                    .add(
                        recognition_schedule::Column::SourceInvoiceItemRef
                            .eq(source_invoice_item_ref),
                    )
                    .add(recognition_schedule::Column::RevenueStream.eq(revenue_stream))
                    .add(recognition_schedule::Column::Status.eq(SCHEDULE_STATUS_ACTIVE)),
            )
            .one(txn)
            .await
            .map_err(|e| scope_to_repo(e, self.db.db().backend()))?;
        row.map(decode_schedule).transpose()
    }

    /// Preserve the existing from-state no-op and single version increment.
    ///
    /// # Errors
    /// [`RepoError::RecognitionPolicyConflict`] when `to_status` is not a valid schedule
    /// status; [`RepoError::InvalidStoredMoney`] when the stored row is malformed or its
    /// version is negative / exhausted; [`RepoError::Conflict`] when the observed version is
    /// stale, or on classified database contention; [`RepoError::Db`] on a scope or storage
    /// failure.
    pub async fn mark_schedule_status(
        &self,
        txn: &DbTx<'_>,
        scope: &AccessScope,
        tenant: Uuid,
        schedule_id: &str,
        from_status: &str,
        to_status: &str,
    ) -> Result<u64, RepoError> {
        let Some(row) = self
            .read_schedule_in_txn(txn, scope, tenant, schedule_id)
            .await?
        else {
            return Ok(0);
        };
        if row.status != from_status {
            return Ok(0);
        }
        if !valid_schedule_status(to_status) {
            return Err(phase("invalid schedule target status"));
        }
        self.write_schedule(
            txn,
            scope,
            &row,
            &row.total_deferred,
            &row.recognized,
            to_status,
            next_version(row.version)?,
        )
        .await?;
        Ok(1)
    }

    /// Find the latest DONE planned period on the caller runner for the cross-version replacement floor.
    ///
    /// # Errors
    /// [`RepoError::Db`] on a scope or storage failure; [`RepoError::Conflict`] on classified
    /// database contention.
    pub async fn max_done_segment_period_in_txn<R: DBRunner>(
        &self,
        txn: &R,
        scope: &AccessScope,
        tenant: Uuid,
        schedule_id: &str,
    ) -> Result<Option<String>, RepoError> {
        let row = recognition_segment::Entity::find()
            .secure()
            .scope_with(scope)
            .filter(
                Condition::all()
                    .add(recognition_segment::Column::TenantId.eq(tenant))
                    .add(recognition_segment::Column::ScheduleId.eq(schedule_id))
                    .add(recognition_segment::Column::Status.eq(SEGMENT_STATUS_DONE)),
            )
            .order_by(recognition_segment::Column::PeriodId, Order::Desc)
            .one(txn)
            .await
            .map_err(|e| scope_to_repo(e, self.db.db().backend()))?;
        Ok(row.map(|r| r.period_id))
    }

    /// Read the DONE floor across all versions at the unchanged schedule business key.
    ///
    /// # Errors
    /// [`RepoError::Db`] on a scope or storage failure; [`RepoError::Conflict`] on classified
    /// database contention.
    pub async fn max_done_business_period_in_txn<R: DBRunner>(
        &self,
        txn: &R,
        scope: &AccessScope,
        schedule: &ScheduleState,
    ) -> Result<Option<String>, RepoError> {
        let versions = recognition_schedule::Entity::find()
            .secure()
            .scope_with(scope)
            .filter(
                Condition::all()
                    .add(recognition_schedule::Column::TenantId.eq(schedule.tenant_id))
                    .add(
                        recognition_schedule::Column::SourceInvoiceId
                            .eq(&schedule.source_invoice_id),
                    )
                    .add(
                        recognition_schedule::Column::SourceInvoiceItemRef
                            .eq(&schedule.source_invoice_item_ref),
                    )
                    .add(recognition_schedule::Column::RevenueStream.eq(&schedule.revenue_stream)),
            )
            .all(txn)
            .await
            .map_err(|e| scope_to_repo(e, self.db.db().backend()))?;
        let version_ids: Vec<String> = versions.into_iter().map(|v| v.schedule_id).collect();
        if version_ids.is_empty() {
            return Ok(None);
        }
        // One query for the latest DONE period across every version.
        let row = recognition_segment::Entity::find()
            .secure()
            .scope_with(scope)
            .filter(
                Condition::all()
                    .add(recognition_segment::Column::TenantId.eq(schedule.tenant_id))
                    .add(recognition_segment::Column::ScheduleId.is_in(version_ids))
                    .add(recognition_segment::Column::Status.eq(SEGMENT_STATUS_DONE)),
            )
            .order_by(recognition_segment::Column::PeriodId, Order::Desc)
            .one(txn)
            .await
            .map_err(|e| scope_to_repo(e, self.db.db().backend()))?;
        Ok(row.map(|r| r.period_id))
    }

    /// Count due unfinished segments in the period-close transaction.
    ///
    /// # Errors
    /// [`RepoError::Db`] on a scope or storage failure; [`RepoError::Conflict`] on classified
    /// database contention.
    pub async fn count_due_not_done_in_txn<R: DBRunner>(
        &self,
        txn: &R,
        scope: &AccessScope,
        tenant: Uuid,
        period_id: &str,
    ) -> Result<usize, RepoError> {
        let rows = recognition_segment::Entity::find()
            .secure()
            .scope_with(scope)
            .filter(
                Condition::all()
                    .add(recognition_segment::Column::TenantId.eq(tenant))
                    .add(recognition_segment::Column::PeriodId.lte(period_id))
                    .add(recognition_segment::Column::Status.ne(SEGMENT_STATUS_DONE)),
            )
            .all(txn)
            .await
            .map_err(|e| scope_to_repo(e, self.db.db().backend()))?;
        Ok(rows.len())
    }

    /// Insert an ACTIVE replacement using the explicit predecessor-derived version. Caller marks the predecessor REPLACED in the same transaction.
    ///
    /// # Errors
    /// [`RepoError::MoneyOutCapExceeded`] when `total_deferred` is negative;
    /// [`RepoError::InvalidStoredMoney`] when the replacement version is negative;
    /// [`RepoError::Db`] on a scope or storage failure (an at-most-one-live collision
    /// included); [`RepoError::Conflict`] on classified database contention.
    pub async fn insert_replacement_schedule(
        &self,
        txn: &DbTx<'_>,
        scope: &AccessScope,
        schedule: &ReplacementSchedule,
    ) -> Result<(), RepoError> {
        nonnegative(&schedule.total_deferred)?;
        if schedule.version < 0 {
            return Err(invalid("negative replacement version"));
        }
        let am = recognition_schedule::ActiveModel {
            tenant_id: Set(schedule.tenant_id),
            schedule_id: Set(schedule.schedule_id.clone()),
            payer_tenant_id: Set(schedule.payer_tenant_id),
            source_invoice_id: Set(schedule.source_invoice_id.clone()),
            source_invoice_item_ref: Set(schedule.source_invoice_item_ref.clone()),
            po_allocation_group: Set(schedule.po_allocation_group.clone()),
            subscription_ref: Set(schedule.subscription_ref.clone()),
            revenue_stream: Set(schedule.revenue_stream.clone()),
            currency: Set(schedule.total_deferred.currency().code().to_owned()),
            currency_scale: Set(i16::from(schedule.total_deferred.currency().scale())),
            total_deferred: Set(encode_amount(&schedule.total_deferred)),
            recognized: Set("0".to_owned()),
            policy_ref: Set(schedule.policy_ref.clone()),
            ssp_snapshot_ref: Set(schedule.ssp_snapshot_ref.clone()),
            vc_estimate_ref: Set(schedule.vc_estimate_ref.clone()),
            vc_method_ref: Set(schedule.vc_method_ref.clone()),
            status: Set(SCHEDULE_STATUS_ACTIVE.to_owned()),
            version: Set(schedule.version),
        };
        recognition_schedule::Entity::insert(am.clone())
            .secure()
            .scope_with_model(scope, &am)
            .map_err(|e| scope_to_repo(e, self.db.db().backend()))?
            .exec(txn)
            .await
            .map_err(|e| scope_to_repo(e, self.db.db().backend()))?;
        Ok(())
    }

    /// Apply an exact signed delta; validate stored invariants first and CAS observed state/version.
    ///
    /// # Errors
    /// [`RepoError::RecognitionPolicyConflict`] when the schedule is absent or inaccessible;
    /// [`RepoError::Money`] when `amount` disagrees with the stored currency metadata or the
    /// exact result leaves the money contract; [`RepoError::MoneyOutCapExceeded`] when the
    /// result would leave `recognized` outside `0..=total_deferred`; [`RepoError::Conflict`]
    /// when the observed version is stale, or on classified database contention;
    /// [`RepoError::Db`] on a scope or storage failure; [`RepoError::InvalidStoredMoney`] when
    /// the stored row is malformed.
    pub async fn add_recognized(
        &self,
        txn: &DbTx<'_>,
        scope: &AccessScope,
        tenant: Uuid,
        schedule_id: &str,
        amount: &PostedMoney,
    ) -> Result<(), RepoError> {
        self.change_schedule_money(
            txn,
            scope,
            tenant,
            schedule_id,
            amount,
            ScheduleDelta::Recognized,
        )
        .await
    }

    /// [`Self::add_recognized`] on a schedule the caller already read in this
    /// transaction: the same validation, with the CAS on the observed
    /// status, version, currency metadata and both amounts instead of a fresh
    /// read. Returns the schedule as written, so a release can complete it
    /// without reading it again.
    ///
    /// # Errors
    /// As [`Self::add_recognized`], except that an absent schedule cannot occur;
    /// a stale observation, or one that disagrees with the stored row, is
    /// [`RepoError::Conflict`].
    pub async fn add_recognized_to(
        &self,
        txn: &DbTx<'_>,
        scope: &AccessScope,
        schedule: &ScheduleState,
        amount: &PostedMoney,
    ) -> Result<ScheduleState, RepoError> {
        self.apply_schedule_delta(txn, scope, schedule, amount, ScheduleDelta::Recognized)
            .await
    }

    /// Apply an exact signed delta; validate stored invariants first and CAS observed state/version.
    ///
    /// # Errors
    /// [`RepoError::RecognitionPolicyConflict`] when the schedule is absent or inaccessible;
    /// [`RepoError::Money`] when `amount` disagrees with the stored currency metadata or the
    /// exact result leaves the money contract; [`RepoError::MoneyOutCapExceeded`] when the
    /// reduction would leave `recognized` outside `0..=total_deferred`; [`RepoError::Conflict`]
    /// when the observed version is stale, or on classified database contention;
    /// [`RepoError::Db`] on a scope or storage failure; [`RepoError::InvalidStoredMoney`] when
    /// the stored row is malformed.
    pub async fn reduce_deferred(
        &self,
        txn: &DbTx<'_>,
        scope: &AccessScope,
        tenant: Uuid,
        schedule_id: &str,
        amount: &PostedMoney,
    ) -> Result<(), RepoError> {
        self.change_schedule_money(
            txn,
            scope,
            tenant,
            schedule_id,
            amount,
            ScheduleDelta::Reduce,
        )
        .await
    }

    /// Apply an exact signed delta; validate stored invariants first and CAS observed state/version.
    ///
    /// # Errors
    /// [`RepoError::RecognitionPolicyConflict`] when the schedule is absent or inaccessible;
    /// [`RepoError::Money`] when `amount` disagrees with the stored currency metadata or the
    /// exact result leaves the money contract; [`RepoError::MoneyOutCapExceeded`] when the
    /// increase would leave `recognized` outside `0..=total_deferred`; [`RepoError::Conflict`]
    /// when the observed version is stale, or on classified database contention;
    /// [`RepoError::Db`] on a scope or storage failure; [`RepoError::InvalidStoredMoney`] when
    /// the stored row is malformed.
    pub async fn increase_total_deferred(
        &self,
        txn: &DbTx<'_>,
        scope: &AccessScope,
        tenant: Uuid,
        schedule_id: &str,
        amount: &PostedMoney,
    ) -> Result<(), RepoError> {
        self.change_schedule_money(
            txn,
            scope,
            tenant,
            schedule_id,
            amount,
            ScheduleDelta::Increase,
        )
        .await
    }

    /// Read typed segment money in segment-number order, validating each spec against its scoped parent.
    ///
    /// # Errors
    /// [`RepoError::Db`] on a scope or storage failure; [`RepoError::Conflict`] on classified
    /// database contention. [`RepoError::InvalidStoredMoney`] when a stored amount is malformed
    /// or off its currency contract.
    pub async fn list_segments_in_txn<R: DBRunner>(
        &self,
        txn: &R,
        scope: &AccessScope,
        tenant: Uuid,
        schedule_id: &str,
    ) -> Result<Vec<SegmentState>, RepoError> {
        let rows = recognition_segment::Entity::find()
            .secure()
            .scope_with(scope)
            .filter(
                Condition::all()
                    .add(recognition_segment::Column::TenantId.eq(tenant))
                    .add(recognition_segment::Column::ScheduleId.eq(schedule_id)),
            )
            .order_by(recognition_segment::Column::SegmentNo, Order::Asc)
            .all(txn)
            .await
            .map_err(|e| scope_to_repo(e, self.db.db().backend()))?;
        let parent = self
            .read_schedule_in_txn(txn, scope, tenant, schedule_id)
            .await?;
        rows.into_iter()
            .map(|row| decode_segment(row, parent.as_ref()))
            .collect()
    }

    /// Pending amount changes share the same version protocol as queue/release.
    ///
    /// # Errors
    /// [`RepoError::RecognitionPolicyConflict`] when the segment is absent or not PENDING;
    /// [`RepoError::Money`] when `amount` disagrees with the stored currency metadata or the
    /// exact result leaves the money contract; [`RepoError::MoneyOutCapExceeded`] when the sum
    /// would be negative; [`RepoError::Conflict`] when the observed version is stale, or on
    /// classified database contention; [`RepoError::Db`] on a scope or storage failure;
    /// [`RepoError::InvalidStoredMoney`] when the stored row is malformed.
    pub async fn add_pending_segment_amount(
        &self,
        txn: &DbTx<'_>,
        scope: &AccessScope,
        tenant: Uuid,
        schedule_id: &str,
        segment_no: i32,
        amount: &PostedMoney,
    ) -> Result<(), RepoError> {
        let row = self
            .required_segment(txn, scope, tenant, schedule_id, segment_no)
            .await?;
        if row.status != SEGMENT_STATUS_PENDING {
            return Err(phase("cannot extend released or queued segment"));
        }
        same_spec(&row.amount, amount)?;
        let sum = exact(&row.amount)
            .checked_add(&exact(amount))
            .map_err(exact_error)?;
        if sum.is_negative() {
            return Err(cap("negative segment amount"));
        }
        let sum = sum
            .into_posted_exact(row.amount.currency().clone())
            .map_err(exact_error)?;
        self.write_segment(
            txn,
            scope,
            &row,
            &sum,
            &row.status,
            row.recognized_at,
            row.run_id,
        )
        .await
    }

    /// Completion deliberately keeps the lineage/version; observed ACTIVE guards stale writers.
    ///
    /// # Errors
    /// [`RepoError::Conflict`] when the observed ACTIVE row changed underneath, or on
    /// classified database contention; [`RepoError::Db`] on a scope or storage failure;
    /// [`RepoError::InvalidStoredMoney`] when the stored row is malformed.
    pub async fn complete_schedule_if_drained(
        &self,
        txn: &DbTx<'_>,
        scope: &AccessScope,
        tenant: Uuid,
        schedule_id: &str,
    ) -> Result<bool, RepoError> {
        let Some(row) = self
            .read_schedule_in_txn(txn, scope, tenant, schedule_id)
            .await?
        else {
            return Ok(false);
        };
        self.complete_observed_schedule_if_drained(txn, scope, &row)
            .await
    }

    /// [`Self::complete_schedule_if_drained`] on a schedule the caller already
    /// read or wrote in this transaction (for example the state
    /// [`Self::add_recognized_to`] returns); the CAS guards the observed
    /// ACTIVE state, version, currency metadata and both amounts, so the
    /// drained decision taken on `row` holds for the stored row.
    ///
    /// # Errors
    /// [`RepoError::Conflict`] when the observed ACTIVE row changed underneath or `row`
    /// disagrees with it, or on classified database contention; [`RepoError::Db`] on a
    /// scope or storage failure.
    pub async fn complete_observed_schedule_if_drained(
        &self,
        txn: &DbTx<'_>,
        scope: &AccessScope,
        row: &ScheduleState,
    ) -> Result<bool, RepoError> {
        if row.status != SCHEDULE_STATUS_ACTIVE
            || row.recognized.amount() != row.total_deferred.amount()
        {
            return Ok(false);
        }
        self.write_schedule(
            txn,
            scope,
            row,
            &row.total_deferred,
            &row.recognized,
            SCHEDULE_STATUS_COMPLETED,
            row.version,
        )
        .await?;
        Ok(true)
    }

    /// Release an eligible segment using its observed state/version; no nested retry.
    ///
    /// # Errors
    /// [`RepoError::RecognitionPolicyConflict`] when the segment is absent, not PENDING/QUEUED,
    /// or an earlier segment is not DONE; [`RepoError::Conflict`] when the observed version is
    /// stale, or on classified database contention; [`RepoError::Db`] on a scope or storage
    /// failure; [`RepoError::InvalidStoredMoney`] when the stored row is malformed or its
    /// version is negative / exhausted.
    pub async fn stamp_segment_done(
        &self,
        txn: &DbTx<'_>,
        scope: &AccessScope,
        tenant: Uuid,
        schedule_id: &str,
        segment_no: i32,
        run_id: Uuid,
        recognized_at: OffsetDateTime,
    ) -> Result<(), RepoError> {
        let row = self
            .required_segment(txn, scope, tenant, schedule_id, segment_no)
            .await?;
        self.stamp_observed_segment_done(txn, scope, &row, run_id, recognized_at)
            .await
    }

    /// [`Self::stamp_segment_done`] on a segment the caller already read in this
    /// transaction: the same phase and predecessor checks, with the CAS on the
    /// observed status, version, period, currency metadata and amount instead of
    /// a fresh read.
    ///
    /// # Errors
    /// [`RepoError::RecognitionPolicyConflict`] when the segment is not PENDING/QUEUED or an
    /// earlier segment is not DONE; [`RepoError::Conflict`] when the observation is stale or
    /// disagrees with the stored row, or on classified database contention; [`RepoError::Db`] on a scope or storage failure;
    /// [`RepoError::InvalidStoredMoney`] when its version is negative / exhausted.
    pub async fn stamp_observed_segment_done(
        &self,
        txn: &DbTx<'_>,
        scope: &AccessScope,
        row: &SegmentState,
        run_id: Uuid,
        recognized_at: OffsetDateTime,
    ) -> Result<(), RepoError> {
        if !matches!(
            row.status.as_str(),
            SEGMENT_STATUS_PENDING | SEGMENT_STATUS_QUEUED
        ) {
            return Err(phase("segment is not pending or queued"));
        }
        if self
            .count_predecessors_not_done_in(
                txn,
                scope,
                row.tenant_id,
                &row.schedule_id,
                &row.period_id,
            )
            .await?
            != 0
        {
            return Err(phase("earlier segment is not done"));
        }
        self.write_segment(
            txn,
            scope,
            row,
            &row.amount,
            SEGMENT_STATUS_DONE,
            Some(recognized_at),
            Some(run_id),
        )
        .await
    }

    /// Standalone presentation lookup; financial decisions use the caller-runner twin.
    ///
    /// # Errors
    /// [`RepoError::Db`] when no connection can be acquired or on a scope / storage failure;
    /// [`RepoError::Conflict`] on classified database contention.
    /// [`RepoError::InvalidStoredMoney`] when a stored amount is malformed or off its currency
    /// contract.
    pub async fn read_schedule(
        &self,
        scope: &AccessScope,
        tenant: Uuid,
        schedule_id: &str,
    ) -> Result<Option<ScheduleState>, RepoError> {
        let conn = self
            .db
            .conn()
            .map_err(|e| db_to_repo(e, self.db.db().backend()))?;
        let row = recognition_schedule::Entity::find()
            .secure()
            .scope_with(scope)
            .filter(
                Condition::all()
                    .add(recognition_schedule::Column::TenantId.eq(tenant))
                    .add(recognition_schedule::Column::ScheduleId.eq(schedule_id)),
            )
            .one(&conn)
            .await
            .map_err(|e| scope_to_repo(e, self.db.db().backend()))?;
        row.map(decode_schedule).transpose()
    }

    /// Typed discovery list ordered by item reference, descending version, then schedule ID. Return at most 500 rows and a truncation flag.
    ///
    /// # Errors
    /// [`RepoError::Db`] when no connection can be acquired or on a scope / storage failure;
    /// [`RepoError::Conflict`] on classified database contention.
    /// [`RepoError::InvalidStoredMoney`] when a stored amount is malformed or off its currency
    /// contract.
    pub async fn list_schedules(
        &self,
        scope: &AccessScope,
        tenant: Uuid,
        invoice_id: Option<&str>,
        revenue_stream: Option<&str>,
    ) -> Result<(Vec<ScheduleState>, bool), RepoError> {
        const SCHEDULE_LIST_CAP: usize = 500;
        let conn = self
            .db
            .conn()
            .map_err(|e| db_to_repo(e, self.db.db().backend()))?;
        let mut predicate = Condition::all().add(recognition_schedule::Column::TenantId.eq(tenant));
        if let Some(invoice_id) = invoice_id {
            predicate = predicate.add(recognition_schedule::Column::SourceInvoiceId.eq(invoice_id));
        }
        if let Some(revenue_stream) = revenue_stream {
            predicate =
                predicate.add(recognition_schedule::Column::RevenueStream.eq(revenue_stream));
        }
        let rows = recognition_schedule::Entity::find()
            .secure()
            .scope_with(scope)
            .filter(predicate)
            .order_by(
                recognition_schedule::Column::SourceInvoiceItemRef,
                Order::Asc,
            )
            .order_by(recognition_schedule::Column::Version, Order::Desc)
            .order_by(recognition_schedule::Column::ScheduleId, Order::Asc)
            .limit(SCHEDULE_LIST_CAP as u64 + 1)
            .all(&conn)
            .await
            .map_err(|e| scope_to_repo(e, self.db.db().backend()))?;
        let mut rows = rows
            .into_iter()
            .map(decode_schedule)
            .collect::<Result<Vec<_>, _>>()?;
        let truncated = rows.len() > SCHEDULE_LIST_CAP;
        if truncated {
            rows.truncate(SCHEDULE_LIST_CAP);
            tracing::warn!(
                tenant = %tenant,
                cap = SCHEDULE_LIST_CAP,
                "recognition-schedule list hit the cap; result truncated (no pagination)"
            );
        }
        Ok((rows, truncated))
    }

    /// Standalone typed segment list, ordered by immutable segment number.
    ///
    /// # Errors
    /// [`RepoError::Db`] when no connection can be acquired or on a scope / storage failure;
    /// [`RepoError::Conflict`] on classified database contention.
    /// [`RepoError::InvalidStoredMoney`] when a stored amount is malformed or off its currency
    /// contract.
    pub async fn list_segments(
        &self,
        scope: &AccessScope,
        tenant: Uuid,
        schedule_id: &str,
    ) -> Result<Vec<SegmentState>, RepoError> {
        let conn = self
            .db
            .conn()
            .map_err(|e| db_to_repo(e, self.db.db().backend()))?;
        let rows = recognition_segment::Entity::find()
            .secure()
            .scope_with(scope)
            .filter(
                Condition::all()
                    .add(recognition_segment::Column::TenantId.eq(tenant))
                    .add(recognition_segment::Column::ScheduleId.eq(schedule_id)),
            )
            .order_by(recognition_segment::Column::SegmentNo, Order::Asc)
            .all(&conn)
            .await
            .map_err(|e| scope_to_repo(e, self.db.db().backend()))?;
        let parent = self
            .read_schedule_in_txn(&conn, scope, tenant, schedule_id)
            .await?;
        rows.into_iter()
            .map(|row| decode_segment(row, parent.as_ref()))
            .collect()
    }

    /// Advisory snapshot of PENDING/QUEUED segments of ACTIVE schedules through the target period. State-changing callers must reread on their attempt runner.
    ///
    /// # Errors
    /// [`RepoError::Db`] when no connection can be acquired or on a scope / storage failure;
    /// [`RepoError::Conflict`] on classified database contention.
    /// [`RepoError::InvalidStoredMoney`] when a stored amount is malformed or off its currency
    /// contract.
    pub async fn list_due_pending_segments(
        &self,
        scope: &AccessScope,
        tenant: Uuid,
        period_id: &str,
    ) -> Result<Vec<DuePendingSegment>, RepoError> {
        let conn = self
            .db
            .conn()
            .map_err(|e| db_to_repo(e, self.db.db().backend()))?;

        let segments = recognition_segment::Entity::find()
            .secure()
            .scope_with(scope)
            .filter(
                Condition::all()
                    .add(recognition_segment::Column::TenantId.eq(tenant))
                    .add(
                        recognition_segment::Column::Status
                            .is_in([SEGMENT_STATUS_PENDING, SEGMENT_STATUS_QUEUED]),
                    )
                    .add(recognition_segment::Column::PeriodId.lte(period_id)),
            )
            .order_by(recognition_segment::Column::ScheduleId, Order::Asc)
            .order_by(recognition_segment::Column::SegmentNo, Order::Asc)
            .all(&conn)
            .await
            .map_err(|e| scope_to_repo(e, self.db.db().backend()))?;

        let mut schedule_ctx: HashMap<String, Option<ScheduleState>> = HashMap::new();
        let mut due = Vec::with_capacity(segments.len());
        for seg in segments {
            if !schedule_ctx.contains_key(&seg.schedule_id) {
                let row = recognition_schedule::Entity::find()
                    .secure()
                    .scope_with(scope)
                    .filter(
                        Condition::all()
                            .add(recognition_schedule::Column::TenantId.eq(tenant))
                            .add(recognition_schedule::Column::ScheduleId.eq(&seg.schedule_id)),
                    )
                    .one(&conn)
                    .await
                    .map_err(|e| scope_to_repo(e, self.db.db().backend()))?;
                schedule_ctx.insert(
                    seg.schedule_id.clone(),
                    row.map(decode_schedule).transpose()?,
                );
            }
            let Some(schedule) = schedule_ctx.get(&seg.schedule_id).and_then(Option::as_ref) else {
                return Err(invalid("due recognition segment has no scoped parent"));
            };
            let seg = decode_segment(seg, Some(schedule))?;
            if schedule.status != SCHEDULE_STATUS_ACTIVE {
                continue;
            }
            due.push(DuePendingSegment {
                schedule_id: seg.schedule_id,
                segment_no: seg.segment_no,
                period_id: seg.period_id,
                amount: seg.amount,
                revenue_stream: schedule.revenue_stream.clone(),
                total_deferred: schedule.total_deferred.clone(),
                recognized: schedule.recognized.clone(),
            });
        }
        Ok(due)
    }

    /// Exact net REVENUE from RECOGNITION journal entries, grouped and ordered by actual posting period, stream, and currency.
    ///
    /// # Errors
    /// [`RepoError::Db`] when no connection can be acquired or on a scope / storage failure;
    /// [`RepoError::Conflict`] on classified database contention.
    /// [`RepoError::InvalidStoredMoney`] when a stored amount is malformed or off its currency
    /// contract. [`RepoError::Money`] when the exact per-stream fold leaves the money contract.
    pub async fn list_revenue_disaggregation(
        &self,
        scope: &AccessScope,
        tenant: Uuid,
        period_id: Option<&str>,
    ) -> Result<Vec<RecognizedStreamEntry>, RepoError> {
        let conn = self
            .db
            .conn()
            .map_err(|e| db_to_repo(e, self.db.db().backend()))?;

        let mut entry_filter = Condition::all()
            .add(journal_entry::Column::TenantId.eq(tenant))
            .add(journal_entry::Column::SourceDocType.eq(SourceDocType::Recognition.as_str()));
        if let Some(period_id) = period_id {
            entry_filter = entry_filter.add(journal_entry::Column::PeriodId.eq(period_id));
        }
        let entry_ids: Vec<Uuid> = journal_entry::Entity::find()
            .secure()
            .scope_with(scope)
            .filter(entry_filter)
            .all(&conn)
            .await
            .map_err(|e| scope_to_repo(e, self.db.db().backend()))?
            .into_iter()
            .map(|e| e.entry_id)
            .collect();
        if entry_ids.is_empty() {
            return Ok(Vec::new());
        }

        let lines = journal_line::Entity::find()
            .secure()
            .scope_with(scope)
            .filter(
                Condition::all()
                    .add(journal_line::Column::TenantId.eq(tenant))
                    .add(journal_line::Column::AccountClass.eq(AccountClass::Revenue.as_str()))
                    .add(journal_line::Column::EntryId.is_in(entry_ids)),
            )
            .all(&conn)
            .await
            .map_err(|e| scope_to_repo(e, self.db.db().backend()))?;

        fold_revenue(lines)
    }

    /// Bounded scheduler enumeration of distinct tenant/planned-period pairs for PENDING/QUEUED segments; this is not a monetary decision feed.
    ///
    /// # Errors
    /// [`RepoError::Db`] when no connection can be acquired or on a scope / storage failure;
    /// [`RepoError::Conflict`] on classified database contention.
    pub async fn list_due_tenant_periods(
        &self,
        limit: u64,
    ) -> Result<Vec<(Uuid, String)>, RepoError> {
        let conn = self
            .db
            .conn()
            .map_err(|e| db_to_repo(e, self.db.db().backend()))?;
        let segments = recognition_segment::Entity::find()
            .secure()
            .scope_with(&AccessScope::allow_all())
            .filter(
                Condition::all().add(
                    recognition_segment::Column::Status
                        .is_in([SEGMENT_STATUS_PENDING, SEGMENT_STATUS_QUEUED]),
                ),
            )
            .order_by(recognition_segment::Column::TenantId, Order::Asc)
            .order_by(recognition_segment::Column::PeriodId, Order::Asc)
            .limit(limit)
            .all(&conn)
            .await
            .map_err(|e| scope_to_repo(e, self.db.db().backend()))?;
        let mut seen: std::collections::BTreeSet<(Uuid, String)> =
            std::collections::BTreeSet::new();
        for seg in segments {
            seen.insert((seg.tenant_id, seg.period_id));
        }
        Ok(seen.into_iter().collect())
    }

    /// Advisory standalone predecessor lookup; release attempts use the runner twin.
    ///
    /// # Errors
    /// [`RepoError::Db`] when no connection can be acquired or on a scope / storage failure;
    /// [`RepoError::Conflict`] on classified database contention.
    pub async fn count_predecessors_not_done(
        &self,
        scope: &AccessScope,
        tenant: Uuid,
        schedule_id: &str,
        period_id: &str,
    ) -> Result<u64, RepoError> {
        let conn = self
            .db
            .conn()
            .map_err(|e| db_to_repo(e, self.db.db().backend()))?;
        self.count_predecessors_not_done_in(&conn, scope, tenant, schedule_id, period_id)
            .await
    }
    /// Read predecessor states in the same attempt as release.
    ///
    /// # Errors
    /// [`RepoError::Db`] on a scope or storage failure; [`RepoError::Conflict`] on classified
    /// database contention.
    pub async fn count_predecessors_not_done_in<R: DBRunner>(
        &self,
        runner: &R,
        scope: &AccessScope,
        tenant: Uuid,
        schedule_id: &str,
        period_id: &str,
    ) -> Result<u64, RepoError> {
        let count = recognition_segment::Entity::find()
            .secure()
            .scope_with(scope)
            .filter(
                Condition::all()
                    .add(recognition_segment::Column::TenantId.eq(tenant))
                    .add(recognition_segment::Column::ScheduleId.eq(schedule_id))
                    .add(recognition_segment::Column::PeriodId.lt(period_id))
                    .add(recognition_segment::Column::Status.ne(SEGMENT_STATUS_DONE)),
            )
            .count(runner)
            .await
            .map_err(|e| scope_to_repo(e, self.db.db().backend()))?;
        Ok(count)
    }

    /// Standalone queue uses one transaction, with no repository retry.
    ///
    /// # Errors
    /// [`RepoError::Db`] when the serializable transaction cannot be opened or committed, or on
    /// a scope / storage failure; [`RepoError::Conflict`] when the observed version is stale,
    /// or on classified database contention; [`RepoError::InvalidStoredMoney`] when the stored
    /// segment is malformed or its version is negative / exhausted.
    pub async fn mark_segment_queued(
        &self,
        scope: &AccessScope,
        tenant: Uuid,
        schedule_id: &str,
        segment_no: i32,
    ) -> Result<(), RepoError> {
        let repo = self.clone();
        let scope = scope.clone();
        let schedule_id = schedule_id.to_owned();
        self.db
            .db()
            .transaction_ref_mapped_with_config(
                toolkit_db::secure::TxConfig::serializable(),
                move |txn| {
                    Box::pin(async move {
                        repo.mark_segment_queued_in(txn, &scope, tenant, &schedule_id, segment_no)
                            .await
                            .map_err(QueueAttemptError::Repository)
                    })
                },
            )
            .await
            .map_err(|e| match e {
                QueueAttemptError::Repository(e) => e,
                QueueAttemptError::Database(e) => db_to_repo(e, self.db.db().backend()),
            })
    }

    /// Queue within the caller attempt; absent/nonpending rows keep the existing no-op.
    ///
    /// # Errors
    /// [`RepoError::Db`] on a scope or storage failure; [`RepoError::Conflict`] when the
    /// observed version is stale, or on classified database contention;
    /// [`RepoError::InvalidStoredMoney`] when the stored segment is malformed or its version is
    /// negative / exhausted.
    pub async fn mark_segment_queued_in(
        &self,
        txn: &DbTx<'_>,
        scope: &AccessScope,
        tenant: Uuid,
        schedule_id: &str,
        segment_no: i32,
    ) -> Result<(), RepoError> {
        let Some(row) = self
            .read_segment_in(txn, scope, tenant, schedule_id, segment_no)
            .await?
        else {
            return Ok(());
        };
        if row.status != SEGMENT_STATUS_PENDING {
            return Ok(());
        }
        self.write_segment(
            txn,
            scope,
            &row,
            &row.amount,
            SEGMENT_STATUS_QUEUED,
            None,
            None,
        )
        .await
    }

    /// Read a tenant/period/run orchestration record under the supplied scope.
    ///
    /// # Errors
    /// [`RepoError::Db`] when no connection can be acquired or on a scope / storage failure;
    /// [`RepoError::Conflict`] on classified database contention.
    pub async fn read_run(
        &self,
        scope: &AccessScope,
        tenant: Uuid,
        period_id: &str,
        run_id: Uuid,
    ) -> Result<Option<recognition_run::Model>, RepoError> {
        let conn = self
            .db
            .conn()
            .map_err(|e| db_to_repo(e, self.db.db().backend()))?;
        let row = recognition_run::Entity::find()
            .secure()
            .scope_with(scope)
            .filter(
                Condition::all()
                    .add(recognition_run::Column::TenantId.eq(tenant))
                    .add(recognition_run::Column::PeriodId.eq(period_id))
                    .add(recognition_run::Column::RunId.eq(run_id)),
            )
            .one(&conn)
            .await
            .map_err(|e| scope_to_repo(e, self.db.db().backend()))?;
        Ok(row)
    }

    /// Read a run by tenant and run ID; existing first-match semantics across periods remain.
    ///
    /// # Errors
    /// [`RepoError::Db`] when no connection can be acquired or on a scope / storage failure;
    /// [`RepoError::Conflict`] on classified database contention.
    pub async fn read_run_out_of_txn(
        &self,
        scope: &AccessScope,
        tenant: Uuid,
        run_id: Uuid,
    ) -> Result<Option<recognition_run::Model>, RepoError> {
        let conn = self
            .db
            .conn()
            .map_err(|e| db_to_repo(e, self.db.db().backend()))?;
        recognition_run::Entity::find()
            .secure()
            .scope_with(scope)
            .filter(
                Condition::all()
                    .add(recognition_run::Column::TenantId.eq(tenant))
                    .add(recognition_run::Column::RunId.eq(run_id)),
            )
            .one(&conn)
            .await
            .map_err(|e| scope_to_repo(e, self.db.db().backend()))
    }

    /// Preserve scoped OData pagination, filtering, and default run-ID ordering for nonmonetary orchestration records.
    ///
    /// # Errors
    /// [`OdataPageError::Db`] on a storage / connection failure; [`OdataPageError::Odata`] on a
    /// malformed `$filter` / `$orderby` / cursor (the caller projects it to a canonical 400).
    pub async fn list_runs(
        &self,
        scope: &AccessScope,
        tenant: Uuid,
        query: &ODataQuery,
    ) -> Result<Page<recognition_run::Model>, OdataPageError> {
        let conn = self
            .db
            .conn()
            .map_err(|e| OdataPageError::Db(format!("conn: {e}")))?;
        let base_select = recognition_run::Entity::find()
            .secure()
            .scope_with(scope)
            .filter(Condition::all().add(recognition_run::Column::TenantId.eq(tenant)));
        let query = query_with_default_order(query, "run_id");
        paginate_odata::<
            RecognitionRunFilterField,
            RecognitionRunODataMapper,
            recognition_run::Entity,
            recognition_run::Model,
            _,
            _,
        >(
            base_select,
            &conn,
            &query,
            ("run_id", SortDir::Asc),
            LimitCfg {
                default: 25,
                max: 200,
            },
            |m| m,
        )
        .await
        .map_err(map_odata_err)
    }

    /// Insert a RUNNING orchestration record, retaining existing uniqueness semantics.
    ///
    /// # Errors
    /// [`RepoError::Db`] when no connection can be acquired, on a scope / storage failure, or
    /// on a duplicate run identity; [`RepoError::Conflict`] on classified database contention.
    pub async fn insert_run(
        &self,
        scope: &AccessScope,
        tenant: Uuid,
        run_id: Uuid,
        period_id: &str,
        started_at_utc: OffsetDateTime,
    ) -> Result<(), RepoError> {
        let conn = self
            .db
            .conn()
            .map_err(|e| db_to_repo(e, self.db.db().backend()))?;
        let am = recognition_run::ActiveModel {
            tenant_id: Set(tenant),
            period_id: Set(period_id.to_owned()),
            run_id: Set(run_id),
            started_at_utc: Set(started_at_utc),
            status: Set(RUN_STATUS_RUNNING.to_owned()),
        };
        recognition_run::Entity::insert(am.clone())
            .secure()
            .scope_with_model(scope, &am)
            .map_err(|e| scope_to_repo(e, self.db.db().backend()))?
            .exec(&conn)
            .await
            .map_err(|e| scope_to_repo(e, self.db.db().backend()))?;
        Ok(())
    }

    /// Finish only the matching RUNNING tenant/period/run row; repeated finish remains a no-op.
    ///
    /// # Errors
    /// [`RepoError::Db`] when no connection can be acquired or on a scope / storage failure;
    /// [`RepoError::Conflict`] on classified database contention.
    pub async fn finish_run(
        &self,
        scope: &AccessScope,
        tenant: Uuid,
        period_id: &str,
        run_id: Uuid,
        done: bool,
    ) -> Result<(), RepoError> {
        let conn = self
            .db
            .conn()
            .map_err(|e| db_to_repo(e, self.db.db().backend()))?;
        let next = if done {
            RUN_STATUS_DONE
        } else {
            RUN_STATUS_FAILED
        };
        recognition_run::Entity::update_many()
            .secure()
            .scope_with(scope)
            .col_expr(
                recognition_run::Column::Status,
                Expr::value(next.to_owned()),
            )
            .filter(
                Condition::all()
                    .add(recognition_run::Column::TenantId.eq(tenant))
                    .add(recognition_run::Column::PeriodId.eq(period_id))
                    .add(recognition_run::Column::RunId.eq(run_id))
                    .add(recognition_run::Column::Status.eq(RUN_STATUS_RUNNING)),
            )
            .exec(&conn)
            .await
            .map_err(|e| scope_to_repo(e, self.db.db().backend()))?;
        Ok(())
    }

    /// Return the earliest OPEN fiscal period for the tenant legal entity.
    ///
    /// # Errors
    /// [`RepoError::Db`] when no connection can be acquired or on a scope / storage failure;
    /// [`RepoError::Conflict`] on classified database contention.
    pub async fn current_open_period(
        &self,
        scope: &AccessScope,
        tenant: Uuid,
    ) -> Result<Option<String>, RepoError> {
        let conn = self
            .db
            .conn()
            .map_err(|e| db_to_repo(e, self.db.db().backend()))?;
        self.current_open_period_in(&conn, scope, tenant).await
    }

    /// Resolve missed-close reassignment in the release attempt's snapshot.
    ///
    /// # Errors
    /// [`RepoError::Db`] on a scope or storage failure; [`RepoError::Conflict`] on classified
    /// database contention.
    pub async fn current_open_period_in<R: DBRunner>(
        &self,
        runner: &R,
        scope: &AccessScope,
        tenant: Uuid,
    ) -> Result<Option<String>, RepoError> {
        let row = fiscal_period::Entity::find()
            .secure()
            .scope_with(scope)
            .filter(
                Condition::all()
                    .add(fiscal_period::Column::TenantId.eq(tenant))
                    .add(fiscal_period::Column::LegalEntityId.eq(tenant))
                    .add(fiscal_period::Column::Status.eq(PERIOD_STATUS_OPEN)),
            )
            .order_by(fiscal_period::Column::PeriodId, Order::Asc)
            .one(runner)
            .await
            .map_err(|e| scope_to_repo(e, self.db.db().backend()))?;
        Ok(row.map(|p| p.period_id))
    }
}

impl RecognitionRepo {
    /// Require a scoped schedule without leaking foreign-tenant existence.
    async fn required_schedule<R: DBRunner>(
        &self,
        runner: &R,
        scope: &AccessScope,
        tenant: Uuid,
        id: &str,
    ) -> Result<ScheduleState, RepoError> {
        self.read_schedule_in_txn(runner, scope, tenant, id)
            .await?
            .ok_or_else(|| phase("schedule absent or inaccessible"))
    }

    /// Scoped attempt-local segment read; parent metadata is part of validation.
    ///
    /// # Errors
    /// [`RepoError::Db`] on a scope or storage failure; [`RepoError::Conflict`] on classified
    /// database contention. [`RepoError::InvalidStoredMoney`] when a stored amount is malformed
    /// or off its currency contract.
    pub async fn read_segment_in<R: DBRunner>(
        &self,
        runner: &R,
        scope: &AccessScope,
        tenant: Uuid,
        schedule_id: &str,
        segment_no: i32,
    ) -> Result<Option<SegmentState>, RepoError> {
        let Some(row) = self
            .find_segment_row(runner, scope, tenant, schedule_id, segment_no)
            .await?
        else {
            return Ok(None);
        };
        let parent = self
            .read_schedule_in_txn(runner, scope, tenant, schedule_id)
            .await?;
        decode_segment(row, parent.as_ref()).map(Some)
    }

    /// [`Self::read_segment_in`] of a segment of `parent`, a schedule the caller
    /// already read on this runner: the segment is validated against `parent`
    /// instead of reading the schedule row again.
    ///
    /// # Errors
    /// [`RepoError::Db`] on a scope or storage failure; [`RepoError::Conflict`] on classified
    /// database contention. [`RepoError::InvalidStoredMoney`] when a stored amount is malformed
    /// or off its parent's currency contract.
    pub async fn read_segment_of<R: DBRunner>(
        &self,
        runner: &R,
        scope: &AccessScope,
        parent: &ScheduleState,
        segment_no: i32,
    ) -> Result<Option<SegmentState>, RepoError> {
        self.find_segment_row(
            runner,
            scope,
            parent.tenant_id,
            &parent.schedule_id,
            segment_no,
        )
        .await?
        .map(|row| decode_segment(row, Some(parent)))
        .transpose()
    }

    /// The scoped stored segment row, undecoded.
    async fn find_segment_row<R: DBRunner>(
        &self,
        runner: &R,
        scope: &AccessScope,
        tenant: Uuid,
        schedule_id: &str,
        segment_no: i32,
    ) -> Result<Option<recognition_segment::Model>, RepoError> {
        recognition_segment::Entity::find()
            .secure()
            .scope_with(scope)
            .filter(
                Condition::all()
                    .add(recognition_segment::Column::TenantId.eq(tenant))
                    .add(recognition_segment::Column::ScheduleId.eq(schedule_id))
                    .add(recognition_segment::Column::SegmentNo.eq(segment_no)),
            )
            .one(runner)
            .await
            .map_err(|e| scope_to_repo(e, self.db.db().backend()))
    }

    /// Require a scoped segment for an amount or release operation.
    async fn required_segment<R: DBRunner>(
        &self,
        runner: &R,
        scope: &AccessScope,
        tenant: Uuid,
        id: &str,
        segment_no: i32,
    ) -> Result<SegmentState, RepoError> {
        self.read_segment_in(runner, scope, tenant, id, segment_no)
            .await?
            .ok_or_else(|| phase("segment absent or inaccessible"))
    }

    /// Validate all correlated money before narrowing the changed value once.
    async fn change_schedule_money(
        &self,
        txn: &DbTx<'_>,
        scope: &AccessScope,
        tenant: Uuid,
        id: &str,
        delta: &PostedMoney,
        kind: ScheduleDelta,
    ) -> Result<(), RepoError> {
        let row = self.required_schedule(txn, scope, tenant, id).await?;
        self.apply_schedule_delta(txn, scope, &row, delta, kind)
            .await
            .map(drop)
    }

    /// Apply a delta to an observed schedule: validate the correlated money,
    /// narrow once, CAS on the observed state/version, and return the row as
    /// written.
    async fn apply_schedule_delta(
        &self,
        txn: &DbTx<'_>,
        scope: &AccessScope,
        row: &ScheduleState,
        delta: &PostedMoney,
        kind: ScheduleDelta,
    ) -> Result<ScheduleState, RepoError> {
        same_spec(&row.total_deferred, delta)?;
        let total = exact(&row.total_deferred);
        let recognized = exact(&row.recognized);
        let (total, recognized) = match kind {
            ScheduleDelta::Recognized => (
                total,
                recognized.checked_add(&exact(delta)).map_err(exact_error)?,
            ),
            ScheduleDelta::Reduce => (
                total.checked_sub(&exact(delta)).map_err(exact_error)?,
                recognized,
            ),
            ScheduleDelta::Increase => (
                total.checked_add(&exact(delta)).map_err(exact_error)?,
                recognized,
            ),
        };
        if total.is_negative()
            || recognized.is_negative()
            || total
                .checked_sub(&recognized)
                .map_err(exact_error)?
                .is_negative()
        {
            return Err(cap("recognized must be between zero and total deferred"));
        }
        let spec = row.total_deferred.currency();
        let total = total.into_posted_exact(spec.clone()).map_err(exact_error)?;
        let recognized = recognized
            .into_posted_exact(spec.clone())
            .map_err(exact_error)?;
        let version = next_version(row.version)?;
        self.write_schedule(txn, scope, row, &total, &recognized, &row.status, version)
            .await?;
        Ok(ScheduleState {
            total_deferred: total,
            recognized,
            version,
            ..row.clone()
        })
    }

    /// One literal schedule CAS, including status because completion keeps the version.
    ///
    /// The predicate also pins the observed currency metadata and both stored
    /// amounts, so an observation that disagrees with the row at the same
    /// version (a caller-edited [`ScheduleState`]) is a conflict, never a write.
    async fn write_schedule(
        &self,
        txn: &DbTx<'_>,
        scope: &AccessScope,
        old: &ScheduleState,
        total: &PostedMoney,
        recognized: &PostedMoney,
        status: &str,
        version: i64,
    ) -> Result<(), RepoError> {
        let result = recognition_schedule::Entity::update_many()
            .secure()
            .scope_with(scope)
            .col_expr(
                recognition_schedule::Column::TotalDeferred,
                Expr::value(encode_amount(total)),
            )
            .col_expr(
                recognition_schedule::Column::Recognized,
                Expr::value(encode_amount(recognized)),
            )
            .col_expr(
                recognition_schedule::Column::Status,
                Expr::value(status.to_owned()),
            )
            .col_expr(recognition_schedule::Column::Version, Expr::value(version))
            .filter(
                Condition::all()
                    .add(recognition_schedule::Column::TenantId.eq(old.tenant_id))
                    .add(recognition_schedule::Column::ScheduleId.eq(&old.schedule_id))
                    .add(recognition_schedule::Column::Version.eq(old.version))
                    .add(recognition_schedule::Column::Status.eq(&old.status))
                    .add(
                        recognition_schedule::Column::Currency
                            .eq(old.total_deferred.currency().code()),
                    )
                    .add(
                        recognition_schedule::Column::CurrencyScale
                            .eq(i16::from(old.total_deferred.currency().scale())),
                    )
                    .add(
                        recognition_schedule::Column::TotalDeferred
                            .eq(encode_amount(&old.total_deferred)),
                    )
                    .add(
                        recognition_schedule::Column::Recognized.eq(encode_amount(&old.recognized)),
                    ),
            )
            .exec(txn)
            .await
            .map_err(|e| scope_to_repo(e, self.db.db().backend()))?;
        cas(result.rows_affected)
    }

    /// Shared amount/state CAS. Queue and release cannot silently overwrite pending money:
    /// the predicate pins the observed period, currency metadata and stored amount as
    /// well as version and status, so a caller-edited [`SegmentState`] is a conflict.
    async fn write_segment(
        &self,
        txn: &DbTx<'_>,
        scope: &AccessScope,
        old: &SegmentState,
        amount: &PostedMoney,
        status: &str,
        recognized_at: Option<OffsetDateTime>,
        run_id: Option<Uuid>,
    ) -> Result<(), RepoError> {
        let version = next_version(old.version)?;
        let result = recognition_segment::Entity::update_many()
            .secure()
            .scope_with(scope)
            .col_expr(
                recognition_segment::Column::Amount,
                Expr::value(encode_amount(amount)),
            )
            .col_expr(
                recognition_segment::Column::Status,
                Expr::value(status.to_owned()),
            )
            .col_expr(recognition_segment::Column::Version, Expr::value(version))
            .col_expr(
                recognition_segment::Column::RecognizedAt,
                Expr::value(recognized_at),
            )
            .col_expr(recognition_segment::Column::RunId, Expr::value(run_id))
            .filter(
                Condition::all()
                    .add(recognition_segment::Column::TenantId.eq(old.tenant_id))
                    .add(recognition_segment::Column::ScheduleId.eq(&old.schedule_id))
                    .add(recognition_segment::Column::SegmentNo.eq(old.segment_no))
                    .add(recognition_segment::Column::Version.eq(old.version))
                    .add(recognition_segment::Column::Status.eq(&old.status))
                    .add(recognition_segment::Column::PeriodId.eq(&old.period_id))
                    .add(recognition_segment::Column::Currency.eq(old.amount.currency().code()))
                    .add(
                        recognition_segment::Column::CurrencyScale
                            .eq(i16::from(old.amount.currency().scale())),
                    )
                    .add(recognition_segment::Column::Amount.eq(encode_amount(&old.amount))),
            )
            .exec(txn)
            .await
            .map_err(|e| scope_to_repo(e, self.db.db().backend()))?;
        cas(result.rows_affected)
    }
}

/// Existing schedule delta semantics, including signed reversals.
enum ScheduleDelta {
    Recognized,
    Reduce,
    Increase,
}
/// A stale observation always aborts the whole caller attempt.
fn cas(rows: u64) -> Result<(), RepoError> {
    if rows == 1 {
        Ok(())
    } else {
        Err(RepoError::Conflict("recognition row changed".into()))
    }
}
/// Exhausted/negative versions are storage faults, never retryable contention.
fn next_version(version: i64) -> Result<i64, RepoError> {
    if version < 0 {
        return Err(invalid("negative recognition version"));
    }
    version
        .checked_add(1)
        .ok_or_else(|| invalid("recognition version exhausted"))
}
/// Exact arithmetic over posted values without intermediate carrier bounds.
fn exact(value: &PostedMoney) -> ExactAmount {
    ExactAmount::from_decimal(value.amount())
}
/// Preserve numeric rejection separately from stored corruption.
fn exact_error(error: ExactError) -> RepoError {
    match error {
        ExactError::Money(e) => RepoError::Money(e),
        other => RepoError::Db(other.to_string()),
    }
}
/// Input metadata must match; scale is never an identity dimension.
fn same_spec(a: &PostedMoney, b: &PostedMoney) -> Result<(), RepoError> {
    Ok(a.currency().ensure_same(b.currency())?)
}
/// Creation permits zero and forbids negative schedule/segment amounts.
fn nonnegative(value: &PostedMoney) -> Result<(), RepoError> {
    if value.amount() < Decimal::ZERO {
        Err(cap("negative recognition amount"))
    } else {
        Ok(())
    }
}
/// Existing recognition cap rejection is refined to OverRecognition by its adapter.
fn cap(detail: &str) -> RepoError {
    RepoError::MoneyOutCapExceeded(detail.to_owned())
}
/// Valid stored phase rejecting an operation is a business error, not contention.
fn phase(detail: &str) -> RepoError {
    RepoError::RecognitionPolicyConflict(detail.to_owned())
}
/// Corrupt stored values fail closed before any operation can repair them.
fn invalid(detail: &str) -> RepoError {
    RepoError::InvalidStoredMoney(detail.to_owned())
}
/// Closed set already enforced by the fresh schema.
fn valid_schedule_status(status: &str) -> bool {
    matches!(
        status,
        SCHEDULE_STATUS_ACTIVE
            | SCHEDULE_STATUS_COMPLETED
            | SCHEDULE_STATUS_REPLACED
            | SCHEDULE_STATUS_CANCELLED
    )
}

/// Decode and validate the complete correlated schedule state.
fn decode_schedule(row: recognition_schedule::Model) -> Result<ScheduleState, RepoError> {
    let total_deferred = decode_money(&row.total_deferred, &row.currency, row.currency_scale)?;
    let recognized = decode_money(&row.recognized, &row.currency, row.currency_scale)?;
    if total_deferred.amount() < Decimal::ZERO
        || recognized.amount() < Decimal::ZERO
        || recognized.amount() > total_deferred.amount()
        || row.version < 0
        || !valid_schedule_status(&row.status)
    {
        return Err(invalid("invalid stored recognition schedule invariants"));
    }
    Ok(ScheduleState {
        tenant_id: row.tenant_id,
        schedule_id: row.schedule_id,
        payer_tenant_id: row.payer_tenant_id,
        source_invoice_id: row.source_invoice_id,
        source_invoice_item_ref: row.source_invoice_item_ref,
        po_allocation_group: row.po_allocation_group,
        subscription_ref: row.subscription_ref,
        revenue_stream: row.revenue_stream,
        total_deferred,
        recognized,
        policy_ref: row.policy_ref,
        ssp_snapshot_ref: row.ssp_snapshot_ref,
        vc_estimate_ref: row.vc_estimate_ref,
        vc_method_ref: row.vc_method_ref,
        status: row.status,
        version: row.version,
    })
}

/// Decode each segment's own metadata and validate it against its scoped parent.
fn decode_segment(
    row: recognition_segment::Model,
    parent: Option<&ScheduleState>,
) -> Result<SegmentState, RepoError> {
    let amount = decode_money(&row.amount, &row.currency, row.currency_scale)?;
    let parent = parent.ok_or_else(|| invalid("recognition segment has no scoped parent"))?;
    if amount.currency() != parent.total_deferred.currency()
        || amount.amount() < Decimal::ZERO
        || row.version < 0
        || !matches!(
            row.status.as_str(),
            SEGMENT_STATUS_PENDING | SEGMENT_STATUS_QUEUED | SEGMENT_STATUS_DONE
        )
    {
        return Err(invalid(
            "invalid stored recognition segment invariants or parent spec",
        ));
    }
    Ok(SegmentState {
        tenant_id: row.tenant_id,
        schedule_id: row.schedule_id,
        segment_no: row.segment_no,
        period_id: row.period_id,
        amount,
        version: row.version,
        status: row.status,
        recognized_at: row.recognized_at,
        run_id: row.run_id,
    })
}

/// Exact per-(stream, currency, account) revenue accumulator keyed for stable output order.
type RevenueFold =
    std::collections::BTreeMap<(String, String, String), (CurrencySpec, ExactAmount)>;

/// Sum every signed contribution exactly before a single bounded result per currency grain.
fn fold_revenue(lines: Vec<journal_line::Model>) -> Result<Vec<RecognizedStreamEntry>, RepoError> {
    let mut grouped = RevenueFold::new();
    for line in lines {
        let decoded = super::journal_repo::decode_line(&line)?;
        let money = decoded.money;
        if money.amount() < Decimal::ZERO
            || decoded
                .functional_money
                .as_ref()
                .is_some_and(|v| v.amount() < Decimal::ZERO)
        {
            return Err(invalid("negative stored recognized revenue line"));
        }
        let Some(stream) = line.revenue_stream else {
            continue;
        };
        let signed = match line.side.as_str() {
            s if s == Side::Credit.as_str() => exact(&money),
            s if s == Side::Debit.as_str() => ExactAmount::from_decimal(Decimal::ZERO)
                .checked_sub(&exact(&money))
                .map_err(exact_error)?,
            _ => return Err(invalid("invalid recognized revenue side")),
        };
        let entry = grouped
            .entry((line.period_id, stream, money.currency().code().to_owned()))
            .or_insert_with(|| {
                (
                    money.currency().clone(),
                    ExactAmount::from_decimal(Decimal::ZERO),
                )
            });
        if &entry.0 != money.currency() {
            return Err(invalid("conflicting stored recognition revenue scale"));
        }
        entry.1 = entry.1.checked_add(&signed).map_err(exact_error)?;
    }
    grouped
        .into_iter()
        .map(|((period_id, revenue_stream, _), (spec, amount))| {
            Ok(RecognizedStreamEntry {
                period_id,
                revenue_stream,
                recognized: amount.into_posted_exact(spec).map_err(exact_error)?,
            })
        })
        .collect()
}

#[cfg(test)]
#[path = "recognition_repo_tests.rs"]
mod tests;

/// Preserve both infrastructure and repository failures across standalone queue transaction rollback.
#[derive(Debug)]
enum QueueAttemptError {
    Repository(RepoError),
    Database(DbError),
}
impl From<DbError> for QueueAttemptError {
    fn from(error: DbError) -> Self {
        Self::Database(error)
    }
}
