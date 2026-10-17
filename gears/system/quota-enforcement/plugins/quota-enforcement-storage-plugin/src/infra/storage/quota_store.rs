//! [`SqlQuotaStore`]: the `toolkit-db` adapter behind [`QuotaStore`].
//!
//! Every mutation is one transaction: the Quota row, its counter row, one
//! operation-log row, and the notification events (I1, I11). Update and
//! deactivate lock the Quota row first, then its counter rows (ADR-0002 lock
//! order), decide on the merged row inside the lock (I6, I14), and write by
//! compare-and-set on the record version, so a row that moved under the lock
//! is reported, never overwritten. Reads run on a plain connection: the
//! keyset page re-applies the caller's scope on every request, and the two
//! platform-plane aggregates run unscoped, as the contract states.
//!
//! Seams for later features are marked `2.5` (consumption counters in the cap
//! guard) and `2.6` (the lease cascade in deactivation).

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use async_trait::async_trait;
use quota_enforcement_sdk::{
    ActiveQuotaCounts, DeactivateOutcome, EventId, LeaseToken, MetricId, NotificationEvent,
    NotificationEventKind, NotificationScope, PageRequest, PageResult, ProjectionBinding, Quota,
    QuotaDraft, QuotaFilter, QuotaId, QuotaPatch, QuotaType, TenantId,
};
use time::OffsetDateTime;
use toolkit_db::secure::{DBRunner, ScopeError, validate_tenant_in_scope};
use toolkit_db::{Db, DbError};
use toolkit_security::AccessScope;
use uuid::Uuid;

use crate::domain::ports::{Actor, QuotaStore, StoreError};
use crate::infra::outbox::{EnqueueError, NotificationEnqueuer};
use crate::infra::storage::cursor::{self, MAX_FILTER_IDS};
use crate::infra::storage::entity::quota;
use crate::infra::storage::quota_mapping::{
    self, MappingError, QuotaUpdate, STATUS_ACTIVE, status_name,
};
use crate::infra::storage::repo::operation_log_repo::{
    self, Entry, OP_QUOTA_CREATE, OP_QUOTA_DEACTIVATE, OP_QUOTA_UPDATE,
};
use crate::infra::storage::repo::{
    allocation_counter_repo, consumption_counter_repo, lease_repo, quota_repo,
};

const LOG_TARGET: &str = "qe.storage";

/// The Quota tables on the plugin's database.
#[derive(Clone)]
pub struct SqlQuotaStore {
    db: Db,
    enqueuer: Arc<dyn NotificationEnqueuer>,
    clock: crate::infra::storage::consumption_store::Clock,
}

/// Every way a transaction body can fail, before the lift to [`StoreError`].
#[derive(Debug, thiserror::Error)]
enum TxError {
    #[error(transparent)]
    Db(#[from] DbError),
    #[error(transparent)]
    Scope(#[from] ScopeError),
    #[error(transparent)]
    Map(#[from] MappingError),
    #[error(transparent)]
    Enqueue(#[from] EnqueueError),
    #[error(transparent)]
    Store(#[from] StoreError),
}

/// Lift a transaction failure onto the port error. Database failures are
/// logged here and reported as `Unavailable`; scope refusals are the
/// post-PDP defence (`SubjectOutOfScope`); a caller value that does not fit
/// its column is `ValueOutOfRange`; everything the schema should have made
/// impossible, an unrecognised scope failure included, is `Corrupt`.
fn lift(operation: &'static str, err: TxError) -> StoreError {
    match err {
        TxError::Store(err) => err,
        TxError::Db(err) => unavailable(operation, &err),
        TxError::Scope(ScopeError::Db(err)) => unavailable(operation, &err),
        TxError::Scope(
            ScopeError::TenantNotInScope { .. } | ScopeError::Denied(_) | ScopeError::Invalid(_),
        ) => StoreError::SubjectOutOfScope,
        // `ScopeError` is `#[non_exhaustive]`. A scope the ORM will not
        // compile is an inconsistency, not a caller's authorization problem.
        TxError::Scope(other) => corrupt(operation, &other),
        TxError::Map(MappingError::CapOutOfRange { cap }) => StoreError::ValueOutOfRange {
            field: "cap",
            value: cap.to_string(),
        },
        TxError::Map(MappingError::VersionOutOfRange { field, value }) => {
            StoreError::ValueOutOfRange {
                field,
                value: value.to_string(),
            }
        }
        TxError::Map(err @ MappingError::MetadataWithoutContract) => StoreError::InvalidPatch {
            detail: err.to_string(),
        },
        TxError::Map(err @ MappingError::Column { .. }) => corrupt(operation, &err),
        TxError::Enqueue(EnqueueError::NotBound) => {
            tracing::warn!(
                target: LOG_TARGET,
                operation,
                "notification outbox is not bound; the mutation was rolled back"
            );
            StoreError::Unavailable { operation }
        }
        TxError::Enqueue(EnqueueError::Outbox(toolkit_db::outbox::OutboxError::Database(err))) => {
            unavailable(operation, &err)
        }
        TxError::Enqueue(err) => corrupt(operation, &err),
    }
}

fn unavailable(operation: &'static str, err: &dyn std::fmt::Display) -> StoreError {
    tracing::warn!(target: LOG_TARGET, operation, error = %err, "storage backend call failed");
    StoreError::Unavailable { operation }
}

fn corrupt(operation: &'static str, err: &dyn std::fmt::Display) -> StoreError {
    tracing::error!(target: LOG_TARGET, operation, error = %err, "storage state is inconsistent");
    StoreError::Corrupt {
        operation,
        detail: err.to_string(),
    }
}

/// The events of a creation with the assigned id filled in.
fn with_quota_id(events: &[NotificationEvent], id: QuotaId) -> Vec<NotificationEvent> {
    events
        .iter()
        .cloned()
        .map(|mut event| {
            if event.quota_id.is_none() {
                event.quota_id = Some(id);
            }
            event
        })
        .collect()
}

impl SqlQuotaStore {
    /// Bind the store to the plugin's database and its notification outbox.
    #[must_use]
    pub fn new(db: Db, enqueuer: Arc<dyn NotificationEnqueuer>) -> Self {
        Self {
            db,
            enqueuer,
            clock: Arc::new(OffsetDateTime::now_utc),
        }
    }

    /// The same store on a caller-driven clock. The cap guard compares the
    /// current period against it, so a test that drives period boundaries has
    /// to drive this store's clock too.
    #[must_use]
    pub fn with_clock(
        db: Db,
        enqueuer: Arc<dyn NotificationEnqueuer>,
        clock: crate::infra::storage::consumption_store::Clock,
    ) -> Self {
        Self {
            db,
            enqueuer,
            clock,
        }
    }

    fn now(&self) -> OffsetDateTime {
        (self.clock)()
    }

    fn conn(&self, operation: &'static str) -> Result<toolkit_db::DbConn<'_>, StoreError> {
        self.db.conn().map_err(|e| unavailable(operation, &e))
    }

    /// The locked active row, or the port error that stops the mutation.
    async fn lock_active_row(
        runner: &impl DBRunner,
        scope: &AccessScope,
        quota_id: QuotaId,
    ) -> Result<quota::Model, TxError> {
        let row = quota_repo::find_by_id(
            runner,
            scope,
            quota_id.as_uuid(),
            Some(lease_repo::RowWait::Wait),
        )
        .await?
        .ok_or(StoreError::QuotaNotFound { id: quota_id })?;
        if row.status != STATUS_ACTIVE {
            return Err(StoreError::QuotaDeactivated { id: quota_id }.into());
        }
        Ok(row)
    }

    /// What the merged row would over-commit: the counters under the row lock.
    ///
    /// An allocation Quota reads its in-flight counter. A consumption Quota
    /// reads the period it is accumulating into, and only that one: a lowered
    /// cap governs the period the Quota is in, so an elapsed row's total is
    /// history and a Quota whose current period has not been materialized has
    /// consumed nothing yet.
    async fn consumed_under_lock(
        runner: &impl DBRunner,
        scope: &AccessScope,
        row: &quota::Model,
        now: OffsetDateTime,
    ) -> Result<u64, TxError> {
        let negative = |column: &'static str, value: i64| -> TxError {
            MappingError::Column {
                column,
                detail: format!("negative counter amount {value}"),
            }
            .into()
        };
        // This transaction holds the counter row, so it gives back the expired
        // holds still sitting in it before judging the cap (I4): capacity an
        // expired lease no longer holds must not block a legitimate reduction
        // until a sweeper happens to run.
        //
        // `return_expired_for` *writes* the reconciled counter, so both arms
        // below read it afterwards and take the value as it stands. Subtracting
        // the returned amount from a value read after that write would remove
        // it twice and let a cap below the real usage through.
        if row.quota_type == QuotaType::Allocation.as_gts_id() {
            return_expired_for(runner, scope, row.id, None, now).await?;
            let in_flight = allocation_counter_repo::read_in_flight_for_update(
                runner,
                scope,
                row.id,
                lease_repo::RowWait::Wait,
            )
            .await?
            .unwrap_or(0);
            return u64::try_from(in_flight).map_err(|_| negative("in_flight", in_flight));
        }
        let current = consumption_counter_repo::find_latest_for_update(
            runner,
            scope,
            row.id,
            lease_repo::RowWait::Wait,
        )
        .await?
        .filter(|period| period.period_start <= now && now < period.period_end);
        let Some(period) = current else {
            return Ok(0);
        };
        return_expired_for(runner, scope, row.id, Some(period.period_id), now).await?;
        let settled = consumption_counter_repo::find_by_period_id_for_update(
            runner,
            scope,
            period.period_id,
            lease_repo::RowWait::Wait,
        )
        .await?
        .map_or(period.consumed, |row| row.consumed);
        u64::try_from(settled).map_err(|_| negative("consumed", settled))
    }

    /// Invariants I6 and I14 on the merged row.
    fn check_merged(
        row: &quota::Model,
        update: &QuotaUpdate,
        consumed: u64,
    ) -> Result<(), TxError> {
        let merged_cap = update.merged_cap(row);
        if let Some(cap) = merged_cap {
            let new_cap = u64::try_from(cap).unwrap_or(0);
            if new_cap < consumed {
                return Err(StoreError::CapBelowConsumed { new_cap, consumed }.into());
            }
        }
        let thresholds = quota_mapping::thresholds_of(update.merged_thresholds(row))?;
        if merged_cap.is_none() && !thresholds.is_empty() {
            return Err(StoreError::ThresholdsRequireBoundedCap.into());
        }
        Ok(())
    }
}

#[async_trait]
// @cpt-flow:cpt-cf-quota-enforcement-flow-quota-deactivate:p1
// @cpt-state:cpt-cf-quota-enforcement-state-quota-lifecycle:p1
// @cpt-state:cpt-cf-quota-enforcement-state-lease:p1
// @cpt-dod:cpt-cf-quota-enforcement-dod-deactivation-cascade:p1
impl QuotaStore for SqlQuotaStore {
    async fn create_quota(
        &self,
        actor: &Actor,
        scope: &AccessScope,
        draft: QuotaDraft,
        events: &[NotificationEvent],
    ) -> Result<QuotaId, StoreError> {
        const OPERATION: &str = "create quota";
        validate_tenant_in_scope(draft.tenant_id.as_uuid(), scope)
            .map_err(|_| StoreError::SubjectOutOfScope)?;
        let id = QuotaId::generate();
        let now = self.now();
        let row =
            quota_mapping::draft_to_row(id, &draft, now).map_err(|e| lift(OPERATION, e.into()))?;
        let events = with_quota_id(events, id);
        let tenant_id = draft.tenant_id.as_uuid();
        let metric = draft.metric.as_str().to_owned();
        let with_counter = draft.quota_type == QuotaType::Allocation;
        let scope = scope.clone();
        let actor = actor.clone();
        let enqueuer = Arc::clone(&self.enqueuer);
        self.db
            .transaction_ref_mapped(move |tx| {
                Box::pin(async move {
                    quota_repo::insert(tx, &scope, row).await?;
                    // The pair's capacity row exists before its first lease, so
                    // an acquisition only ever locks it and never meets another
                    // transaction's uncommitted insert (I8).
                    lease_repo::ensure_capacity_row(tx, &scope, tenant_id, &metric, now).await?;
                    if with_counter {
                        allocation_counter_repo::insert_initial(
                            tx,
                            &scope,
                            id.as_uuid(),
                            tenant_id,
                            now,
                        )
                        .await?;
                    }
                    operation_log_repo::append(
                        tx,
                        &scope,
                        Entry {
                            tenant_id,
                            quota_id: id.as_uuid(),
                            operation: OP_QUOTA_CREATE,
                            actor: &actor,
                            record_version: 1,
                            detail: String::new(),
                            occurred_at: now,
                        },
                    )
                    .await?;
                    enqueuer.enqueue_all(tx, &events).await?;
                    Ok::<QuotaId, TxError>(id)
                })
            })
            .await
            .map_err(|e| lift(OPERATION, e))
    }

    async fn update_quota(
        &self,
        actor: &Actor,
        scope: &AccessScope,
        quota_id: QuotaId,
        patch: QuotaPatch,
        events: &[NotificationEvent],
    ) -> Result<Quota, StoreError> {
        const OPERATION: &str = "update quota";
        let update =
            quota_mapping::patch_to_update(&patch).map_err(|e| lift(OPERATION, e.into()))?;
        let scope = scope.clone();
        let actor = actor.clone();
        let events = events.to_vec();
        let enqueuer = Arc::clone(&self.enqueuer);
        let clock = Arc::clone(&self.clock);
        self.db
            .transaction_ref_mapped(move |tx| {
                Box::pin(async move {
                    let row = Self::lock_active_row(tx, &scope, quota_id).await?;
                    // Read under the lock, never before it. An update that
                    // waited here may have waited out a period boundary, and a
                    // debit that crossed it first has already opened the row
                    // this cap has to respect.
                    let now = clock();
                    let consumed = Self::consumed_under_lock(tx, &scope, &row, now).await?;
                    Self::check_merged(&row, &update, consumed)?;
                    // @cpt-begin:cpt-cf-quota-enforcement-state-quota-lifecycle:p1:inst-qst-update
                    let applied = quota_repo::apply_update(
                        tx,
                        &scope,
                        row.id,
                        row.record_version,
                        &update,
                        now,
                    )
                    .await?;
                    // @cpt-end:cpt-cf-quota-enforcement-state-quota-lifecycle:p1:inst-qst-update
                    if !applied {
                        return Err(StoreError::Corrupt {
                            operation: OPERATION,
                            detail: "the locked row moved before the update".to_owned(),
                        }
                        .into());
                    }
                    let committed = quota_repo::find_by_id(tx, &scope, row.id, None)
                        .await?
                        .ok_or_else(|| StoreError::Corrupt {
                            operation: OPERATION,
                            detail: "the updated row does not read back".to_owned(),
                        })?;
                    operation_log_repo::append(
                        tx,
                        &scope,
                        Entry {
                            tenant_id: row.tenant_id,
                            quota_id: row.id,
                            operation: OP_QUOTA_UPDATE,
                            actor: &actor,
                            record_version: committed.record_version,
                            detail: patched_fields(&update),
                            occurred_at: now,
                        },
                    )
                    .await?;
                    enqueuer.enqueue_all(tx, &events).await?;
                    Ok::<Quota, TxError>(quota_mapping::row_to_quota(committed)?)
                })
            })
            .await
            .map_err(|e| lift(OPERATION, e))
    }

    async fn deactivate_quota(
        &self,
        actor: &Actor,
        scope: &AccessScope,
        quota_id: QuotaId,
        events: &[NotificationEvent],
    ) -> Result<DeactivateOutcome, StoreError> {
        const OPERATION: &str = "deactivate quota";
        let now = self.now();
        let scope = scope.clone();
        let actor = actor.clone();
        let events = events.to_vec();
        let enqueuer = Arc::clone(&self.enqueuer);
        self.db
            .transaction_ref_mapped(move |tx| {
                Box::pin(async move {
                    // @cpt-begin:cpt-cf-quota-enforcement-flow-quota-deactivate:p1:inst-qde-cascade
                    let row = Self::lock_active_row(tx, &scope, quota_id).await?;
                    // @cpt-begin:cpt-cf-quota-enforcement-state-quota-lifecycle:p1:inst-qst-deactivate
                    let flipped =
                        quota_repo::mark_deactivated(tx, &scope, row.id, row.record_version, now)
                            .await?;
                    // @cpt-end:cpt-cf-quota-enforcement-state-quota-lifecycle:p1:inst-qst-deactivate
                    if !flipped {
                        return Err(StoreError::Corrupt {
                            operation: OPERATION,
                            detail: "the locked row moved before the deactivation".to_owned(),
                        }
                        .into());
                    }
                    // Rank 3: the leases still holding this Quota. Expired
                    // ones are already released (I4) and belong to the sweeper,
                    // so the cascade neither resolves nor re-credits them.
                    let holders = lease_repo::active_on_quota_for_update(
                        tx,
                        &scope,
                        row.id,
                        now,
                        lease_repo::RowWait::Wait,
                    )
                    .await?;
                    let mut resolved = Vec::with_capacity(holders.len());
                    let mut lease_events = Vec::new();
                    for lease in holders {
                        // Rank 4, then 5 and 5b: every hold of this lease, on
                        // this Quota and on any other it spans, ascending.
                        let capacity = lease_repo::lock_capacity_row(
                            tx,
                            &scope,
                            lease.tenant_id,
                            &lease.metric,
                            lease_repo::RowWait::Wait,
                        )
                        .await?;
                        let holds = lease_repo::holds_of(tx, &scope, lease.token).await?;
                        let mut released = 0_u64;
                        for hold in &holds {
                            let amount = u64::try_from(hold.held_amount).unwrap_or(0);
                            if lease_repo::mark_hold_returned(
                                tx,
                                &scope,
                                lease.token,
                                hold.quota_id,
                                now,
                            )
                            .await?
                            {
                                return_capacity(tx, &scope, hold.quota_id, hold.period_id, amount)
                                    .await?;
                                released = released.saturating_add(amount);
                            }
                        }
                        // @cpt-begin:cpt-cf-quota-enforcement-state-lease:p1:inst-lst-deactivate
                        if !lease_repo::mark_state(
                            tx,
                            &scope,
                            lease.token,
                            lease_repo::STATE_RESOLVED_BY_DEACTIVATION,
                            now,
                        )
                        .await?
                        {
                            continue;
                        }
                        // @cpt-end:cpt-cf-quota-enforcement-state-lease:p1:inst-lst-deactivate
                        // Diagnostic only, so a missing row costs the count, not the
                        // transition.
                        if let Some(capacity) = &capacity {
                            lease_repo::bump_active_count(
                                tx,
                                &scope,
                                lease.tenant_id,
                                &lease.metric,
                                capacity.active_count,
                                -1,
                                now,
                            )
                            .await?;
                        }
                        resolved.push(LeaseToken::from(lease.token));
                        // Only the transaction knows which leases it resolved,
                        // so it builds their events itself (I11).
                        lease_events.push(NotificationEvent {
                            event_id: EventId::generate(),
                            kind: NotificationEventKind::LeaseResolvedByDeactivation,
                            scope: NotificationScope::Tenant {
                                tenant_id: TenantId::from(lease.tenant_id),
                            },
                            quota_id: Some(QuotaId::from(row.id)),
                            policy_id: None,
                            subject: None,
                            payload: serde_json::json!({
                                "lease_token": LeaseToken::from(lease.token),
                                "held_amount": released,
                                "quota_id": QuotaId::from(row.id),
                            }),
                            emitted_at: now,
                        });
                    }
                    // @cpt-end:cpt-cf-quota-enforcement-flow-quota-deactivate:p1:inst-qde-cascade
                    let outcome = DeactivateOutcome {
                        resolved_leases: resolved,
                    };
                    operation_log_repo::append(
                        tx,
                        &scope,
                        Entry {
                            tenant_id: row.tenant_id,
                            quota_id: row.id,
                            operation: OP_QUOTA_DEACTIVATE,
                            actor: &actor,
                            record_version: row.record_version + 1,
                            detail: format!("resolved_leases={}", outcome.resolved_leases.len()),
                            occurred_at: now,
                        },
                    )
                    .await?;
                    // @cpt-begin:cpt-cf-quota-enforcement-flow-quota-deactivate:p1:inst-qde-events
                    enqueuer.enqueue_all(tx, &events).await?;
                    if !lease_events.is_empty() {
                        enqueuer.enqueue_all(tx, &lease_events).await?;
                    }
                    // @cpt-end:cpt-cf-quota-enforcement-flow-quota-deactivate:p1:inst-qde-events
                    // @cpt-begin:cpt-cf-quota-enforcement-flow-quota-deactivate:p1:inst-qde-atomic
                    Ok::<DeactivateOutcome, TxError>(outcome)
                    // @cpt-end:cpt-cf-quota-enforcement-flow-quota-deactivate:p1:inst-qde-atomic
                })
            })
            .await
            .map_err(|e| lift(OPERATION, e))
    }

    async fn read_quotas(
        &self,
        scope: &AccessScope,
        filter: QuotaFilter,
        page: PageRequest,
    ) -> Result<PageResult<Quota>, StoreError> {
        const OPERATION: &str = "read quotas";
        if filter.ids.len() > MAX_FILTER_IDS {
            return Err(StoreError::InvalidFilter {
                detail: format!(
                    "{} ids named, at most {MAX_FILTER_IDS} are accepted",
                    filter.ids.len()
                ),
            });
        }
        let limit = cursor::effective_limit(page.limit);
        let after = page
            .cursor
            .as_deref()
            .map(cursor::decode)
            .transpose()?
            .map(QuotaId::as_uuid);
        let ids: Vec<Uuid> = filter.ids.iter().map(|id| id.as_uuid()).collect();
        let status = filter.status.map(status_name);
        let query = quota_repo::ListQuery {
            tenant_id: filter.tenant_id.map(TenantId::as_uuid),
            subject: filter
                .subject
                .as_ref()
                .map(|s| (s.projection_type.as_ref(), s.subject_id.as_str())),
            metric: filter.metric.as_ref().map(MetricId::as_str),
            status,
            ids: &ids,
            after,
            limit: u64::from(limit) + 1,
        };
        let conn = self.conn(OPERATION)?;
        let mut rows = quota_repo::list_page(&conn, scope, &query)
            .await
            .map_err(|e| lift(OPERATION, e.into()))?;
        let has_more = rows.len() > limit as usize;
        rows.truncate(limit as usize);
        let next_cursor = if has_more {
            rows.last().map(|row| cursor::encode(QuotaId::new(row.id)))
        } else {
            None
        };
        let items = rows
            .into_iter()
            .map(quota_mapping::row_to_quota)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| lift(OPERATION, e.into()))?;
        Ok(PageResult { items, next_cursor })
    }

    async fn read_active_projection_bindings(
        &self,
    ) -> Result<HashSet<ProjectionBinding>, StoreError> {
        const OPERATION: &str = "read active projection bindings";
        let conn = self.conn(OPERATION)?;
        let rows = quota_repo::active_bindings(&conn)
            .await
            .map_err(|e| lift(OPERATION, e.into()))?;
        rows.into_iter()
            .map(|row| {
                Ok(ProjectionBinding {
                    metric: MetricId::parse(&row.metric).map_err(|e| MappingError::Column {
                        column: "metric",
                        detail: e.to_string(),
                    })?,
                    projection_type: gts::GtsTypeId::try_new(&row.projection_type).map_err(
                        |e| MappingError::Column {
                            column: "projection_type",
                            detail: e.to_string(),
                        },
                    )?,
                })
            })
            .collect::<Result<HashSet<_>, MappingError>>()
            .map_err(|e| lift(OPERATION, e.into()))
    }

    async fn read_active_quota_counts(&self) -> Result<ActiveQuotaCounts, StoreError> {
        const OPERATION: &str = "read active quota counts";
        let conn = self.conn(OPERATION)?;
        let cap_zero = quota_repo::count_active_cap_zero(&conn)
            .await
            .map_err(|e| lift(OPERATION, e.into()))?;
        let cap_unbounded = quota_repo::count_active_cap_unbounded(&conn)
            .await
            .map_err(|e| lift(OPERATION, e.into()))?;
        let rows = quota_repo::active_counts_by_metric(&conn)
            .await
            .map_err(|e| lift(OPERATION, e.into()))?;
        let by_metric = rows
            .into_iter()
            .map(|row| {
                let metric = MetricId::parse(&row.metric).map_err(|e| MappingError::Column {
                    column: "metric",
                    detail: e.to_string(),
                })?;
                Ok((metric, u64::try_from(row.total).unwrap_or(0)))
            })
            .collect::<Result<HashMap<_, _>, MappingError>>()
            .map_err(|e| lift(OPERATION, e.into()))?;
        Ok(ActiveQuotaCounts {
            cap_zero,
            cap_unbounded,
            by_metric,
        })
    }
}

/// The names of the columns a patch sets, for the operation log. Names only,
/// never values: metadata is opaque to QE.
fn patched_fields(update: &QuotaUpdate) -> String {
    let mut fields = Vec::new();
    if update.cap.is_some() {
        fields.push("cap");
    }
    if update.notification_thresholds.is_some() {
        fields.push("notification_thresholds");
    }
    if update.validity.is_some() {
        fields.push("validity_window");
    }
    if update.metadata.is_some() {
        fields.push("metadata,constraint_contract");
    }
    if update.enforcement_mode.is_some() {
        fields.push("enforcement_mode");
    }
    if update.fail_open_hint.is_some() {
        fields.push("fail_open_hint");
    }
    fields.join(",")
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "quota_store_tests.rs"]
mod quota_store_tests;

/// Give a hold's capacity back to the counter it was taken from.
///
/// The acquisition period, not the current one: a lease resolved after a
/// boundary still settles where it was acquired (I5). Flooring at zero keeps a
/// double return — which the `returned_at` stamp already prevents — from
/// driving a counter negative.
async fn return_capacity(
    tx: &impl DBRunner,
    scope: &AccessScope,
    quota_id: Uuid,
    period_id: Option<Uuid>,
    amount: u64,
) -> Result<(), TxError> {
    if amount == 0 {
        return Ok(());
    }
    if let Some(period_id) = period_id {
        let Some(row) = consumption_counter_repo::find_by_period_id_for_update(
            tx,
            scope,
            period_id,
            lease_repo::RowWait::Wait,
        )
        .await?
        else {
            return Ok(());
        };
        let value = u64::try_from(row.consumed)
            .unwrap_or(0)
            .saturating_sub(amount);
        consumption_counter_repo::write_counter(
            tx,
            scope,
            period_id,
            row.record_version,
            i64::try_from(value).unwrap_or(i64::MAX),
            row.highest_crossed_threshold_pct,
            row.updated_at,
        )
        .await?;
    } else {
        let Some(row) = consumption_counter_repo::find_allocation_for_update(
            tx,
            scope,
            quota_id,
            lease_repo::RowWait::Wait,
        )
        .await?
        else {
            return Ok(());
        };
        let value = u64::try_from(row.in_flight)
            .unwrap_or(0)
            .saturating_sub(amount);
        consumption_counter_repo::write_allocation(
            tx,
            scope,
            quota_id,
            row.record_version,
            i64::try_from(value).unwrap_or(i64::MAX),
            row.highest_crossed_threshold_pct,
            row.updated_at,
        )
        .await?;
    }
    Ok(())
}

/// Return every expired hold on one counter row that nobody has given back
/// yet, and report the total.
///
/// The same rule the consumption store applies on its own writers (I4): the
/// transaction that holds the row reconciles it, stamps each hold, and moves
/// the counter once, so a later sweep finds nothing left to credit.
async fn return_expired_for(
    runner: &impl DBRunner,
    scope: &AccessScope,
    quota_id: Uuid,
    period_id: Option<Uuid>,
    now: OffsetDateTime,
) -> Result<u64, TxError> {
    let holds =
        lease_repo::unreturned_expired_holds(runner, scope, quota_id, period_id, now).await?;
    let mut total = 0_u64;
    for hold in holds {
        if lease_repo::mark_hold_returned(runner, scope, hold.lease_token, hold.quota_id, now)
            .await?
        {
            let amount = u64::try_from(hold.held_amount).unwrap_or(0);
            return_capacity(runner, scope, quota_id, period_id, amount).await?;
            total = total.saturating_add(amount);
        }
    }
    Ok(total)
}
