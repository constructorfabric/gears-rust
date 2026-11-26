//! The consumption primitives: debit, credit, the snapshot read, and the
//! reclamation the retention sweeper drives. Return and rollback live in
//! `return_store`.
//!
//! Every mutation is one transaction that moves the counters, writes the
//! idempotency record, appends the operation-log row, and enqueues the events
//! together (I1, I11). Locks are taken in ADR-0002 order: the Quota row first,
//! then its counter rows, and Quotas themselves ascending by id, so two
//! operations over overlapping Quota sets cannot deadlock.
//!
//! Two writers can share an idempotency scope and still lock disjoint Quota
//! rows, so the row locks alone do not serialize them. The record's primary key
//! does: the loser's insert violates it, its transaction rolls back whole, and
//! it then resolves the winner's record into a replay or a payload mismatch.

use std::sync::Arc;

use quota_enforcement_sdk::{
    ApplicableQuotas, AppliedMutation, AttributionDigest, ChargeReturn, CounterSnapshot,
    DECISION_BLOB_VERSION, Decision, DecisionResult, EvaluatedDebit, EvaluatedMutation,
    EvaluationContext, EvaluationFailure, EvaluationQuota, IdempotencyRecord, IdempotencyScope,
    IdempotencyWrite, MutationResult, NO_APPLICABLE_QUOTA, NotificationEvent,
    NotificationEventKind, NotificationScope, OperationType, PageRequest, PageResult,
    PartialIdempotencyWrite, PayloadHash, PeriodId, PeriodType, PeriodWindow, PolicyScope,
    PolicyVersion, Quota, QuotaId, QuotaScopeTier, QuotaSnapshot, QuotaStatus, QuotaType,
    Retention, RollbackTarget, StorageError, TenantId, ThresholdCrossing, TransitionOutcome,
    conditions_hold, plan_entries, plan_of, threshold_crossings,
};
use serde_json::{Value, json};
use time::OffsetDateTime;
use toolkit_db::secure::{DBRunner, ScopeError};
use toolkit_db::{Db, DbError};
use toolkit_security::AccessScope;
use uuid::Uuid;

use super::config_cache::{ConfigCache, ConfigSnapshot, DEFAULT_REFRESH};
use super::entity::quota_consumption_counter;
use super::locking::{ContentionBudget, lock_scopes, with_budget};
use super::policy_store::{decode_version, scope_key};
use super::quota_mapping::{self, MappingError};
use super::repo::operation_log_repo::Entry;
use super::repo::{
    RowWait, charge_repo, consumption_counter_repo as counter_repo, idempotency_repo as idem_repo,
    lease_repo, operation_log_repo, policy_repo, quota_repo,
};
use crate::domain::ports::Actor;
use crate::infra::outbox::{
    AttemptWakes, EnqueueError, NotificationEnqueuer, NotificationOutbox, settling,
};

const LOG_TARGET: &str = "qe.storage";

/// How many times a transaction that lost the record's primary key is retried.
/// Two: one to observe the winner, and the original attempt.
pub(super) const RACE_ATTEMPTS: u32 = 2;

/// Wall clock of a store, replaceable in tests so that period boundaries and
/// retention deadlines can be driven without waiting for them. The Quota store
/// shares it: its cap guard reads the period this clock says is current.
pub type Clock = Arc<dyn Fn() -> OffsetDateTime + Send + Sync>;

/// SQL adapter of the consumption primitives.
// @cpt-algo:cpt-cf-quota-enforcement-algo-threshold-emission:p1
// @cpt-algo:cpt-cf-quota-enforcement-algo-period-rollover:p1
// @cpt-algo:cpt-cf-quota-enforcement-algo-evaluation-pipeline:p1
// @cpt-algo:cpt-cf-quota-enforcement-algo-idempotency-replay:p1
// @cpt-state:cpt-cf-quota-enforcement-state-consumption-period:p1
// @cpt-state:cpt-cf-quota-enforcement-state-idempotency-record:p1
// @cpt-flow:cpt-cf-quota-enforcement-flow-credit:p1
// @cpt-algo:cpt-cf-quota-enforcement-algo-lazy-expiry:p1
// @cpt-dod:cpt-cf-quota-enforcement-dod-lazy-expiry:p1
// @cpt-dod:cpt-cf-quota-enforcement-dod-counter-period:p1
#[derive(Clone)]
pub struct SqlConsumptionStore {
    pub(super) db: Db,
    pub(super) enqueuer: Arc<dyn NotificationOutbox>,
    pub(super) clock: Clock,
    pub(super) config: Arc<ConfigCache>,
}

#[derive(Debug, thiserror::Error)]
pub(super) enum TxError {
    #[error(transparent)]
    Db(#[from] DbError),
    #[error(transparent)]
    Scope(#[from] ScopeError),
    #[error(transparent)]
    Storage(#[from] StorageError),
    #[error(transparent)]
    Map(#[from] MappingError),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Enqueue(#[from] EnqueueError),
    /// Another transaction committed this idempotency scope first. Internal:
    /// it never reaches a caller, it rolls this transaction back so the loser
    /// can resolve the winner's record. It carries the scope because credit
    /// derives that scope inside the transaction, where the caller cannot see
    /// it.
    #[error("another transaction committed this idempotency scope first")]
    Raced(Box<IdempotencyWrite>),
}

pub(super) fn unavailable(
    operation: &'static str,
    source: &'static str,
    error: &dyn std::fmt::Display,
) -> StorageError {
    tracing::warn!(
        target: LOG_TARGET,
        operation,
        source,
        error = %error,
        "consumption storage backend call failed"
    );
    StorageError::Unavailable(format!("{operation} failed: {source} unavailable"))
}

fn corrupt(operation: &'static str, error: &dyn std::fmt::Display) -> StorageError {
    tracing::error!(
        target: LOG_TARGET,
        operation,
        error = %error,
        "consumption storage state is inconsistent"
    );
    StorageError::Internal(format!("{operation} found inconsistent counter state"))
}

pub(super) fn lift(operation: &'static str, error: TxError) -> StorageError {
    if super::locking::is_lock_not_available(&error) {
        return StorageError::LeaseContentionTimeout;
    }
    match error {
        TxError::Storage(error) => error,
        TxError::Db(error) => unavailable(operation, "transaction", &error),
        TxError::Scope(ScopeError::Db(error)) => unavailable(operation, "query", &error),
        TxError::Enqueue(EnqueueError::Outbox(toolkit_db::outbox::OutboxError::Database(
            error,
        ))) => unavailable(operation, "outbox", &error),
        TxError::Enqueue(EnqueueError::NotBound) => {
            tracing::warn!(
                target: LOG_TARGET,
                operation,
                "notification outbox is not bound; the mutation was rolled back"
            );
            StorageError::Unavailable(format!("{operation} failed: outbox is not bound"))
        }
        // ORM scope refusal here is an internal inconsistency.
        TxError::Scope(error) => corrupt(operation, &error),
        TxError::Map(error) => corrupt(operation, &error),
        TxError::Json(error) => corrupt(operation, &error),
        TxError::Enqueue(error) => corrupt(operation, &error),
        // Consumed by a retry loop; seeing it here means the loop gave up.
        TxError::Raced(_) => StorageError::Unavailable(format!(
            "{operation} failed: the idempotency key is being written concurrently"
        )),
    }
}

/// Everything a debit's transaction needs, owned.
///
/// The future that runs inside a transaction may not name any lifetime of its
/// caller, so the borrowed [`EvaluatedMutation`] is copied into this before the
/// transaction opens. Only the evaluation callback is shared rather than
/// cloned, which is why the contract passes it as a handle.
pub(super) struct OwnedMutation {
    pub(super) applicable: ApplicableQuotas,
    pub(super) request: Value,
    metric_id: Option<String>,
    limits: quota_enforcement_sdk::engine::EvaluationLimits,
    pub(super) amount: u64,
    pub(super) idempotency: IdempotencyWrite,
    pub(super) authorized: AttributionDigest,
    evaluate: Arc<quota_enforcement_sdk::engine::TransactionEvaluator>,
}

impl OwnedMutation {
    pub(super) fn of(mutation: &EvaluatedMutation<'_>) -> Self {
        Self {
            applicable: mutation.applicable.clone(),
            request: mutation.request.clone(),
            metric_id: mutation.metric_id.map(str::to_owned),
            limits: mutation.limits,
            amount: mutation.amount,
            idempotency: mutation.idempotency.clone(),
            authorized: mutation.authorized,
            evaluate: Arc::clone(&mutation.evaluate),
        }
    }
}

/// One counter movement of a plan entry. A charge keeps one entry per
/// movement, against the period it was attributed to (I5).
#[derive(Debug, Clone)]
pub(super) struct AppliedEntry {
    pub(super) quota_id: Uuid,
    pub(super) period_id: Option<Uuid>,
    pub(super) amount: u64,
    /// Counter value after the movement, for the caller's mutation result.
    pub(super) value: u64,
    /// Chain of the plan entry this movement applied.
    pub(super) chain: u32,
    /// Position of that entry inside its chain.
    pub(super) position: u32,
}

impl AppliedEntry {
    /// The entry as part of a plan, tagged with its `(chain, position)`.
    pub(super) fn planned(&self) -> ((u32, u32), quota_enforcement_sdk::QuotaDebitPlan) {
        (
            (self.chain, self.position),
            quota_enforcement_sdk::QuotaDebitPlan {
                quota_id: QuotaId::new(self.quota_id),
                amount: self.amount,
            },
        )
    }
}

/// What a debit committed, before it becomes the caller's result.
pub(super) struct Applied {
    pub(super) entries: Vec<AppliedEntry>,
    pub(super) crossings: Vec<ThresholdCrossing>,
}

// ---------------------------------------------------------------------------
// Period helpers
// ---------------------------------------------------------------------------

/// The window a Quota's period type puts `now` in.
pub(super) fn window_of(quota: &Quota, now: OffsetDateTime) -> PeriodWindow {
    quota
        .period
        .unwrap_or(PeriodType::OneTime)
        .window_containing(now)
}

/// The row a consumption Quota accumulates into at `now`, materializing it when
/// the Quota has none or its latest row has elapsed.
///
/// Materialization creates the successor row and nothing else: settling the row
/// it succeeds belongs to a mutating primitive, so a snapshot read enqueues no
/// event.
pub(super) async fn ensure_current_period(
    tx: &impl DBRunner,
    scope: &AccessScope,
    quota: &Quota,
    now: OffsetDateTime,
    wait: RowWait,
) -> Result<quota_consumption_counter::Model, TxError> {
    let latest = counter_repo::find_latest_for_update(tx, scope, quota.id.as_uuid(), wait).await?;
    if let Some(row) = latest
        && row.period_start <= now
        && now < row.period_end
    {
        return Ok(row);
    }
    // @cpt-begin:cpt-cf-quota-enforcement-algo-period-rollover:p1:inst-per-spec
    // @cpt-begin:cpt-cf-quota-enforcement-algo-period-rollover:p1:inst-per-lazy-if
    // @cpt-begin:cpt-cf-quota-enforcement-algo-period-rollover:p1:inst-per-lazy
    // @cpt-begin:cpt-cf-quota-enforcement-state-consumption-period:p1:inst-perst-create
    let window = window_of(quota, now);
    let period = counter_repo::NewPeriod {
        quota_id: quota.id.as_uuid(),
        tenant_id: quota.tenant_id.as_uuid(),
        start: window.start,
        end: window.end,
        now,
    };
    // @cpt-end:cpt-cf-quota-enforcement-state-consumption-period:p1:inst-perst-create
    // @cpt-end:cpt-cf-quota-enforcement-algo-period-rollover:p1:inst-per-lazy
    // @cpt-end:cpt-cf-quota-enforcement-algo-period-rollover:p1:inst-per-lazy-if
    // @cpt-end:cpt-cf-quota-enforcement-algo-period-rollover:p1:inst-per-spec
    match counter_repo::insert_period(tx, scope, Uuid::now_v7(), &period).await {
        Ok(row) => Ok(row),
        Err(error) if error.is_unique_violation() => {
            // The unique `(quota_id, period_start)` key arbitrates concurrent opens.
            counter_repo::find_latest_for_update(tx, scope, quota.id.as_uuid(), wait)
                .await?
                .ok_or_else(|| {
                    TxError::Storage(StorageError::Internal(
                        "the period row that won materialization is missing".to_owned(),
                    ))
                })
        }
        Err(error) => Err(error.into()),
    }
}

/// Settle every elapsed, unsettled row of `quota`, emitting one rollover event
/// each. Only a mutation against the current period calls this.
pub(super) async fn settle_elapsed_rows(
    tx: &impl DBRunner,
    scope: &AccessScope,
    actor: &Actor,
    quota: &Quota,
    now: OffsetDateTime,
    enqueuer: &Arc<dyn NotificationEnqueuer>,
    wait: RowWait,
) -> Result<(), TxError> {
    let closing =
        counter_repo::find_elapsed_unsettled_for_update(tx, scope, quota.id.as_uuid(), now, wait)
            .await?;
    // @cpt-begin:cpt-cf-quota-enforcement-algo-period-rollover:p1:inst-per-settle
    // @cpt-begin:cpt-cf-quota-enforcement-algo-period-rollover:p1:inst-per-window
    // @cpt-begin:cpt-cf-quota-enforcement-algo-period-rollover:p1:inst-per-forfeit
    // @cpt-begin:cpt-cf-quota-enforcement-algo-period-rollover:p1:inst-per-return
    // @cpt-begin:cpt-cf-quota-enforcement-state-consumption-period:p1:inst-perst-settle
    for row in closing {
        // A lease acquired in this period settles against it whenever it
        // resolves (I5), so the period is not closed while one is still live:
        // the rollover event would report a consumed amount that can still
        // move, and a later commit would land on a settled row.
        if lease_repo::has_live_holds(tx, scope, row.period_id, now).await? {
            continue;
        }
        // Expired holds on the closing row are returned before its total is
        // read, so the payload states what the period actually consumed.
        let owed = return_expired_holds(tx, scope, row.quota_id, Some(row.period_id), now).await?;
        let closing_consumed = u64::try_from(row.consumed)
            .unwrap_or(0)
            .saturating_sub(owed);
        // The holds are stamped returned now, so no reader subtracts them any
        // more: the row itself has to drop them, in this transaction.
        if owed > 0 {
            let stored = i64::try_from(closing_consumed).unwrap_or(i64::MAX);
            let written = counter_repo::write_counter(
                tx,
                scope,
                row.period_id,
                row.record_version,
                stored,
                row.highest_crossed_threshold_pct,
                now,
            )
            .await?;
            if !written {
                return Err(TxError::Storage(StorageError::Internal(
                    "the locked closing period row moved before the write".to_owned(),
                )));
            }
        }
        if !counter_repo::mark_settled(tx, scope, row.period_id, now).await? {
            continue;
        }
        let event = quota_event(
            actor,
            quota,
            NotificationEventKind::PeriodRollover,
            json!({
                "closing_period_id": PeriodId::new(row.period_id),
                "closing_consumed": closing_consumed,
                "closing_cap": quota.cap,
                "new_period_boundary": row.period_end,
            }),
            now,
        );
        enqueuer
            .enqueue_all(tx, std::slice::from_ref(&event))
            .await?;
    }
    // @cpt-end:cpt-cf-quota-enforcement-state-consumption-period:p1:inst-perst-settle
    // @cpt-end:cpt-cf-quota-enforcement-algo-period-rollover:p1:inst-per-return
    // @cpt-end:cpt-cf-quota-enforcement-algo-period-rollover:p1:inst-per-forfeit
    // @cpt-end:cpt-cf-quota-enforcement-algo-period-rollover:p1:inst-per-window
    // @cpt-end:cpt-cf-quota-enforcement-algo-period-rollover:p1:inst-per-settle
    Ok(())
}

pub(super) fn quota_event(
    actor: &Actor,
    quota: &Quota,
    kind: NotificationEventKind,
    payload: Value,
    now: OffsetDateTime,
) -> NotificationEvent {
    NotificationEvent {
        event_id: quota_enforcement_sdk::EventId::generate(),
        kind,
        actor: actor.clone(),
        scope: NotificationScope::Tenant {
            tenant_id: quota.tenant_id,
        },
        quota_id: Some(quota.id),
        policy_id: None,
        subject: Some(quota.subject.clone()),
        payload,
        emitted_at: now,
    }
}

/// Raise one Quota's counter and emit whatever thresholds the move crossed.
///
/// Emission is silent for a row past its boundary: that row is being closed,
/// and ADR-0004 keeps threshold and adjustment events out of the settlement
/// window.
/// Give back every expired hold on one counter row that nobody has returned
/// yet, and report what that came to.
///
/// An expired lease is released the moment its TTL passes (I4), but its
/// capacity sits in the counter until someone gives it back. The writer that
/// holds the row is the one that does it: it stamps each hold `returned_at`
/// under that lock (rank 5b) and folds the total into its own single write, so
/// the row moves once and a later sweep finds nothing left to credit.
///
/// Doing it here rather than only in the reader is what keeps a credit from
/// flooring away capacity that is still owed: the stored counter and the
/// logical one agree from the end of this call onward.
pub(super) async fn return_expired_holds(
    tx: &impl DBRunner,
    scope: &AccessScope,
    quota_id: Uuid,
    period_id: Option<Uuid>,
    now: OffsetDateTime,
) -> Result<u64, TxError> {
    // @cpt-begin:cpt-cf-quota-enforcement-algo-lazy-expiry:p1:inst-lzy-rule
    let holds = lease_repo::unreturned_expired_holds(tx, scope, quota_id, period_id, now).await?;
    let mut total = 0_u64;
    for hold in holds {
        if lease_repo::mark_hold_returned(tx, scope, hold.lease_token, hold.quota_id, now).await? {
            total = total.saturating_add(u64::try_from(hold.held_amount).unwrap_or(0));
        }
    }
    Ok(total)
    // @cpt-end:cpt-cf-quota-enforcement-algo-lazy-expiry:p1:inst-lzy-rule
}

/// What expired holds still occupy on one counter row, for a reader that may
/// not write (I3): a snapshot subtracts this instead of returning it.
pub(super) async fn unreturned_expired(
    tx: &impl DBRunner,
    scope: &AccessScope,
    quota_id: Uuid,
    period_id: Option<Uuid>,
    now: OffsetDateTime,
) -> Result<u64, TxError> {
    // @cpt-begin:cpt-cf-quota-enforcement-algo-lazy-expiry:p1:inst-lzy-return
    let holds = lease_repo::unreturned_expired_holds(tx, scope, quota_id, period_id, now).await?;
    Ok(holds
        .into_iter()
        .map(|hold| u64::try_from(hold.held_amount).unwrap_or(0))
        .fold(0_u64, u64::saturating_add))
    // @cpt-end:cpt-cf-quota-enforcement-algo-lazy-expiry:p1:inst-lzy-return
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn debit_counter(
    tx: &impl DBRunner,
    scope: &AccessScope,
    actor: &Actor,
    quota: &Quota,
    amount: u64,
    now: OffsetDateTime,
    enqueuer: &Arc<dyn NotificationEnqueuer>,
    wait: RowWait,
) -> Result<(AppliedEntry, Option<ThresholdCrossing>), TxError> {
    let overflow = || {
        TxError::Storage(StorageError::Internal(format!(
            "counter of quota {} would overflow",
            quota.id
        )))
    };
    // @cpt-begin:cpt-cf-quota-enforcement-algo-period-rollover:p1:inst-per-attr
    // @cpt-begin:cpt-cf-quota-enforcement-state-consumption-period:p1:inst-perst-boundary
    let (period_id, pre, version, silent) = if quota.quota_type == QuotaType::Consumption {
        let row = ensure_current_period(tx, scope, quota, now, wait).await?;
        let pre = u64::try_from(row.consumed).unwrap_or(0);
        (
            Some(row.period_id),
            pre,
            row.record_version,
            now >= row.period_end,
        )
    } else {
        let row = counter_repo::find_allocation_for_update(tx, scope, quota.id.as_uuid(), wait)
            .await?
            .ok_or_else(|| {
                TxError::Storage(StorageError::Internal(format!(
                    "allocation quota {} has no counter row",
                    quota.id
                )))
            })?;
        (
            None,
            u64::try_from(row.in_flight).unwrap_or(0),
            row.record_version,
            false,
        )
    };
    // @cpt-end:cpt-cf-quota-enforcement-state-consumption-period:p1:inst-perst-boundary
    // @cpt-end:cpt-cf-quota-enforcement-algo-period-rollover:p1:inst-per-attr
    // The row is held, so this writer reconciles the expired holds still
    // sitting in it (I4) and carries the correction into its own write.
    let expired = return_expired_holds(tx, scope, quota.id.as_uuid(), period_id, now).await?;
    let pre = pre.saturating_sub(expired);
    let post = pre.checked_add(amount).ok_or_else(overflow)?;
    let marker = current_marker(tx, scope, quota, period_id, wait).await?;
    let crossing = threshold_crossings(
        quota.id,
        pre,
        post,
        quota.cap,
        &quota.notification_thresholds,
        marker,
    );
    // @cpt-begin:cpt-cf-quota-enforcement-algo-threshold-emission:p1:inst-thr-marker
    let next_marker = crossing
        .as_ref()
        .map(|c| i16::from(c.highest_crossed_threshold))
        .or_else(|| marker.map(i16::from));
    // @cpt-end:cpt-cf-quota-enforcement-algo-threshold-emission:p1:inst-thr-marker
    let stored = i64::try_from(post).map_err(|_| overflow())?;
    let written = match period_id {
        Some(period_id) => {
            counter_repo::write_counter(tx, scope, period_id, version, stored, next_marker, now)
                .await?
        }
        None => {
            counter_repo::write_allocation(
                tx,
                scope,
                quota.id.as_uuid(),
                version,
                stored,
                next_marker,
                now,
            )
            .await?
        }
    };
    if !written {
        return Err(TxError::Storage(StorageError::Internal(
            "the locked counter row moved before the write".to_owned(),
        )));
    }
    // @cpt-begin:cpt-cf-quota-enforcement-algo-threshold-emission:p1:inst-thr-settle-if
    // @cpt-begin:cpt-cf-quota-enforcement-algo-threshold-emission:p1:inst-thr-settle-skip
    // @cpt-begin:cpt-cf-quota-enforcement-algo-threshold-emission:p1:inst-thr-emit
    if let Some(crossing) = &crossing
        && !silent
    {
        let event = quota_event(
            actor,
            quota,
            NotificationEventKind::ThresholdCrossed,
            json!({
                "crossed_thresholds": crossing.crossed_thresholds,
                "highest_crossed_threshold": crossing.highest_crossed_threshold,
                "consumed": post,
                "cap": quota.cap,
            }),
            now,
        );
        enqueuer
            .enqueue_all(tx, std::slice::from_ref(&event))
            .await?;
    }
    // @cpt-end:cpt-cf-quota-enforcement-algo-threshold-emission:p1:inst-thr-emit
    // @cpt-end:cpt-cf-quota-enforcement-algo-threshold-emission:p1:inst-thr-settle-skip
    // @cpt-end:cpt-cf-quota-enforcement-algo-threshold-emission:p1:inst-thr-settle-if
    Ok((
        AppliedEntry {
            quota_id: quota.id.as_uuid(),
            period_id,
            amount,
            value: post,
            chain: 0,
            position: 0,
        },
        if silent { None } else { crossing },
    ))
}

async fn current_marker(
    tx: &impl DBRunner,
    scope: &AccessScope,
    quota: &Quota,
    period_id: Option<Uuid>,
    wait: RowWait,
) -> Result<Option<u8>, TxError> {
    let raw = match period_id {
        Some(period_id) => counter_repo::find_by_period_id_for_update(tx, scope, period_id, wait)
            .await?
            .and_then(|row| row.highest_crossed_threshold_pct),
        None => counter_repo::find_allocation_for_update(tx, scope, quota.id.as_uuid(), wait)
            .await?
            .and_then(|row| row.highest_crossed_threshold_pct),
    };
    Ok(raw.and_then(|value| u8::try_from(value).ok()))
}

/// Lower one counter, flooring at zero. A downward move emits no threshold: the
/// marker only advances, so a threshold crossed once stays crossed until the
/// period rolls over.
pub(super) async fn credit_counter(
    tx: &impl DBRunner,
    scope: &AccessScope,
    quota_id: Uuid,
    period_id: Option<Uuid>,
    amount: u64,
    now: OffsetDateTime,
    wait: RowWait,
) -> Result<u64, TxError> {
    let (value, version, marker) = if let Some(period_id) = period_id {
        let row = counter_repo::find_by_period_id_for_update(tx, scope, period_id, wait)
            .await?
            .ok_or_else(|| {
                TxError::Storage(StorageError::Internal(
                    "the period row to credit is missing".to_owned(),
                ))
            })?;
        (
            row.consumed,
            row.record_version,
            row.highest_crossed_threshold_pct,
        )
    } else {
        let row = counter_repo::find_allocation_for_update(tx, scope, quota_id, wait)
            .await?
            .ok_or_else(|| {
                TxError::Storage(StorageError::Internal(
                    "the allocation counter to credit is missing".to_owned(),
                ))
            })?;
        (
            row.in_flight,
            row.record_version,
            row.highest_crossed_threshold_pct,
        )
    };
    // Reconcile before crediting: flooring at zero against a counter that still
    // carries an expired hold would lose the difference when the sweep arrives.
    let expired = return_expired_holds(tx, scope, quota_id, period_id, now).await?;
    let pre = u64::try_from(value).unwrap_or(0).saturating_sub(expired);
    if expired == 0 && amount == 0 {
        return Ok(pre);
    }
    let post = pre.saturating_sub(amount);
    let stored = i64::try_from(post).unwrap_or(0);
    let written = match period_id {
        Some(period_id) => {
            counter_repo::write_counter(tx, scope, period_id, version, stored, marker, now).await?
        }
        None => {
            counter_repo::write_allocation(tx, scope, quota_id, version, stored, marker, now)
                .await?
        }
    };
    if !written {
        return Err(TxError::Storage(StorageError::Internal(
            "the locked counter row moved before the credit".to_owned(),
        )));
    }
    Ok(post)
}

// ---------------------------------------------------------------------------
// Idempotency helpers
// ---------------------------------------------------------------------------

pub(super) fn scope_key_of(scope_of: &IdempotencyScope) -> idem_repo::ScopeKey<'_> {
    idem_repo::ScopeKey {
        tenant_id: scope_of.tenant_id.as_uuid(),
        operation_type: scope_of.operation_type.as_str(),
        idem_key: &scope_of.key,
    }
}

/// The decision blob a record stores: the decision plus its schema version,
/// which the decision's own deserializer ignores when reading it back.
pub(super) fn versioned_blob(decision: &Decision) -> Result<String, TxError> {
    let mut blob = serde_json::to_value(decision)?;
    if let Value::Object(map) = &mut blob {
        map.insert("__version".to_owned(), Value::from(DECISION_BLOB_VERSION));
    }
    Ok(serde_json::to_string(&blob)?)
}

pub(super) fn decision_of(blob: &str) -> Result<Decision, TxError> {
    Ok(serde_json::from_str(blob)?)
}

/// What answers a repeated request: a charge or an idempotency record.
pub(super) struct StoredRecord {
    pub(super) payload_hash: Vec<u8>,
    pub(super) blob: String,
    engine_id: Option<String>,
    policy_id: Option<String>,
    policy_version: Option<i32>,
    created_at: OffsetDateTime,
    /// For a charge, the end of its return window.
    pub(super) expires_at: OffsetDateTime,
}

impl From<charge::Model> for StoredRecord {
    fn from(row: charge::Model) -> Self {
        Self {
            payload_hash: row.payload_hash,
            blob: row.decision_blob,
            engine_id: row.engine_id,
            policy_id: row.policy_id,
            policy_version: row.policy_version,
            created_at: row.created_at,
            expires_at: row.return_window_end,
        }
    }
}

impl From<idempotency_row::Model> for StoredRecord {
    fn from(row: idempotency_row::Model) -> Self {
        Self {
            payload_hash: row.payload_hash,
            blob: row.decision_blob,
            engine_id: row.engine_id,
            policy_id: row.policy_id,
            policy_version: row.policy_version,
            created_at: row.created_at,
            expires_at: row.expires_at,
        }
    }
}

/// The record under `of`. A debit or a commit is answered by its charge for as
/// long as the charge lives, otherwise by its unexpired idempotency record.
pub(super) async fn find_stored(
    runner: &impl DBRunner,
    scope: &AccessScope,
    of: &IdempotencyScope,
    now: OffsetDateTime,
    lock: Option<RowWait>,
) -> Result<Option<StoredRecord>, ScopeError> {
    let key = scope_key_of(of);
    // @cpt-begin:cpt-cf-quota-enforcement-algo-idempotency-replay:p1:inst-idem-window
    if matches!(
        of.operation_type,
        OperationType::Debit | OperationType::Commit
    ) && let Some(charge) = charge_repo::find(runner, scope, &key, now).await?
    {
        return Ok(Some(charge.into()));
    }
    // @cpt-end:cpt-cf-quota-enforcement-algo-idempotency-replay:p1:inst-idem-window
    Ok(idem_repo::find(runner, scope, &key, now, lock)
        .await?
        .map(StoredRecord::from))
}

/// What an existing record says about this attempt.
pub(super) enum Replay {
    /// No record: the operation is new.
    Fresh,
    /// The same payload was recorded; return what it decided.
    Stored(Box<StoredRecord>),
}

use super::entity::{charge, charge_entry, idempotency_record as idempotency_row};

/// Look up the record under `write`'s scope, refusing a divergent payload.
pub(super) async fn replay_of(
    tx: &impl DBRunner,
    scope: &AccessScope,
    write: &IdempotencyWrite,
    now: OffsetDateTime,
    lock: Option<RowWait>,
) -> Result<Replay, TxError> {
    let Some(stored) = find_stored(tx, scope, &write.scope, now, lock).await? else {
        return Ok(Replay::Fresh);
    };
    if stored.payload_hash != write.payload_hash.as_bytes() {
        return Err(StorageError::IdempotencyPayloadMismatch.into());
    }
    Ok(Replay::Stored(Box::new(stored)))
}

/// Retention deadline of a record written now for `(tenant, metric)`; for a
/// charge, the end of its return window.
pub(super) fn expires_at(
    config: &ConfigSnapshot,
    tenant: TenantId,
    metric: &str,
    now: OffsetDateTime,
) -> OffsetDateTime {
    now + config.idempotency_retention(tenant, metric)
}

/// Everything a record write needs beyond its scope.
pub(super) struct RecordWrite<'a> {
    pub(super) write: &'a IdempotencyWrite,
    /// What a replay answers with. A denied debit or a credit records its
    /// decision; an acquisition records its whole outcome,
    /// because a replay must return the token it issued and not merely the
    /// verdict.
    pub(super) blob: RecordBlob<'a>,
    pub(super) policy: Option<&'a PolicyVersion>,
    pub(super) expires_at: OffsetDateTime,
    pub(super) now: OffsetDateTime,
}

/// What a record stores under its key.
pub(super) enum RecordBlob<'a> {
    /// The transaction's decision, wrapped with its schema version.
    Decision(&'a Decision),
    /// A document the caller already serialized, version and all.
    Verbatim(&'a str),
}

/// Insert the record, reporting a lost primary-key race as [`TxError::Raced`]
/// so the whole transaction rolls back.
///
/// An expired row under the same key is deleted first: a replay past the
/// retention window is a new operation, so the key must be free for it.
pub(super) async fn write_record(
    tx: &impl DBRunner,
    scope: &AccessScope,
    record: &RecordWrite<'_>,
) -> Result<(), TxError> {
    let key = scope_key_of(&record.write.scope);
    idem_repo::delete_expired_at_key(tx, scope, &key, record.now).await?;
    let new = idem_repo::NewRecord {
        key,
        payload_hash: record.write.payload_hash.as_bytes(),
        decision_blob: match record.blob {
            RecordBlob::Decision(decision) => versioned_blob(decision)?,
            RecordBlob::Verbatim(blob) => blob.to_owned(),
        },
        engine_id: record.policy.map(|p| p.engine_id.clone()),
        policy_id: record.policy.map(|p| p.policy_id.to_string()),
        policy_version: record.policy.and_then(|p| i32::try_from(p.version).ok()),
        created_at: record.now,
        expires_at: record.expires_at,
    };
    // @cpt-begin:cpt-cf-quota-enforcement-algo-idempotency-replay:p1:inst-idem-persist
    // @cpt-begin:cpt-cf-quota-enforcement-state-idempotency-record:p1:inst-idemst-persist
    match idem_repo::insert(tx, scope, &new).await? {
        // @cpt-end:cpt-cf-quota-enforcement-state-idempotency-record:p1:inst-idemst-persist
        // @cpt-end:cpt-cf-quota-enforcement-algo-idempotency-replay:p1:inst-idem-persist
        idem_repo::Inserted::Yes => Ok(()),
        idem_repo::Inserted::Raced => Err(TxError::Raced(Box::new(record.write.clone()))),
    }
}

/// Everything the charge of an allowed debit or a lease commit records.
pub(super) struct ChargeWrite<'a> {
    pub(super) write: &'a IdempotencyWrite,
    pub(super) decision: &'a Decision,
    pub(super) policy: Option<&'a PolicyVersion>,
    pub(super) authorized: AttributionDigest,
    pub(super) metric: &'a str,
    pub(super) amount: u64,
    pub(super) entries: &'a [AppliedEntry],
    pub(super) return_window_end: OffsetDateTime,
    pub(super) now: OffsetDateTime,
}

/// Write the charge, which is the operation's idempotency record (ADR-0011),
/// in the transaction of its counters. A deleted charge under the scope is
/// replaced; a live one is [`TxError::Raced`].
pub(super) async fn write_charge(
    tx: &impl DBRunner,
    scope: &AccessScope,
    charge: &ChargeWrite<'_>,
) -> Result<(), TxError> {
    let (row, entries) = charge_rows(charge)?;
    charge_repo::delete_deleted_at_key(tx, scope, &scope_key_of(&charge.write.scope), charge.now)
        .await?;
    match charge_repo::insert(tx, scope, row, entries).await? {
        idem_repo::Inserted::Yes => Ok(()),
        idem_repo::Inserted::Raced => Err(TxError::Raced(Box::new(charge.write.clone()))),
    }
}

/// The rows of `charge`: the charge and its entries, before the holding flags
/// and the expiry are set.
pub(super) fn charge_rows(
    charge: &ChargeWrite<'_>,
) -> Result<(charge::Model, Vec<charge_entry::Model>), TxError> {
    let column = |what: &str| {
        TxError::Storage(StorageError::Internal(format!(
            "the {what} of a charge does not fit its column"
        )))
    };
    let id = Uuid::now_v7();
    let tenant_id = charge.write.scope.tenant_id.as_uuid();
    let mut entries = Vec::with_capacity(charge.entries.len());
    for entry in charge.entries {
        entries.push(charge_entry::Model {
            charge_id: id,
            chain: i32::try_from(entry.chain).map_err(|_| column("chain"))?,
            position: i32::try_from(entry.position).map_err(|_| column("position"))?,
            tenant_id,
            quota_id: entry.quota_id,
            period_id: entry.period_id,
            charged: i64::try_from(entry.amount).map_err(|_| column("charged amount"))?,
            returned: 0,
            holding: false,
        });
    }
    let row = charge::Model {
        id,
        tenant_id,
        operation: charge.write.scope.operation_type.as_str().to_owned(),
        idem_key: charge.write.scope.key.clone(),
        payload_hash: charge.write.payload_hash.as_bytes().to_vec(),
        attribution_digest: charge.authorized.as_bytes().to_vec(),
        decision_blob: versioned_blob(charge.decision)?,
        engine_id: charge.policy.map(|p| p.engine_id.clone()),
        policy_id: charge.policy.map(|p| p.policy_id.to_string()),
        policy_version: charge.policy.and_then(|p| i32::try_from(p.version).ok()),
        metric: charge.metric.to_owned(),
        amount: i64::try_from(charge.amount).map_err(|_| column("amount"))?,
        created_at: charge.now,
        return_window_end: charge.return_window_end,
        kept_until: None,
        expires_at: None,
    };
    Ok((row, entries))
}

fn record_to_model(
    scope_of: &IdempotencyScope,
    row: StoredRecord,
) -> Result<IdempotencyRecord, TxError> {
    let payload_hash: [u8; 32] = row.payload_hash.try_into().map_err(|_| {
        TxError::Storage(StorageError::Internal(
            "idempotency record has an unreadable payload hash".to_owned(),
        ))
    })?;
    Ok(IdempotencyRecord {
        scope: scope_of.clone(),
        payload_hash: PayloadHash::from_bytes(payload_hash),
        decision_blob: serde_json::from_str(&row.blob)?,
        engine_id: row.engine_id,
        policy_id: row.policy_id.map(quota_enforcement_sdk::PolicyId::new),
        policy_version: row.policy_version.and_then(|v| u32::try_from(v).ok()),
        created_at: row.created_at,
        expires_at: row.expires_at,
    })
}

// ---------------------------------------------------------------------------
// The store
// ---------------------------------------------------------------------------

impl SqlConsumptionStore {
    /// Bind the database and the same-transaction event enqueuer.
    #[must_use]
    pub fn new(db: Db, enqueuer: Arc<dyn NotificationOutbox>) -> Self {
        Self::with_clock(db, enqueuer, Arc::new(OffsetDateTime::now_utc))
    }

    /// The same store on a caller-driven clock. Tests move period boundaries
    /// and retention deadlines this way instead of waiting for them.
    #[must_use]
    pub fn with_clock(db: Db, enqueuer: Arc<dyn NotificationOutbox>, clock: Clock) -> Self {
        Self {
            db,
            enqueuer,
            clock,
            config: Arc::new(ConfigCache::new(DEFAULT_REFRESH)),
        }
    }

    /// The same store, reading the configuration through `config`.
    #[must_use]
    pub fn with_config(mut self, config: Arc<ConfigCache>) -> Self {
        self.config = config;
        self
    }

    fn now(&self) -> OffsetDateTime {
        (self.clock)()
    }

    /// The configuration snapshot, reloaded once the refresh interval passed.
    pub(super) async fn config(&self) -> Result<Arc<ConfigSnapshot>, StorageError> {
        self.config.read(&self.db, self.now()).await
    }

    /// The Quotas of `mutation`'s applicable set, locked ascending by id
    /// (ADR-0002), that are active and whose conditions hold on its request
    /// (I15). The conditions are judged on the locked rows.
    pub(super) async fn lock_applicable(
        tx: &impl DBRunner,
        scope: &AccessScope,
        mutation: &OwnedMutation,
        wait: RowWait,
    ) -> Result<Vec<Quota>, TxError> {
        let applicable = &mutation.applicable;
        let subjects: Vec<(String, String)> = applicable
            .subjects
            .iter()
            .map(|s| (s.subject_type.to_string(), s.subject_id.clone()))
            .collect();
        let ids = quota_repo::find_applicable_ids(
            tx,
            scope,
            applicable.tenant_id.as_uuid(),
            applicable.metric.as_str(),
            &subjects,
        )
        .await?;
        // @cpt-begin:cpt-cf-quota-enforcement-algo-evaluation-pipeline:p1:inst-pipe-lockread
        let mut quotas = Vec::with_capacity(ids.len());
        for id in ids {
            if let Some(row) = quota_repo::find_by_id(tx, scope, id, Some(wait)).await? {
                let quota = quota_mapping::row_to_quota(row)?;
                if quota.status == QuotaStatus::Active
                    && conditions_hold(&quota.metadata, &mutation.request)
                {
                    quotas.push(quota);
                }
            }
        }
        // @cpt-end:cpt-cf-quota-enforcement-algo-evaluation-pipeline:p1:inst-pipe-lockread
        Ok(quotas)
    }

    /// Select the policy this transaction evaluates: the metric's own when one
    /// is active, the seeded global otherwise. Selection happens here, under
    /// the same locks as the mutation, never in the caller.
    async fn select_policy(
        tx: &impl DBRunner,
        metric: &quota_enforcement_sdk::MetricId,
    ) -> Result<PolicyVersion, TxError> {
        let metric_scope = scope_key(&PolicyScope::Metric {
            metric: metric.clone(),
        });
        if let Some(row) = policy_repo::active_at_scope(tx, &metric_scope).await? {
            return Ok(decode_version(&row)?);
        }
        let global = scope_key(&PolicyScope::Global);
        let row = policy_repo::active_at_scope(tx, &global)
            .await?
            .ok_or_else(|| {
                TxError::Storage(StorageError::Internal(
                    "no active global resolution policy is seeded".to_owned(),
                ))
            })?;
        Ok(decode_version(&row)?)
    }

    /// Evaluate the selected policy against the locked Quota rows, reading
    /// their counters without locking them. Lease acquisition evaluates this
    /// way: it takes its capacity row (rank 4) only after an allowed verdict,
    /// so it may not hold counter rows (rank 5) before it.
    pub(super) async fn evaluate(
        tx: &impl DBRunner,
        scope: &AccessScope,
        quotas: &[Quota],
        mutation: &OwnedMutation,
        now: OffsetDateTime,
    ) -> Result<(PolicyVersion, Decision), TxError> {
        let mut snapshots = Vec::with_capacity(quotas.len());
        for quota in quotas {
            snapshots.push(snapshot_of(tx, scope, quota, now).await?);
        }
        Self::decide(tx, &snapshots, mutation, now, None).await
    }

    /// [`Self::evaluate`] over counter rows it locks first (rank 5), so the
    /// counter and the expired-hold correction come from one state: every
    /// writer that lowers a counter holds that row before stamping a hold.
    pub(super) async fn evaluate_locked(
        tx: &impl DBRunner,
        scope: &AccessScope,
        quotas: &[Quota],
        mutation: &OwnedMutation,
        now: OffsetDateTime,
    ) -> Result<(PolicyVersion, Decision), TxError> {
        let mut locked = Vec::with_capacity(quotas.len());
        for quota in quotas {
            locked.push(lock_counters_of(tx, scope, quota, now, RowWait::Nowait).await?);
        }
        let mut snapshots = Vec::with_capacity(quotas.len());
        for (quota, counter) in quotas.iter().zip(&locked) {
            snapshots.push(snapshot_of_locked(tx, scope, quota, counter, now).await?);
        }
        Self::decide(tx, &snapshots, mutation, now, None).await
    }

    /// Run the policy of `mutation`'s metric over `snapshots`. `budget`
    /// replaces the policy's own timeout when given: a batch item runs on
    /// what remains of the batch timer.
    pub(super) async fn decide(
        tx: &impl DBRunner,
        snapshots: &[QuotaSnapshot],
        mutation: &OwnedMutation,
        now: OffsetDateTime,
        budget: Option<quota_enforcement_sdk::engine::EvaluationBudget>,
    ) -> Result<(PolicyVersion, Decision), TxError> {
        let policy = Self::select_policy(tx, &mutation.applicable.metric).await?;
        let evaluation: Vec<EvaluationQuota<'_>> = snapshots
            .iter()
            .map(|snapshot| EvaluationQuota {
                snapshot,
                tier: QuotaScopeTier::of(&snapshot.subject),
            })
            .collect();
        // Resolve the budget from the version selected by this transaction,
        // unless the caller fixed it.
        let budget = match budget {
            Some(budget) => budget,
            None => mutation.limits.budget(policy.timeout_ms).map_err(|error| {
                TxError::Storage(StorageError::EvaluationFailed {
                    engine_id: policy.engine_id.clone(),
                    failure: error.into(),
                })
            })?,
        };
        let context = EvaluationContext {
            policy: &policy,
            metric: &mutation.applicable.metric,
            amount: mutation.amount,
            time: now,
            quotas: &evaluation,
            request: &mutation.request,
            metric_id: mutation.metric_id.as_deref(),
            budget,
        };
        // @cpt-begin:cpt-cf-quota-enforcement-algo-evaluation-pipeline:p1:inst-pipe-engine
        // @cpt-begin:cpt-cf-quota-enforcement-algo-evaluation-pipeline:p1:inst-pipe-fail-if
        let outcome = (mutation.evaluate)(&context).map_err(|failure| match failure {
            EvaluationFailure::PreparationRequired { policy_id, version } => {
                TxError::Storage(StorageError::PreparationRequired { policy_id, version })
            }
            failure => TxError::Storage(StorageError::EvaluationFailed {
                engine_id: policy.engine_id.clone(),
                failure,
            }),
        })?;
        // @cpt-end:cpt-cf-quota-enforcement-algo-evaluation-pipeline:p1:inst-pipe-fail-if
        // @cpt-end:cpt-cf-quota-enforcement-algo-evaluation-pipeline:p1:inst-pipe-engine
        Ok((policy, outcome.into_decision()))
    }

    /// Apply a validated plan: settle what the plan's Quotas outgrew, then move
    /// each counter, in ascending `quota_id` order (ADR-0002).
    pub(super) async fn apply_plan(
        tx: &impl DBRunner,
        scope: &AccessScope,
        actor: &Actor,
        quotas: &[Quota],
        decision: &Decision,
        now: OffsetDateTime,
        enqueuer: &Arc<dyn NotificationEnqueuer>,
    ) -> Result<Applied, TxError> {
        let mut entries = Vec::new();
        let mut crossings = Vec::new();
        let mut planned: Vec<_> = plan_entries(&decision.debit_plan).collect();
        planned.sort_by_key(|(_, plan)| plan.quota_id);
        for ((chain, position), plan) in planned {
            let quota_id = plan.quota_id;
            let quota = quotas
                .iter()
                .find(|quota| quota.id == quota_id)
                .ok_or_else(|| {
                    TxError::Storage(StorageError::Internal(format!(
                        "the plan names quota {quota_id}, which is not applicable"
                    )))
                })?;
            if quota.quota_type == QuotaType::Consumption {
                settle_elapsed_rows(tx, scope, actor, quota, now, enqueuer, RowWait::Nowait)
                    .await?;
            }
            // @cpt-begin:cpt-cf-quota-enforcement-algo-evaluation-pipeline:p1:inst-pipe-apply
            let (entry, crossing) = debit_counter(
                tx,
                scope,
                actor,
                quota,
                plan.amount,
                now,
                enqueuer,
                RowWait::Nowait,
            )
            .await?;
            // @cpt-end:cpt-cf-quota-enforcement-algo-evaluation-pipeline:p1:inst-pipe-apply
            entries.push(AppliedEntry {
                chain,
                position,
                ..entry
            });
            crossings.extend(crossing);
        }
        Ok(Applied { entries, crossings })
    }
}

/// One Quota's snapshot, reading its current period without materializing one.
async fn snapshot_of(
    tx: &impl DBRunner,
    scope: &AccessScope,
    quota: &Quota,
    now: OffsetDateTime,
) -> Result<QuotaSnapshot, TxError> {
    // A read may not write (I3), so it subtracts the expired holds nobody has
    // returned yet rather than returning them: an expired lease stops counting
    // against capacity at its TTL, whether or not a sweeper has been by (I4).
    let counter = if quota.quota_type == QuotaType::Consumption {
        let latest = counter_repo::find_latest(tx, scope, quota.id.as_uuid()).await?;
        LockedCounter::consumption(quota, latest, now)
    } else {
        let row = counter_repo::find_allocation(tx, scope, quota.id.as_uuid()).await?;
        LockedCounter::allocation(row.map(|row| row.in_flight))
    };
    snapshot_of_locked(tx, scope, quota, &counter, now).await
}

/// What a Quota's counter holds: the stored value of its current period (or
/// its allocation row), which period that is, and the window it falls in.
pub(super) struct LockedCounter {
    stored: u64,
    period_id: Option<Uuid>,
    window: Option<PeriodWindow>,
}

impl LockedCounter {
    pub(super) fn consumption(
        quota: &Quota,
        latest: Option<quota_consumption_counter::Model>,
        now: OffsetDateTime,
    ) -> Self {
        let current = latest.filter(|row| row.period_start <= now && now < row.period_end);
        Self {
            stored: current
                .as_ref()
                .map_or(0, |row| u64::try_from(row.consumed).unwrap_or(0)),
            period_id: current.as_ref().map(|row| row.period_id),
            window: Some(window_of(quota, now)),
        }
    }

    pub(super) fn allocation(in_flight: Option<i64>) -> Self {
        Self {
            stored: in_flight.map_or(0, |value| u64::try_from(value).unwrap_or(0)),
            period_id: None,
            window: None,
        }
    }

    /// The current period row this counter reads, `None` for an allocation
    /// row or a period with no row yet.
    pub(super) fn period_id(&self) -> Option<Uuid> {
        self.period_id
    }
}

/// Lock every counter row of `quota` that a write in this transaction can
/// touch, in the order every writer takes them: its elapsed unsettled periods
/// oldest first (what settlement locks), then its latest period whatever its
/// state (what materializing a successor locks), or its allocation row.
/// Nothing is written: a missing current period reads as zero.
pub(super) async fn lock_counters_of(
    tx: &impl DBRunner,
    scope: &AccessScope,
    quota: &Quota,
    now: OffsetDateTime,
    wait: RowWait,
) -> Result<LockedCounter, TxError> {
    if quota.quota_type == QuotaType::Consumption {
        counter_repo::find_elapsed_unsettled_for_update(tx, scope, quota.id.as_uuid(), now, wait)
            .await?;
        let latest =
            counter_repo::find_latest_for_update(tx, scope, quota.id.as_uuid(), wait).await?;
        Ok(LockedCounter::consumption(quota, latest, now))
    } else {
        let row =
            counter_repo::find_allocation_for_update(tx, scope, quota.id.as_uuid(), wait).await?;
        Ok(LockedCounter::allocation(row.map(|row| row.in_flight)))
    }
}

/// The snapshot of `quota` from its `counter`, less the expired holds nobody
/// has returned yet: the per-Quota state every evaluation sees.
pub(super) async fn snapshot_of_locked(
    tx: &impl DBRunner,
    scope: &AccessScope,
    quota: &Quota,
    counter: &LockedCounter,
    now: OffsetDateTime,
) -> Result<QuotaSnapshot, TxError> {
    let owed = unreturned_expired(tx, scope, quota.id.as_uuid(), counter.period_id, now).await?;
    Ok(snapshot_from(quota, counter, owed, now))
}

/// The snapshot of `quota` from its `counter`, less `owed`, what its expired
/// holds still occupy: the per-Quota state every snapshot read returns.
// @cpt-algo:cpt-cf-quota-enforcement-algo-snapshot-state:p1
// @cpt-dod:cpt-cf-quota-enforcement-dod-snapshot-state-contract:p1
pub(super) fn snapshot_from(
    quota: &Quota,
    counter: &LockedCounter,
    owed: u64,
    now: OffsetDateTime,
) -> QuotaSnapshot {
    // @cpt-begin:cpt-cf-quota-enforcement-algo-snapshot-state:p1:inst-sst-identity
    let (quota_id, subject, metric, quota_type) = (
        quota.id,
        quota.subject.clone(),
        quota.metric.clone(),
        quota.quota_type,
    );
    // @cpt-end:cpt-cf-quota-enforcement-algo-snapshot-state:p1:inst-sst-identity
    // @cpt-begin:cpt-cf-quota-enforcement-algo-snapshot-state:p1:inst-sst-amounts
    // @cpt-begin:cpt-cf-quota-enforcement-algo-lazy-expiry:p1:inst-lzy-capacity
    let consumed = counter.stored.saturating_sub(owed);
    // @cpt-end:cpt-cf-quota-enforcement-algo-lazy-expiry:p1:inst-lzy-capacity
    let remaining = quota.cap.map(|cap| cap.saturating_sub(consumed));
    // @cpt-end:cpt-cf-quota-enforcement-algo-snapshot-state:p1:inst-sst-amounts
    // @cpt-begin:cpt-cf-quota-enforcement-algo-snapshot-state:p1:inst-sst-mode
    let enforcement_mode = quota.enforcement_mode;
    // @cpt-end:cpt-cf-quota-enforcement-algo-snapshot-state:p1:inst-sst-mode
    // @cpt-begin:cpt-cf-quota-enforcement-algo-snapshot-state:p1:inst-sst-period-if
    let period = if quota_type == QuotaType::Consumption {
        // @cpt-begin:cpt-cf-quota-enforcement-algo-snapshot-state:p1:inst-sst-period
        // The current window: its row's bounds when the row exists, the same
        // bounds computed from the period type when it does not yet.
        counter.window
        // @cpt-end:cpt-cf-quota-enforcement-algo-snapshot-state:p1:inst-sst-period
    } else {
        // @cpt-begin:cpt-cf-quota-enforcement-algo-snapshot-state:p1:inst-sst-noperiod-else
        // @cpt-begin:cpt-cf-quota-enforcement-algo-snapshot-state:p1:inst-sst-noperiod
        None
        // @cpt-end:cpt-cf-quota-enforcement-algo-snapshot-state:p1:inst-sst-noperiod
        // @cpt-end:cpt-cf-quota-enforcement-algo-snapshot-state:p1:inst-sst-noperiod-else
    };
    // @cpt-end:cpt-cf-quota-enforcement-algo-snapshot-state:p1:inst-sst-period-if
    // @cpt-begin:cpt-cf-quota-enforcement-algo-snapshot-state:p1:inst-sst-metadata
    let metadata = quota.metadata.clone();
    // @cpt-end:cpt-cf-quota-enforcement-algo-snapshot-state:p1:inst-sst-metadata
    // @cpt-begin:cpt-cf-quota-enforcement-algo-snapshot-state:p1:inst-sst-window
    let validity_window = quota.validity_window;
    let currently_within_window = validity_window.is_none_or(|window| window.contains(now));
    // @cpt-end:cpt-cf-quota-enforcement-algo-snapshot-state:p1:inst-sst-window
    // @cpt-begin:cpt-cf-quota-enforcement-algo-snapshot-state:p1:inst-sst-noattr
    // @cpt-begin:cpt-cf-quota-enforcement-algo-snapshot-state:p1:inst-sst-noheadline
    // @cpt-begin:cpt-cf-quota-enforcement-algo-snapshot-state:p1:inst-sst-return
    // One Quota's own state: the type has no field for policy attribution
    // and nothing here sums across Quotas.
    QuotaSnapshot {
        quota_id,
        subject,
        metric,
        quota_type,
        enforcement_mode,
        cap: quota.cap,
        consumed,
        remaining,
        period,
        metadata,
        validity_window,
        currently_within_window,
    }
    // @cpt-end:cpt-cf-quota-enforcement-algo-snapshot-state:p1:inst-sst-return
    // @cpt-end:cpt-cf-quota-enforcement-algo-snapshot-state:p1:inst-sst-noheadline
    // @cpt-end:cpt-cf-quota-enforcement-algo-snapshot-state:p1:inst-sst-noattr
}

pub(super) fn counters_of(entries: &[AppliedEntry]) -> Vec<CounterSnapshot> {
    entries
        .iter()
        .map(|entry| CounterSnapshot {
            quota_id: QuotaId::new(entry.quota_id),
            period_id: entry.period_id.map(PeriodId::new),
            value: entry.value,
        })
        .collect()
}

/// Who a transaction acts as, and what it reads the time and configuration
/// from and enqueues into.
#[derive(Clone, Copy)]
struct TxEnv<'a> {
    actor: &'a Actor,
    clock: &'a Clock,
    enqueuer: &'a Arc<dyn NotificationEnqueuer>,
    config: &'a ConfigSnapshot,
}

impl SqlConsumptionStore {
    /// One attempt of `apply_debit_plan`: lock, replay or evaluate, apply,
    /// record, all in the caller's transaction.
    async fn debit_in_tx(
        tx: &impl DBRunner,
        scope: &AccessScope,
        mutation: &OwnedMutation,
        events: &[NotificationEvent],
        env: &TxEnv<'_>,
    ) -> Result<TransitionOutcome<EvaluatedDebit>, TxError> {
        let TxEnv {
            actor,
            clock,
            enqueuer,
            config,
        } = *env;
        let quotas = Self::lock_applicable(tx, scope, mutation, RowWait::Nowait).await?;
        // Rank 2: the scope's stripe keeps out a writer of the
        // same key over other Quotas (I8).
        lock_scopes::<TxError>(tx, &[&mutation.idempotency.scope]).await?;
        // Sample time after locking so boundary waits charge the
        // period in which the transaction commits.
        let now = clock();
        if let Replay::Stored(row) =
            replay_of(tx, scope, &mutation.idempotency, now, Some(RowWait::Nowait)).await?
        {
            // Replays return the recorded decision without side effects.
            return Ok(TransitionOutcome::NoOp(EvaluatedDebit {
                decision: decision_of(&row.blob)?,
                mutation: MutationResult::default(),
                retention: Retention::Recorded {
                    expires_at: row.expires_at,
                },
            }));
        }
        let (policy, decision) = Self::evaluate_locked(tx, scope, &quotas, mutation, now).await?;
        if decision.denied_reason() == Some(NO_APPLICABLE_QUOTA) {
            // The one denial that records nothing at all:
            // provisioning a Quota must change the answer.
            return Ok(TransitionOutcome::Applied(EvaluatedDebit {
                decision,
                mutation: MutationResult::default(),
                retention: Retention::Unrecorded,
            }));
        }
        let expires_at = expires_at(
            config,
            mutation.applicable.tenant_id,
            mutation.applicable.metric.as_str(),
            now,
        );
        let applied = if matches!(decision.result, DecisionResult::Allowed) {
            let applied =
                Self::apply_plan(tx, scope, actor, &quotas, &decision, now, enqueuer).await?;
            write_charge(
                tx,
                scope,
                &ChargeWrite {
                    write: &mutation.idempotency,
                    decision: &decision,
                    policy: Some(&policy),
                    authorized: mutation.authorized,
                    metric: mutation.applicable.metric.as_str(),
                    amount: mutation.amount,
                    entries: &applied.entries,
                    return_window_end: expires_at,
                    now,
                },
            )
            .await?;
            applied
        } else {
            write_record(
                tx,
                scope,
                &RecordWrite {
                    write: &mutation.idempotency,
                    blob: RecordBlob::Decision(&decision),
                    policy: Some(&policy),
                    expires_at,
                    now,
                },
            )
            .await?;
            Applied {
                entries: Vec::new(),
                crossings: Vec::new(),
            }
        };
        let mut result = MutationResult {
            counters: counters_of(&applied.entries),
            threshold_crossings: applied.crossings,
            event_ids: Vec::new(),
        };
        if !applied.entries.is_empty() {
            for entry in &applied.entries {
                operation_log_repo::append(
                    tx,
                    scope,
                    Entry {
                        tenant_id: mutation.applicable.tenant_id.as_uuid(),
                        quota_id: entry.quota_id,
                        operation: operation_log_repo::OP_DEBIT,
                        actor,
                        record_version: 1,
                        detail: String::new(),
                        occurred_at: now,
                    },
                )
                .await?;
            }
            // Caller events belong to a mutation that happened.
            enqueuer.enqueue_all(tx, events).await?;
            result.event_ids = events.iter().map(|e| e.event_id).collect();
        }
        Ok(TransitionOutcome::Applied(EvaluatedDebit {
            decision,
            mutation: result,
            retention: Retention::Recorded { expires_at },
        }))
    }
}

#[async_trait::async_trait]
impl crate::domain::ports::ConsumptionStore for SqlConsumptionStore {
    async fn apply_debit_plan(
        &self,
        actor: &Actor,
        scope: &AccessScope,
        mutation: &EvaluatedMutation<'_>,
        events: &[NotificationEvent],
    ) -> Result<TransitionOutcome<EvaluatedDebit>, StorageError> {
        const OPERATION: &str = "apply debit plan";
        let actor = actor.clone();
        let config = self.config().await?;
        let budget = ContentionBudget::of(&config, [mutation.applicable.metric.as_str()]);
        // Each retry is a complete transaction.
        for attempt in 0..RACE_ATTEMPTS {
            // A refused lock rolls this transaction back and runs it
            // again under the budget; a race goes to the arbiter below.
            let result = with_budget(budget, || {
                let wakes = AttemptWakes::begin(&self.enqueuer);
                let enqueuer = wakes.enqueuer();
                let clock = Arc::clone(&self.clock);
                let actor = actor.clone();
                let scope = scope.clone();
                let events = events.to_vec();
                let mutation = OwnedMutation::of(mutation);
                let config = Arc::clone(&config);
                settling(
                    wakes,
                    self.db.transaction_ref_mapped(move |tx| {
                        Box::pin(async move {
                            let env = TxEnv {
                                actor: &actor,
                                clock: &clock,
                                enqueuer: &enqueuer,
                                config: &config,
                            };
                            Self::debit_in_tx(tx, &scope, &mutation, &events, &env).await
                        })
                    }),
                )
            })
            .await;
            match result {
                Ok(outcome) => return Ok(outcome),
                Err(TxError::Raced(lost)) => {
                    // The whole transaction rolled back. Resolve the winner's
                    // record; if it has since gone, run the operation again.
                    if let Some(record) = self.winner_of(&lost).await? {
                        return Ok(TransitionOutcome::NoOp(EvaluatedDebit {
                            decision: decision_of(&record.blob)
                                .map_err(|error| lift(OPERATION, error))?,
                            mutation: MutationResult::default(),
                            retention: Retention::Recorded {
                                expires_at: record.expires_at,
                            },
                        }));
                    }
                    if attempt + 1 == RACE_ATTEMPTS {
                        return Err(lift(OPERATION, TxError::Raced(lost)));
                    }
                }
                Err(error) => return Err(lift(OPERATION, error)),
            }
        }
        Err(lift(
            OPERATION,
            TxError::Raced(Box::new(mutation.idempotency.clone())),
        ))
    }

    async fn apply_batch_debit(
        &self,
        actor: &Actor,
        scope: &AccessScope,
        batch: &quota_enforcement_sdk::EvaluatedBatch<'_>,
        events: &[NotificationEvent],
    ) -> Result<TransitionOutcome<Vec<EvaluatedDebit>>, StorageError> {
        self.apply_batch_debit_impl(actor, scope, batch, events)
            .await
    }

    async fn apply_credit(
        &self,
        actor: &Actor,
        scope: &AccessScope,
        quota_id: QuotaId,
        amount: u64,
        idempotency: &PartialIdempotencyWrite,
        events: &[NotificationEvent],
    ) -> Result<TransitionOutcome<AppliedMutation>, StorageError> {
        const OPERATION: &str = "apply credit";
        let actor = actor.clone();
        let metric = self
            .metric_of(scope, quota_id.as_uuid())
            .await
            .map_err(|error| lift(OPERATION, error))?;
        let config = self.config().await?;
        let budget = ContentionBudget::of(&config, metric.as_deref());
        // The Quota row lock normally serializes credits; the arbiter is a
        // defensive fallback.
        for attempt in 0..RACE_ATTEMPTS {
            // A refused lock rolls this transaction back and runs it
            // again under the budget; a race goes to the arbiter below.
            let result = with_budget(budget, || {
                let actor = actor.clone();
                let wakes = AttemptWakes::begin(&self.enqueuer);
                let enqueuer = wakes.enqueuer();
                let clock = Arc::clone(&self.clock);
                let scope = scope.clone();
                let events = events.to_vec();
                let idempotency = idempotency.clone();
                let config = Arc::clone(&config);
                settling(
                    wakes,
                    self.db.transaction_ref_mapped(move |tx| {
                        Box::pin(async move {
                            let scope = &scope;
                            let events: &[NotificationEvent] = &events;
                            // Check replay before guards that apply only to fresh credits.
                            // @cpt-begin:cpt-cf-quota-enforcement-flow-credit:p1:inst-cre-lock
                            let row = quota_repo::find_by_id(
                                tx,
                                scope,
                                quota_id.as_uuid(),
                                Some(RowWait::Nowait),
                            )
                            .await?
                            .ok_or(StorageError::QuotaNotFound { id: quota_id })?;
                            let quota = quota_mapping::row_to_quota(row)?;
                            // @cpt-end:cpt-cf-quota-enforcement-flow-credit:p1:inst-cre-lock
                            // The row is locked now, so this is the instant the period
                            // guard and the record's retention are both keyed on.
                            let now = clock();
                            if quota.tenant_id != TenantId::new(idempotency.tenant_id.as_uuid()) {
                                return Err(StorageError::SubjectOutOfScope.into());
                            }
                            let write = idempotency.clone().complete(OperationType::Credit);
                            // Rank 2: the scope's stripe (I8).
                            lock_scopes::<TxError>(tx, &[&write.scope]).await?;
                            // @cpt-begin:cpt-cf-quota-enforcement-flow-credit:p1:inst-cre-idem
                            if let Replay::Stored(stored) =
                                replay_of(tx, scope, &write, now, Some(RowWait::Nowait)).await?
                            {
                                return Ok(TransitionOutcome::NoOp(AppliedMutation {
                                    decision: decision_of(&stored.blob)?,
                                    mutation: MutationResult::default(),
                                }));
                            }
                            // @cpt-end:cpt-cf-quota-enforcement-flow-credit:p1:inst-cre-idem
                            // @cpt-begin:cpt-cf-quota-enforcement-flow-credit:p1:inst-cre-guard-if
                            // @cpt-begin:cpt-cf-quota-enforcement-flow-credit:p1:inst-cre-guard
                            if quota.status == QuotaStatus::Deactivated {
                                return Err(StorageError::QuotaDeactivated { id: quota_id }.into());
                            }
                            let period_id = if quota.quota_type == QuotaType::Consumption {
                                // Credit uses calendar closure; materialize an absent
                                // current row.
                                let latest = counter_repo::find_latest_for_update(
                                    tx,
                                    scope,
                                    quota_id.as_uuid(),
                                    RowWait::Nowait,
                                )
                                .await?;
                                if latest.is_some_and(|row| now >= row.period_end) {
                                    return Err(StorageError::PeriodClosed.into());
                                }
                                // @cpt-end:cpt-cf-quota-enforcement-flow-credit:p1:inst-cre-guard
                                // @cpt-end:cpt-cf-quota-enforcement-flow-credit:p1:inst-cre-guard-if
                                let row =
                                    ensure_current_period(tx, scope, &quota, now, RowWait::Nowait)
                                        .await?;
                                settle_elapsed_rows(
                                    tx,
                                    scope,
                                    &actor,
                                    &quota,
                                    now,
                                    &enqueuer,
                                    RowWait::Nowait,
                                )
                                .await?;
                                Some(row.period_id)
                            } else {
                                None
                            };
                            // @cpt-begin:cpt-cf-quota-enforcement-flow-credit:p1:inst-cre-apply
                            let value = credit_counter(
                                tx,
                                scope,
                                quota_id.as_uuid(),
                                period_id,
                                amount,
                                now,
                                RowWait::Nowait,
                            )
                            .await?;
                            let entry = AppliedEntry {
                                quota_id: quota_id.as_uuid(),
                                period_id,
                                amount,
                                value,
                                chain: 0,
                                position: 0,
                            };
                            let decision = Decision::allowed_with_plan(plan_of([entry.planned()]));
                            let expires_at =
                                expires_at(&config, quota.tenant_id, quota.metric.as_str(), now);
                            // A credit evaluates no policy and writes no charge.
                            write_record(
                                tx,
                                scope,
                                &RecordWrite {
                                    write: &write,
                                    blob: RecordBlob::Decision(&decision),
                                    policy: None,
                                    expires_at,
                                    now,
                                },
                            )
                            .await?;
                            operation_log_repo::append(
                                tx,
                                scope,
                                Entry {
                                    tenant_id: quota.tenant_id.as_uuid(),
                                    quota_id: quota_id.as_uuid(),
                                    operation: operation_log_repo::OP_CREDIT,
                                    actor: &actor,
                                    record_version: 1,
                                    detail: String::new(),
                                    occurred_at: now,
                                },
                            )
                            .await?;
                            let adjusted = quota_event(
                                &actor,
                                &quota,
                                NotificationEventKind::QuotaCounterAdjusted,
                                // The consumer identity the operation was authorized
                                // under travels with the event, not only to the log:
                                // a sink has to answer who adjusted the counter.
                                json!({
                                    "credited_amount": amount,
                                    "quota_id": quota_id,
                                    "principal": actor.to_string(),
                                }),
                                now,
                            );
                            enqueuer
                                .enqueue_all(tx, std::slice::from_ref(&adjusted))
                                .await?;
                            enqueuer.enqueue_all(tx, events).await?;
                            // @cpt-end:cpt-cf-quota-enforcement-flow-credit:p1:inst-cre-apply
                            Ok::<_, TxError>(TransitionOutcome::Applied(AppliedMutation {
                                decision,
                                mutation: MutationResult {
                                    counters: counters_of(std::slice::from_ref(&entry)),
                                    threshold_crossings: Vec::new(),
                                    event_ids: events.iter().map(|e| e.event_id).collect(),
                                },
                            }))
                        })
                    }),
                )
            })
            .await;
            match result {
                Ok(outcome) => return Ok(outcome),
                Err(TxError::Raced(lost)) => {
                    if let Some(outcome) = self.applied_winner(OPERATION, &lost).await? {
                        return Ok(outcome);
                    }
                    if attempt + 1 == RACE_ATTEMPTS {
                        return Err(lift(OPERATION, TxError::Raced(lost)));
                    }
                }
                Err(error) => return Err(lift(OPERATION, error)),
            }
        }
        Err(unavailable(
            OPERATION,
            "idempotency arbiter",
            &"the key is being written concurrently",
        ))
    }

    async fn apply_return(
        &self,
        actor: &Actor,
        scope: &AccessScope,
        ret: &ChargeReturn,
        events: &[NotificationEvent],
    ) -> Result<TransitionOutcome<AppliedMutation>, StorageError> {
        self.apply_return_impl(actor, scope, ret, events).await
    }

    async fn apply_rollback(
        &self,
        actor: &Actor,
        scope: &AccessScope,
        target: &RollbackTarget,
        events: &[NotificationEvent],
    ) -> Result<TransitionOutcome<AppliedMutation>, StorageError> {
        self.apply_rollback_impl(actor, scope, target, events).await
    }

    // The counter and the expired holds that correct it are two statements, so
    // they are read under `RepeatableRead`: at `ReadCommitted` a sweeper
    // committing between them would pair a pre-return counter with a
    // post-return correction and over-report usage, denying work that fits.
    async fn read_quota_snapshot(
        &self,
        scope: &AccessScope,
        applicable: &ApplicableQuotas,
    ) -> Result<Vec<QuotaSnapshot>, StorageError> {
        self.read_quota_snapshot_impl(scope, applicable).await
    }

    async fn bulk_read_quota_snapshot(
        &self,
        scope: &AccessScope,
        pairs: &[ApplicableQuotas],
        page: PageRequest,
        in_window_only: bool,
    ) -> Result<PageResult<QuotaSnapshot>, StorageError> {
        self.bulk_read_quota_snapshot_impl(scope, pairs, page, in_window_only)
            .await
    }

    async fn lookup_idempotency(
        &self,
        scope_of: &IdempotencyScope,
    ) -> Result<Option<IdempotencyRecord>, StorageError> {
        const OPERATION: &str = "lookup idempotency";
        let now = self.now();
        // A lookup is the caller's own key; the tenant scope it carries is the
        // one the PDP authorized for this operation.
        let scope = AccessScope::allow_all();
        let conn = self
            .db
            .conn()
            .map_err(|error| unavailable(OPERATION, "connection", &error))?;
        // @cpt-begin:cpt-cf-quota-enforcement-algo-idempotency-replay:p1:inst-idem-lookup
        let row = find_stored(&conn, &scope, scope_of, now, None)
            .await
            .map_err(|error| lift(OPERATION, error.into()))?;
        // @cpt-end:cpt-cf-quota-enforcement-algo-idempotency-replay:p1:inst-idem-lookup
        row.map(|row| record_to_model(scope_of, row))
            .transpose()
            .map_err(|error| lift(OPERATION, error))
    }

    async fn reclaim_expired_idempotency(
        &self,
        batch_size: u32,
        before: OffsetDateTime,
    ) -> Result<u64, StorageError> {
        const OPERATION: &str = "reclaim idempotency";
        // One transaction per batch, under the writers' protocol: the scopes'
        // stripes first — skipped, not waited on, while a writer holds one —
        // then a delete that re-checks the expiry, so a record a writer put
        // in place of an expired one since the selection survives. Charges
        // live in their own table and are never touched here.
        self.db
            .transaction_ref_mapped(move |tx| {
                Box::pin(async move {
                    let all = AccessScope::allow_all();
                    let doomed = idem_repo::select_expired(tx, &all, batch_size, before).await?;
                    let mut keys: Vec<_> = doomed
                        .iter()
                        .map(
                            |(tenant_id, operation_type, idem_key)| idem_repo::ScopeKey {
                                tenant_id: *tenant_id,
                                operation_type,
                                idem_key,
                            },
                        )
                        .collect();
                    let stripes: Vec<i32> = keys.iter().map(idem_repo::stripe_of).collect();
                    let locked = idem_repo::try_lock_stripes(tx, &stripes).await?;
                    keys.retain(|key| locked.contains(&idem_repo::stripe_of(key)));
                    Ok::<_, TxError>(idem_repo::delete_if_expired(tx, &all, &keys, before).await?)
                })
            })
            .await
            .map_err(|error| lift(OPERATION, error))
    }

    async fn reclaim_operation_log(
        &self,
        batch_size: u32,
        before: OffsetDateTime,
    ) -> Result<u64, StorageError> {
        const OPERATION: &str = "reclaim operation log";
        let conn = self
            .db
            .conn()
            .map_err(|error| unavailable(OPERATION, "connection", &error))?;
        operation_log_repo::reclaim_before(&conn, &AccessScope::allow_all(), batch_size, before)
            .await
            .map_err(|error| lift(OPERATION, error.into()))
    }

    async fn reclaim_expired_charges(
        &self,
        batch_size: u32,
        now: OffsetDateTime,
    ) -> Result<u64, StorageError> {
        self.reclaim_expired_charges_impl(batch_size, now).await
    }

    async fn read_held_allocation_charge_stats(
        &self,
        before: OffsetDateTime,
    ) -> Result<Vec<quota_enforcement_sdk::HeldAllocationCharges>, StorageError> {
        self.read_held_allocation_charge_stats_impl(before).await
    }
}

impl SqlConsumptionStore {
    /// The metric of a Quota, read without a lock: it names the contention
    /// budget, which has to be known before the first lock is taken. `None`
    /// for an unknown Quota, whose transaction then reports it properly.
    pub(super) async fn metric_of(
        &self,
        scope: &AccessScope,
        quota_id: Uuid,
    ) -> Result<Option<String>, TxError> {
        let conn = self.db.conn().map_err(TxError::Db)?;
        Ok(quota_repo::find_by_id(&conn, scope, quota_id, None)
            .await?
            .map(|row| row.metric))
    }

    /// After losing the record's primary key, read the record that won.
    ///
    /// `None` means the winner rolled back too, so the operation simply runs
    /// again. A record under the same scope carrying a different payload is
    /// the caller's conflict, not a race to retry.
    pub(super) async fn winner_of(
        &self,
        lost: &IdempotencyWrite,
    ) -> Result<Option<StoredRecord>, StorageError> {
        const OPERATION: &str = "resolve idempotency race";
        let now = self.now();
        let scope = AccessScope::allow_all();
        let conn = self
            .db
            .conn()
            .map_err(|error| unavailable(OPERATION, "connection", &error))?;
        let Some(row) = find_stored(&conn, &scope, &lost.scope, now, None)
            .await
            .map_err(|error| lift(OPERATION, error.into()))?
        else {
            return Ok(None);
        };
        if row.payload_hash != lost.payload_hash.as_bytes() {
            return Err(StorageError::IdempotencyPayloadMismatch);
        }
        Ok(Some(row))
    }

    /// The corrective primitives' half of the same reconciliation: their
    /// outcome type differs from a debit's, the resolution does not.
    async fn applied_winner(
        &self,
        operation: &'static str,
        lost: &IdempotencyWrite,
    ) -> Result<Option<TransitionOutcome<AppliedMutation>>, StorageError> {
        let Some(row) = self.winner_of(lost).await? else {
            return Ok(None);
        };
        let decision = decision_of(&row.blob).map_err(|error| lift(operation, error))?;
        Ok(Some(TransitionOutcome::NoOp(AppliedMutation {
            decision,
            mutation: MutationResult::default(),
        })))
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "consumption_store_tests.rs"]
pub(super) mod tests;
