//! The consumption primitives: debit, credit, rollback, the snapshot read, and
//! the reclamation the retention sweeper drives.
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
    ApplicableQuotas, AppliedMutation, AttributionDigest, CounterSnapshot, DECISION_BLOB_VERSION,
    Decision, DecisionResult, EvaluatedDebit, EvaluatedMutation, EvaluationContext,
    EvaluationFailure, EvaluationQuota, IdempotencyRecord, IdempotencyScope, IdempotencySubjectKey,
    IdempotencyWrite, MutationResult, NO_APPLICABLE_QUOTA, NotificationEvent,
    NotificationEventKind, NotificationScope, OperationType, PartialIdempotencyWrite, PayloadHash,
    PeriodId, PeriodType, PeriodWindow, PolicyScope, PolicyVersion, Quota, QuotaId, QuotaScopeTier,
    QuotaSnapshot, QuotaStatus, QuotaType, Retention, RollbackTarget, StorageError, TenantId,
    ThresholdCrossing, TransitionOutcome, threshold_crossings,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use time::OffsetDateTime;
use toolkit_db::secure::{DBRunner, ScopeError};
use toolkit_db::secure::{TxAccessMode, TxConfig, TxIsolationLevel};
use toolkit_db::{Db, DbError};
use toolkit_security::{AccessScope, SecurityContext};
use uuid::Uuid;

use super::entity::quota_consumption_counter;
use super::locking::{ContentionBudget, lock_scopes, with_budget};
use super::policy_store::{decode_version, scope_key};
use super::quota_mapping::{self, MappingError};
use super::repo::operation_log_repo::Entry;
use super::repo::{
    RowWait, config_repo, consumption_counter_repo as counter_repo, idempotency_repo as idem_repo,
    lease_repo, operation_log_repo, policy_repo, quota_repo,
};
use crate::domain::ports::Actor;
use crate::infra::outbox::{EnqueueError, NotificationEnqueuer};

const LOG_TARGET: &str = "qe.storage";

/// Fallback retention when no configuration row exists: 24 hours (PRD 5.8).
const DEFAULT_RETENTION_SECS: i64 = 86_400;

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
// @cpt-flow:cpt-cf-quota-enforcement-flow-rollback:p1
// @cpt-algo:cpt-cf-quota-enforcement-algo-lazy-expiry:p1
// @cpt-dod:cpt-cf-quota-enforcement-dod-lazy-expiry:p1
// @cpt-dod:cpt-cf-quota-enforcement-dod-counter-period:p1
#[derive(Clone)]
pub struct SqlConsumptionStore {
    pub(super) db: Db,
    pub(super) enqueuer: Arc<dyn NotificationEnqueuer>,
    pub(super) clock: Clock,
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
    /// The row a pre-transaction read decided the lock order from is no longer
    /// the row under the lock. Internal: the primitive restarts.
    #[error("the located record changed before it was locked")]
    Rediscover,
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
        // Both are consumed by a retry loop; seeing either here means the loop
        // gave up.
        TxError::Rediscover => StorageError::Unavailable(format!(
            "{operation}: the located record changed before it could be locked"
        )),
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
    request: Value,
    resource: Value,
    user_projection: Option<gts::GtsTypeId>,
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
            resource: mutation.resource.clone(),
            user_projection: mutation.user_projection.cloned(),
            limits: mutation.limits,
            amount: mutation.amount,
            idempotency: mutation.idempotency.clone(),
            authorized: mutation.authorized,
            evaluate: Arc::clone(&mutation.evaluate),
        }
    }
}

/// One counter movement, as the record stores it. Rollback reverses exactly
/// these, against the period each was attributed to (I5).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct AppliedEntry {
    pub(super) quota_id: Uuid,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) period_id: Option<Uuid>,
    pub(super) amount: u64,
    /// Counter value after the movement, for the caller's mutation result.
    #[serde(default)]
    pub(super) value: u64,
}

/// What a debit committed, before it becomes the caller's result.
struct Applied {
    entries: Vec<AppliedEntry>,
    crossings: Vec<ThresholdCrossing>,
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
        if !counter_repo::mark_settled(tx, scope, row.period_id, now).await? {
            continue;
        }
        let event = quota_event(
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

fn quota_event(
    quota: &Quota,
    kind: NotificationEventKind,
    payload: Value,
    now: OffsetDateTime,
) -> NotificationEvent {
    NotificationEvent {
        event_id: quota_enforcement_sdk::EventId::generate(),
        kind,
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

pub(super) async fn debit_counter(
    tx: &impl DBRunner,
    scope: &AccessScope,
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
        subject_key: scope_of.subject_key.as_bytes(),
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

/// What an existing record says about this attempt.
pub(super) enum Replay {
    /// No record: the operation is new.
    Fresh,
    /// The same payload was recorded; return what it decided.
    Stored(Box<idempotency_row::Model>),
}

use super::entity::idempotency_record as idempotency_row;

/// Look up the record under `write`'s scope, refusing a divergent payload.
pub(super) async fn replay_of(
    tx: &impl DBRunner,
    scope: &AccessScope,
    write: &IdempotencyWrite,
    now: OffsetDateTime,
    lock: Option<RowWait>,
) -> Result<Replay, TxError> {
    let key = scope_key_of(&write.scope);
    let Some(row) = idem_repo::find(tx, scope, &key, now, lock).await? else {
        return Ok(Replay::Fresh);
    };
    if row.payload_hash != write.payload_hash.as_bytes() {
        return Err(StorageError::IdempotencyPayloadMismatch.into());
    }
    Ok(Replay::Stored(Box::new(row)))
}

/// Retention deadline of a record written now for `(tenant, metric)`.
pub(super) async fn expires_at(
    tx: &impl DBRunner,
    tenant: TenantId,
    metric: &str,
    now: OffsetDateTime,
) -> Result<OffsetDateTime, TxError> {
    let seconds =
        config_repo::read_idempotency_retention(tx, &tenant.as_uuid().to_string(), metric)
            .await?
            .unwrap_or(DEFAULT_RETENTION_SECS)
            .max(0);
    Ok(now + time::Duration::seconds(seconds))
}

/// Everything a record write needs beyond its scope.
pub(super) struct RecordWrite<'a> {
    pub(super) write: &'a IdempotencyWrite,
    /// What a replay answers with. A debit, credit or rollback records its
    /// decision; an acquisition records its whole outcome, because a replay
    /// must return the token it issued and not merely the verdict.
    pub(super) blob: RecordBlob<'a>,
    pub(super) entries: Option<&'a [AppliedEntry]>,
    pub(super) authorized: Option<AttributionDigest>,
    pub(super) policy: Option<&'a PolicyVersion>,
    pub(super) expires_at: OffsetDateTime,
    pub(super) now: OffsetDateTime,
}

/// Insert the record, reporting a lost primary-key race as [`TxError::Raced`]
/// so the whole transaction rolls back.
///
/// An expired row under the same key is deleted first: a replay past the
/// retention window is a new operation, so the key must be free for it.
/// What a record stores under its key.
pub(super) enum RecordBlob<'a> {
    /// The transaction's decision, wrapped with its schema version.
    Decision(&'a Decision),
    /// A document the caller already serialized, version and all.
    Verbatim(&'a str),
}

pub(super) async fn write_record(
    tx: &impl DBRunner,
    scope: &AccessScope,
    record: &RecordWrite<'_>,
) -> Result<(), TxError> {
    let key = scope_key_of(&record.write.scope);
    idem_repo::delete_expired_at_key(tx, scope, &key, record.now).await?;
    let entries = record
        .entries
        .filter(|entries| !entries.is_empty())
        .map(serde_json::to_string)
        .transpose()?;
    let new = idem_repo::NewRecord {
        key,
        payload_hash: record.write.payload_hash.as_bytes(),
        decision_blob: match record.blob {
            RecordBlob::Decision(decision) => versioned_blob(decision)?,
            RecordBlob::Verbatim(blob) => blob.to_owned(),
        },
        applied_entries: entries,
        attribution_hash: record.authorized.map(|d| d.as_bytes().to_vec()),
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

fn record_to_model(row: idempotency_row::Model) -> Result<IdempotencyRecord, TxError> {
    let bad = |what: &str| {
        TxError::Storage(StorageError::Internal(format!(
            "idempotency record has an unreadable {what}"
        )))
    };
    let subject_key: [u8; 32] = row
        .subject_key
        .clone()
        .try_into()
        .map_err(|_| bad("subject key"))?;
    let payload_hash: [u8; 32] = row
        .payload_hash
        .clone()
        .try_into()
        .map_err(|_| bad("payload hash"))?;
    let operation_type = [
        OperationType::Debit,
        OperationType::Credit,
        OperationType::Rollback,
        OperationType::Reserve,
        OperationType::Commit,
        OperationType::Release,
        OperationType::BatchDebit,
    ]
    .into_iter()
    .find(|op| op.as_str() == row.operation_type)
    .ok_or_else(|| bad("operation type"))?;
    Ok(IdempotencyRecord {
        scope: IdempotencyScope {
            tenant_id: TenantId::new(row.tenant_id),
            subject_key: IdempotencySubjectKey::from_bytes(subject_key),
            operation_type,
            key: row.idem_key,
        },
        payload_hash: PayloadHash::from_bytes(payload_hash),
        decision_blob: serde_json::from_str(&row.decision_blob)?,
        engine_id: row.engine_id,
        policy_id: row.policy_id.map(quota_enforcement_sdk::PolicyId::new),
        policy_version: row.policy_version.and_then(|v| u32::try_from(v).ok()),
        attribution_hash: row
            .attribution_hash
            .and_then(|bytes| <[u8; 32]>::try_from(bytes).ok())
            .map(AttributionDigest::from_bytes),
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
    pub fn new(db: Db, enqueuer: Arc<dyn NotificationEnqueuer>) -> Self {
        Self {
            db,
            enqueuer,
            clock: Arc::new(OffsetDateTime::now_utc),
        }
    }

    /// The same store on a caller-driven clock. Tests move period boundaries
    /// and retention deadlines this way instead of waiting for them.
    #[must_use]
    pub fn with_clock(db: Db, enqueuer: Arc<dyn NotificationEnqueuer>, clock: Clock) -> Self {
        Self {
            db,
            enqueuer,
            clock,
        }
    }

    fn now(&self) -> OffsetDateTime {
        (self.clock)()
    }

    /// The Quotas of an applicable set, locked ascending by id (ADR-0002).
    pub(super) async fn lock_applicable(
        tx: &impl DBRunner,
        scope: &AccessScope,
        applicable: &ApplicableQuotas,
        wait: RowWait,
    ) -> Result<Vec<Quota>, TxError> {
        let subjects: Vec<(String, String)> = applicable
            .subjects
            .iter()
            .map(|s| (s.projection_type.to_string(), s.subject_id.clone()))
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
                if quota.status == QuotaStatus::Active {
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

    /// Evaluate the selected policy against the locked rows.
    pub(super) async fn evaluate(
        tx: &impl DBRunner,
        scope: &AccessScope,
        quotas: &[Quota],
        mutation: &OwnedMutation,
        now: OffsetDateTime,
    ) -> Result<(PolicyVersion, Decision), TxError> {
        let policy = Self::select_policy(tx, &mutation.applicable.metric).await?;
        let mut snapshots = Vec::with_capacity(quotas.len());
        for quota in quotas {
            snapshots.push(snapshot_of(tx, scope, quota, now).await?);
        }
        let arbitration: Vec<Value> = snapshots
            .iter()
            .map(|s| Value::Object(s.metadata.clone()))
            .collect();
        let evaluation: Vec<EvaluationQuota<'_>> = snapshots
            .iter()
            .zip(&arbitration)
            .map(|(snapshot, arbitration)| EvaluationQuota {
                snapshot,
                tier: match &mutation.user_projection {
                    Some(user) if snapshot.subject.projection_type == *user => QuotaScopeTier::User,
                    _ => QuotaScopeTier::Tenant,
                },
                arbitration,
            })
            .collect();
        // Resolve the budget from the version selected by this transaction.
        let budget = mutation.limits.budget(policy.timeout_ms).map_err(|error| {
            TxError::Storage(StorageError::EvaluationFailed {
                engine_id: policy.engine_id.clone(),
                failure: error.into(),
            })
        })?;
        let context = EvaluationContext {
            policy: &policy,
            metric: &mutation.applicable.metric,
            amount: mutation.amount,
            time: now,
            quotas: &evaluation,
            request: &mutation.request,
            resource: &mutation.resource,
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
    /// each counter in plan order.
    async fn apply_plan(
        tx: &impl DBRunner,
        scope: &AccessScope,
        quotas: &[Quota],
        decision: &Decision,
        now: OffsetDateTime,
        enqueuer: &Arc<dyn NotificationEnqueuer>,
    ) -> Result<Applied, TxError> {
        let mut entries = Vec::new();
        let mut crossings = Vec::new();
        for (quota_id, plan) in &decision.debit_plan {
            let quota = quotas
                .iter()
                .find(|quota| quota.id == *quota_id)
                .ok_or_else(|| {
                    TxError::Storage(StorageError::Internal(format!(
                        "the plan names quota {quota_id}, which is not applicable"
                    )))
                })?;
            if quota.quota_type == QuotaType::Consumption {
                settle_elapsed_rows(tx, scope, quota, now, enqueuer, RowWait::Nowait).await?;
            }
            // @cpt-begin:cpt-cf-quota-enforcement-algo-evaluation-pipeline:p1:inst-pipe-apply
            let (entry, crossing) = debit_counter(
                tx,
                scope,
                quota,
                plan.amount,
                now,
                enqueuer,
                RowWait::Nowait,
            )
            .await?;
            // @cpt-end:cpt-cf-quota-enforcement-algo-evaluation-pipeline:p1:inst-pipe-apply
            entries.push(entry);
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
    let (consumed, period) = if quota.quota_type == QuotaType::Consumption {
        let latest = counter_repo::find_latest(tx, scope, quota.id.as_uuid()).await?;
        let window = window_of(quota, now);
        let current = latest.filter(|row| row.period_start <= now && now < row.period_end);
        let stored = current
            .as_ref()
            .map_or(0, |row| u64::try_from(row.consumed).unwrap_or(0));
        // @cpt-begin:cpt-cf-quota-enforcement-algo-lazy-expiry:p1:inst-lzy-capacity
        let owed = unreturned_expired(
            tx,
            scope,
            quota.id.as_uuid(),
            current.as_ref().map(|row| row.period_id),
            now,
        )
        .await?;
        (stored.saturating_sub(owed), Some(window))
        // @cpt-end:cpt-cf-quota-enforcement-algo-lazy-expiry:p1:inst-lzy-capacity
    } else {
        let row = counter_repo::find_allocation(tx, scope, quota.id.as_uuid()).await?;
        let stored = row.map_or(0, |row| u64::try_from(row.in_flight).unwrap_or(0));
        let owed = unreturned_expired(tx, scope, quota.id.as_uuid(), None, now).await?;
        (stored.saturating_sub(owed), None)
    };
    Ok(QuotaSnapshot {
        quota_id: quota.id,
        subject: quota.subject.clone(),
        metric: quota.metric.clone(),
        quota_type: quota.quota_type,
        enforcement_mode: quota.enforcement_mode,
        cap: quota.cap,
        consumed,
        remaining: quota.cap.map(|cap| cap.saturating_sub(consumed)),
        period,
        metadata: quota.metadata.clone(),
        validity_window: quota.validity_window,
        currently_within_window: quota.validity_window.is_none_or(|w| w.contains(now)),
    })
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

impl SqlConsumptionStore {
    /// One attempt of `apply_debit_plan`: lock, replay or evaluate, apply,
    /// record, all in the caller's transaction.
    async fn debit_in_tx(
        tx: &impl DBRunner,
        scope: &AccessScope,
        mutation: &OwnedMutation,
        events: &[NotificationEvent],
        actor: &Actor,
        clock: &Clock,
        enqueuer: &Arc<dyn NotificationEnqueuer>,
    ) -> Result<TransitionOutcome<EvaluatedDebit>, TxError> {
        let quotas =
            Self::lock_applicable(tx, scope, &mutation.applicable, RowWait::Nowait).await?;
        // Rank 2: the scope's stripe keeps out a writer of the
        // same key over other Quotas (I8).
        lock_scopes(tx, &[&mutation.idempotency.scope]).await?;
        // Sample time after locking so boundary waits charge the
        // period in which the transaction commits.
        let now = clock();
        if let Replay::Stored(row) =
            replay_of(tx, scope, &mutation.idempotency, now, Some(RowWait::Nowait)).await?
        {
            // Replays return the recorded decision without side effects.
            return Ok(TransitionOutcome::NoOp(EvaluatedDebit {
                decision: decision_of(&row.decision_blob)?,
                mutation: MutationResult::default(),
                retention: Retention::Recorded {
                    expires_at: row.expires_at,
                },
            }));
        }
        let (policy, decision) = Self::evaluate(tx, scope, &quotas, mutation, now).await?;
        if decision.denied_reason() == Some(NO_APPLICABLE_QUOTA) {
            // The one denial that records nothing at all:
            // provisioning a Quota must change the answer.
            return Ok(TransitionOutcome::Applied(EvaluatedDebit {
                decision,
                mutation: MutationResult::default(),
                retention: Retention::Unrecorded,
            }));
        }
        let applied = if matches!(decision.result, DecisionResult::Allowed) {
            Self::apply_plan(tx, scope, &quotas, &decision, now, enqueuer).await?
        } else {
            Applied {
                entries: Vec::new(),
                crossings: Vec::new(),
            }
        };
        let expires_at = expires_at(
            tx,
            mutation.applicable.tenant_id,
            mutation.applicable.metric.as_str(),
            now,
        )
        .await?;
        write_record(
            tx,
            scope,
            &RecordWrite {
                write: &mutation.idempotency,
                blob: RecordBlob::Decision(&decision),
                entries: Some(&applied.entries),
                authorized: Some(mutation.authorized),
                policy: Some(&policy),
                expires_at,
                now,
            },
        )
        .await?;
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

/// What an unlocked read of a rollback's original record found: the Quotas to
/// lock first, and the identity the locked row must still have.
#[derive(Debug, Clone, Default)]
struct Discovered {
    quotas: Vec<Uuid>,
    identity: Option<(OffsetDateTime, Vec<u8>)>,
}

/// Who a rollback transaction acts as, and what it reads the time from and
/// enqueues into.
struct RollbackEnv<'a> {
    actor: &'a Actor,
    clock: &'a Clock,
    enqueuer: &'a Arc<dyn NotificationEnqueuer>,
}

impl SqlConsumptionStore {
    /// One attempt of `apply_rollback`, in the caller's transaction.
    async fn rollback_in_tx(
        tx: &impl DBRunner,
        scope: &AccessScope,
        target: &RollbackTarget,
        idempotency: &IdempotencyWrite,
        events: &[NotificationEvent],
        discovered: &Discovered,
        env: &RollbackEnv<'_>,
    ) -> Result<TransitionOutcome<AppliedMutation>, TxError> {
        // Rank 1: the Quotas the original moved, ascending, so
        // a concurrent debit replaying the same key — which
        // holds its Quotas and then wants the record — cannot
        // deadlock against this one.
        let mut ordered_quotas = discovered.quotas.clone();
        ordered_quotas.sort_unstable();
        ordered_quotas.dedup();
        for quota_id in &ordered_quotas {
            // Deactivated rows included: a rollback reverses a
            // Quota that has since been deactivated.
            quota_repo::find_by_id(tx, scope, *quota_id, Some(RowWait::Nowait)).await?;
        }
        // Rank 2: the stripes of this rollback's own scope and
        // of the original's, whose record it marks (I8).
        lock_scopes(tx, &[&idempotency.scope, &target.original]).await?;
        let now = (env.clock)();
        // Check the rollback key first so replay outlives the
        // original record's retention window.
        // @cpt-begin:cpt-cf-quota-enforcement-flow-rollback:p1:inst-rlb-idem
        if let Replay::Stored(stored) =
            replay_of(tx, scope, idempotency, now, Some(RowWait::Nowait)).await?
        {
            return Ok(TransitionOutcome::NoOp(AppliedMutation {
                decision: decision_of(&stored.decision_blob)?,
                mutation: MutationResult::default(),
                expires_at: stored.expires_at,
            }));
        }
        // @cpt-end:cpt-cf-quota-enforcement-flow-rollback:p1:inst-rlb-idem
        let (original, entries) = Self::locked_original(tx, scope, target, discovered, now).await?;
        let original_key = scope_key_of(&target.original);
        let already_reversed = original.reversed_by_key.is_some();
        // @cpt-begin:cpt-cf-quota-enforcement-flow-rollback:p1:inst-rlb-apply
        let (reversed, quotas) = if already_reversed {
            (Vec::new(), Vec::new())
        } else {
            Self::reverse_entries(tx, scope, target, &entries, now).await?
        };
        let plan = if already_reversed {
            std::collections::BTreeMap::new()
        } else {
            reversed
                .iter()
                .map(|entry| {
                    (
                        QuotaId::new(entry.quota_id),
                        quota_enforcement_sdk::QuotaDebitPlan {
                            amount: entry.amount,
                        },
                    )
                })
                .collect()
        };
        let decision = Decision::allowed_with_plan(plan);
        let metric = quotas
            .first()
            .map_or_else(String::new, |quota| quota.metric.as_str().to_owned());
        let expires_at = expires_at(tx, idempotency.scope.tenant_id, &metric, now).await?;
        write_record(
            tx,
            scope,
            &RecordWrite {
                write: idempotency,
                blob: RecordBlob::Decision(&decision),
                entries: None,
                authorized: None,
                policy: None,
                expires_at,
                now,
            },
        )
        .await?;
        if !already_reversed {
            // Reversal happens once however many rollback keys
            // target the original.
            idem_repo::mark_reversed(tx, scope, &original_key, &idempotency.scope.key).await?;
            for (entry, quota) in reversed.iter().zip(&quotas) {
                operation_log_repo::append(
                    tx,
                    scope,
                    Entry {
                        tenant_id: quota.tenant_id.as_uuid(),
                        quota_id: entry.quota_id,
                        operation: operation_log_repo::OP_ROLLBACK,
                        actor: env.actor,
                        record_version: 1,
                        detail: String::new(),
                        occurred_at: now,
                    },
                )
                .await?;
                // Emitted even inside the settlement window:
                // ADR-0004 silences adjusted and threshold events
                // there, not this one.
                let event = quota_event(
                    quota,
                    NotificationEventKind::QuotaRollbackApplied,
                    json!({
                        "original_idempotency_key": target.original.key,
                        "rolled_back_amount": entry.amount,
                        "quota_id": QuotaId::new(entry.quota_id),
                        "principal": env.actor.subject_id,
                    }),
                    now,
                );
                env.enqueuer
                    .enqueue_all(tx, std::slice::from_ref(&event))
                    .await?;
            }
            env.enqueuer.enqueue_all(tx, events).await?;
            // @cpt-end:cpt-cf-quota-enforcement-flow-rollback:p1:inst-rlb-apply
        }
        Ok(TransitionOutcome::Applied(AppliedMutation {
            decision,
            mutation: MutationResult {
                counters: counters_of(&reversed),
                threshold_crossings: Vec::new(),
                event_ids: if already_reversed {
                    Vec::new()
                } else {
                    events.iter().map(|e| e.event_id).collect()
                },
            },
            expires_at,
        }))
    }

    /// The rollback's original record, locked and re-verified: still the row
    /// discovery read, authorized under the request's attribution, and a real
    /// movement (or a lease commit of zero). Returns it with its entries.
    async fn locked_original(
        tx: &impl DBRunner,
        scope: &AccessScope,
        target: &RollbackTarget,
        discovered: &Discovered,
        now: OffsetDateTime,
    ) -> Result<(idempotency_row::Model, Vec<AppliedEntry>), TxError> {
        // @cpt-begin:cpt-cf-quota-enforcement-flow-rollback:p1:inst-rlb-unknown-if
        // @cpt-begin:cpt-cf-quota-enforcement-flow-rollback:p1:inst-rlb-unknown
        let unknown = || StorageError::OperationNotFound {
            key: target.original.key.clone(),
        };
        // @cpt-begin:cpt-cf-quota-enforcement-flow-rollback:p1:inst-rlb-lookup
        let original_key = scope_key_of(&target.original);
        let original = idem_repo::find(tx, scope, &original_key, now, Some(RowWait::Nowait))
            .await?
            .ok_or_else(unknown)?;
        // The unlocked read decided which Quotas to lock. If
        // the row is not the one that was read — the key can
        // expire and be reused by another debit over other
        // Quotas — nothing here holds the right locks, so the
        // transaction gives up and the primitive rediscovers
        // rather than reaching for a rank-1 lock mid-way.
        if discovered
            .identity
            .as_ref()
            .is_none_or(|(created_at, payload_hash)| {
                *created_at != original.created_at || *payload_hash != original.payload_hash
            })
        {
            return Err(TxError::Rediscover);
        }
        // @cpt-end:cpt-cf-quota-enforcement-flow-rollback:p1:inst-rlb-lookup
        // Bind authorization to the original metric and resource,
        // not only its subjects.
        let authorized = original
            .attribution_hash
            .clone()
            .and_then(|bytes| <[u8; 32]>::try_from(bytes).ok())
            .map(AttributionDigest::from_bytes);
        if authorized != Some(target.authorized) {
            return Err(unknown().into());
        }
        let entries: Vec<AppliedEntry> = match &original.applied_entries {
            Some(json) => serde_json::from_str(json)?,
            None => Vec::new(),
        };
        // A debit that moved nothing is not a committed debit:
        // a denial records no movement either and must stay
        // irreversible. A lease commit of zero is a real
        // operation that kept nothing, so it reverses as a
        // successful no-op.
        if entries.is_empty() && target.original.operation_type != OperationType::Commit {
            return Err(unknown().into());
        }
        // @cpt-end:cpt-cf-quota-enforcement-flow-rollback:p1:inst-rlb-unknown
        // @cpt-end:cpt-cf-quota-enforcement-flow-rollback:p1:inst-rlb-unknown-if
        Ok((original, entries))
    }

    /// Lock every Quota and counter row the original moved, refuse a period
    /// that has settled, and credit each entry back. Returns the reversed
    /// entries with their new values, and the Quotas in the same order.
    async fn reverse_entries(
        tx: &impl DBRunner,
        scope: &AccessScope,
        target: &RollbackTarget,
        entries: &[AppliedEntry],
        now: OffsetDateTime,
    ) -> Result<(Vec<AppliedEntry>, Vec<Quota>), TxError> {
        let unknown = || StorageError::OperationNotFound {
            key: target.original.key.clone(),
        };
        let mut reversed = Vec::new();
        let mut quotas = Vec::new();
        let mut ordered = entries.to_vec();
        ordered.sort_by_key(|entry| entry.quota_id);
        for entry in &ordered {
            let row = quota_repo::find_by_id(tx, scope, entry.quota_id, Some(RowWait::Nowait))
                .await?
                .ok_or_else(unknown)?;
            // A Quota deactivated since is still reversed:
            // deactivation forbids new consumption, not the
            // undo of a committed one.
            let quota = quota_mapping::row_to_quota(row)?;
            if let Some(period_id) = entry.period_id {
                let period = counter_repo::find_by_period_id_for_update(
                    tx,
                    scope,
                    period_id,
                    RowWait::Nowait,
                )
                .await?
                .ok_or_else(unknown)?;
                // Closure is settlement-keyed here: the closing
                // period stays reversible until its rollover
                // event has been emitted.
                // @cpt-begin:cpt-cf-quota-enforcement-flow-rollback:p1:inst-rlb-settled-if
                // @cpt-begin:cpt-cf-quota-enforcement-flow-rollback:p1:inst-rlb-settled
                if period.is_settled {
                    return Err(StorageError::PeriodClosed.into());
                }
                // @cpt-end:cpt-cf-quota-enforcement-flow-rollback:p1:inst-rlb-settled
                // @cpt-end:cpt-cf-quota-enforcement-flow-rollback:p1:inst-rlb-settled-if
            }
            quotas.push(quota);
        }
        for entry in &ordered {
            let value = credit_counter(
                tx,
                scope,
                entry.quota_id,
                entry.period_id,
                entry.amount,
                now,
                RowWait::Nowait,
            )
            .await?;
            reversed.push(AppliedEntry {
                value,
                ..entry.clone()
            });
        }
        Ok((reversed, quotas))
    }
}

#[async_trait::async_trait]
impl crate::domain::ports::ConsumptionStore for SqlConsumptionStore {
    async fn apply_debit_plan(
        &self,
        ctx: &SecurityContext,
        scope: &AccessScope,
        mutation: &EvaluatedMutation<'_>,
        events: &[NotificationEvent],
    ) -> Result<TransitionOutcome<EvaluatedDebit>, StorageError> {
        const OPERATION: &str = "apply debit plan";
        let actor = actor_of(ctx);
        let budget = self
            .contention_budget(Some(mutation.applicable.metric.as_str()))
            .await?;
        // Each retry is a complete transaction.
        for attempt in 0..RACE_ATTEMPTS {
            // A refused lock rolls this transaction back and runs it
            // again under the budget; a race goes to the arbiter below.
            let result = with_budget(budget, || {
                let enqueuer = Arc::clone(&self.enqueuer);
                let clock = Arc::clone(&self.clock);
                let actor = actor.clone();
                let scope = scope.clone();
                let events = events.to_vec();
                let mutation = OwnedMutation::of(mutation);
                self.db.transaction_ref_mapped(move |tx| {
                    Box::pin(async move {
                        Self::debit_in_tx(tx, &scope, &mutation, &events, &actor, &clock, &enqueuer)
                            .await
                    })
                })
            })
            .await;
            match result {
                Ok(outcome) => return Ok(outcome),
                Err(TxError::Raced(lost)) => {
                    // The whole transaction rolled back. Resolve the winner's
                    // record; if it has since gone, run the operation again.
                    if let Some(record) = self.winner_of(&lost).await? {
                        return Ok(TransitionOutcome::NoOp(EvaluatedDebit {
                            decision: decision_of(&record.decision_blob)
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

    async fn apply_credit(
        &self,
        ctx: &SecurityContext,
        scope: &AccessScope,
        quota_id: QuotaId,
        amount: u64,
        idempotency: &PartialIdempotencyWrite,
        events: &[NotificationEvent],
    ) -> Result<TransitionOutcome<AppliedMutation>, StorageError> {
        const OPERATION: &str = "apply credit";
        let actor = actor_of(ctx);
        let metric = self
            .metric_of(scope, quota_id.as_uuid())
            .await
            .map_err(|error| lift(OPERATION, error))?;
        let budget = self.contention_budget(metric.as_deref()).await?;
        // The Quota row lock normally serializes credits; the arbiter is a
        // defensive fallback.
        for attempt in 0..RACE_ATTEMPTS {
            // A refused lock rolls this transaction back and runs it
            // again under the budget; a race goes to the arbiter below.
            let result = with_budget(budget, || {
                let actor = actor.clone();
                let enqueuer = Arc::clone(&self.enqueuer);
                let clock = Arc::clone(&self.clock);
                let scope = scope.clone();
                let events = events.to_vec();
                let idempotency = idempotency.clone();
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
                        let write = idempotency.clone().complete(
                            IdempotencySubjectKey::of(std::slice::from_ref(&quota.subject)),
                            OperationType::Credit,
                        );
                        // Rank 2: the scope's stripe (I8).
                        lock_scopes(tx, &[&write.scope]).await?;
                        // @cpt-begin:cpt-cf-quota-enforcement-flow-credit:p1:inst-cre-idem
                        if let Replay::Stored(stored) =
                            replay_of(tx, scope, &write, now, Some(RowWait::Nowait)).await?
                        {
                            return Ok(TransitionOutcome::NoOp(AppliedMutation {
                                decision: decision_of(&stored.decision_blob)?,
                                mutation: MutationResult::default(),
                                expires_at: stored.expires_at,
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
                            settle_elapsed_rows(tx, scope, &quota, now, &enqueuer, RowWait::Nowait)
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
                        };
                        let decision = Decision::allowed_with_plan(
                            [(quota_id, quota_enforcement_sdk::QuotaDebitPlan { amount })]
                                .into_iter()
                                .collect(),
                        );
                        let expires_at =
                            expires_at(tx, quota.tenant_id, quota.metric.as_str(), now).await?;
                        // A credit evaluates no policy, so its record carries
                        // neither engine attribution nor a reversible movement.
                        write_record(
                            tx,
                            scope,
                            &RecordWrite {
                                write: &write,
                                blob: RecordBlob::Decision(&decision),
                                entries: None,
                                authorized: None,
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
                            &quota,
                            NotificationEventKind::QuotaCounterAdjusted,
                            // The consumer identity the operation was authorized
                            // under travels with the event, not only to the log:
                            // a sink has to answer who adjusted the counter.
                            json!({
                                "credited_amount": amount,
                                "quota_id": quota_id,
                                "principal": actor.subject_id,
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
                            expires_at,
                        }))
                    })
                })
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

    async fn apply_rollback(
        &self,
        ctx: &SecurityContext,
        scope: &AccessScope,
        target: &RollbackTarget,
        idempotency: &IdempotencyWrite,
        events: &[NotificationEvent],
    ) -> Result<TransitionOutcome<AppliedMutation>, StorageError> {
        const OPERATION: &str = "apply rollback";
        let actor = actor_of(ctx);
        // One budget for the whole call, rediscoveries included, named by the
        // metric of the first Quota the original moved.
        let budget = self
            .rollback_budget(scope, target)
            .await
            .map_err(|error| lift(OPERATION, error))?;
        // The original row lock normally serializes rollbacks; the arbiter also
        // protects a reused key that reaches different Quotas.
        for attempt in 0..RACE_ATTEMPTS {
            // Rank-free: which Quotas the original moved, so they can be locked
            // first. A record is immutable once written, so this is either the
            // row the transaction will lock or one whose key expired and was
            // reused — which the identity check below catches.
            let original = {
                let conn = self
                    .db
                    .conn()
                    .map_err(|error| lift(OPERATION, TxError::Db(error)))?;
                let key = scope_key_of(&target.original);
                idem_repo::find(&conn, scope, &key, (self.clock)(), None)
                    .await
                    .map_err(|error| lift(OPERATION, error.into()))?
            };
            let discovered = match &original {
                Some(row) => Discovered {
                    quotas: serde_json::from_str::<Vec<AppliedEntry>>(
                        row.applied_entries.as_deref().unwrap_or("[]"),
                    )
                    .map_err(|error| lift(OPERATION, error.into()))?
                    .into_iter()
                    .map(|entry| entry.quota_id)
                    .collect(),
                    identity: Some((row.created_at, row.payload_hash.clone())),
                },
                None => Discovered::default(),
            };
            // A refused lock rolls this transaction back and runs it
            // again under the budget; a race goes to the arbiter below.
            let result = with_budget(budget, || {
                let actor = actor.clone();
                let enqueuer = Arc::clone(&self.enqueuer);
                let clock = Arc::clone(&self.clock);
                let scope = scope.clone();
                let events = events.to_vec();
                let idempotency = idempotency.clone();
                let target = target.clone();
                let discovered = discovered.clone();
                self.db.transaction_ref_mapped(move |tx| {
                    Box::pin(async move {
                        let env = RollbackEnv {
                            actor: &actor,
                            clock: &clock,
                            enqueuer: &enqueuer,
                        };
                        Self::rollback_in_tx(
                            tx,
                            &scope,
                            &target,
                            &idempotency,
                            &events,
                            &discovered,
                            &env,
                        )
                        .await
                    })
                })
            })
            .await;
            match result {
                Ok(outcome) => return Ok(outcome),
                Err(TxError::Rediscover) => {
                    // The record under the lock was not the one discovery read,
                    // so this attempt held the wrong Quota locks. The whole
                    // transaction rolled back, leaving nothing half written;
                    // the next attempt rediscovers and locks afresh.
                    if attempt + 1 == RACE_ATTEMPTS {
                        return Err(lift(OPERATION, TxError::Rediscover));
                    }
                }
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

    // The counter and the expired holds that correct it are two statements, so
    // they are read under `RepeatableRead`: at `ReadCommitted` a sweeper
    // committing between them would pair a pre-return counter with a
    // post-return correction and over-report usage, denying work that fits.
    async fn read_quota_snapshot(
        &self,
        _ctx: &SecurityContext,
        scope: &AccessScope,
        applicable: &ApplicableQuotas,
    ) -> Result<Vec<QuotaSnapshot>, StorageError> {
        const OPERATION: &str = "read quota snapshot";
        let now = self.now();
        let subjects: Vec<(String, String)> = applicable
            .subjects
            .iter()
            .map(|s| (s.projection_type.to_string(), s.subject_id.clone()))
            .collect();
        let conn = self
            .db
            .conn()
            .map_err(|error| unavailable(OPERATION, "connection", &error))?;
        let ids = quota_repo::find_applicable_ids(
            &conn,
            scope,
            applicable.tenant_id.as_uuid(),
            applicable.metric.as_str(),
            &subjects,
        )
        .await
        .map_err(|error| lift(OPERATION, error.into()))?;
        let mut snapshots = Vec::with_capacity(ids.len());
        for id in ids {
            let Some(row) = quota_repo::find_by_id(&conn, scope, id, None)
                .await
                .map_err(|error| lift(OPERATION, error.into()))?
            else {
                continue;
            };
            let quota =
                quota_mapping::row_to_quota(row).map_err(|error| lift(OPERATION, error.into()))?;
            // I3 permits materializing only the row being read, without
            // settlement or outbox events.
            if quota.quota_type == QuotaType::Consumption {
                let quota = quota.clone();
                let scope = scope.clone();
                let clock = Arc::clone(&self.clock);
                self.db
                    .transaction_ref_mapped(move |tx| {
                        Box::pin(async move {
                            // Read under the lock this transaction takes, so a
                            // read that waited out a boundary materializes the
                            // period it is actually in.
                            // The read path is outside I8: it queues.
                            ensure_current_period(tx, &scope, &quota, clock(), RowWait::Wait)
                                .await?;
                            Ok::<_, TxError>(())
                        })
                    })
                    .await
                    .map_err(|error| lift(OPERATION, error))?;
            }
            // The counter and the expired holds that correct it are two
            // statements, so they run under one repeatable-read snapshot: at
            // read-committed a sweeper committing between them would pair a
            // pre-return counter with a post-return correction and over-report
            // usage, denying work that actually fits. Read-only, so this
            // remains the I3 read path.
            let quota_for_read = quota.clone();
            let scope_for_read = scope.clone();
            let snapshot = self
                .db
                .transaction_ref_mapped_with_config(
                    TxConfig {
                        isolation: Some(TxIsolationLevel::RepeatableRead),
                        access_mode: Some(TxAccessMode::ReadOnly),
                    },
                    move |tx| {
                        Box::pin(async move {
                            snapshot_of(tx, &scope_for_read, &quota_for_read, now).await
                        })
                    },
                )
                .await
                .map_err(|error| lift(OPERATION, error))?;
            snapshots.push(snapshot);
        }
        Ok(snapshots)
    }

    async fn lookup_idempotency(
        &self,
        scope_of: &IdempotencyScope,
    ) -> Result<Option<IdempotencyRecord>, StorageError> {
        const OPERATION: &str = "lookup idempotency";
        let now = self.now();
        let key = scope_key_of(scope_of);
        // A lookup is the caller's own key; the tenant scope it carries is the
        // one the PDP authorized for this operation.
        let scope = AccessScope::allow_all();
        let conn = self
            .db
            .conn()
            .map_err(|error| unavailable(OPERATION, "connection", &error))?;
        let row = idem_repo::find(&conn, &scope, &key, now, None)
            .await
            .map_err(|error| lift(OPERATION, error.into()))?;
        row.map(record_to_model)
            .transpose()
            .map_err(|error| lift(OPERATION, error))
    }

    async fn reclaim_expired_idempotency(
        &self,
        batch_size: u32,
        before: OffsetDateTime,
    ) -> Result<u64, StorageError> {
        const OPERATION: &str = "reclaim idempotency";
        let doomed = {
            let conn = self
                .db
                .conn()
                .map_err(|error| unavailable(OPERATION, "connection", &error))?;
            idem_repo::select_expired(&conn, &AccessScope::allow_all(), batch_size, before)
                .await
                .map_err(|error| lift(OPERATION, error.into()))?
        };
        // One short transaction per key, under the writers' protocol: the
        // scope's stripe first — skipped, not waited on, while a writer holds
        // it — then a delete that re-checks the expiry, so a record a writer
        // put in place of the expired one since the selection survives.
        let mut deleted = 0;
        for (tenant_id, subject_key, operation_type, idem_key) in doomed {
            deleted += self
                .db
                .transaction_ref_mapped(move |tx| {
                    Box::pin(async move {
                        let scope = AccessScope::allow_all();
                        let key = idem_repo::ScopeKey {
                            tenant_id,
                            subject_key: &subject_key,
                            operation_type: &operation_type,
                            idem_key: &idem_key,
                        };
                        if !idem_repo::try_lock_stripe(tx, idem_repo::stripe_of(&key)).await? {
                            return Ok::<_, TxError>(0);
                        }
                        Ok(idem_repo::delete_if_expired(tx, &scope, &key, before).await?)
                    })
                })
                .await
                .map_err(|error| lift(OPERATION, error))?;
        }
        Ok(deleted)
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
}

impl SqlConsumptionStore {
    /// A rollback's contention budget, named by the metric of the first Quota
    /// the original moved, read without a lock; the platform default when the
    /// original is absent or moved nothing.
    async fn rollback_budget(
        &self,
        scope: &AccessScope,
        target: &RollbackTarget,
    ) -> Result<ContentionBudget, TxError> {
        let original = {
            let conn = self.db.conn().map_err(TxError::Db)?;
            idem_repo::find(
                &conn,
                scope,
                &scope_key_of(&target.original),
                self.now(),
                None,
            )
            .await?
        };
        let first = match original.and_then(|row| row.applied_entries) {
            Some(entries) => serde_json::from_str::<Vec<AppliedEntry>>(&entries)?
                .first()
                .map(|entry| entry.quota_id),
            None => None,
        };
        let metric = match first {
            Some(quota_id) => self.metric_of(scope, quota_id).await?,
            None => None,
        };
        Ok(self.contention_budget(metric.as_deref()).await?)
    }

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
    ) -> Result<Option<idempotency_row::Model>, StorageError> {
        const OPERATION: &str = "resolve idempotency race";
        let now = self.now();
        let key = scope_key_of(&lost.scope);
        let scope = AccessScope::allow_all();
        let conn = self
            .db
            .conn()
            .map_err(|error| unavailable(OPERATION, "connection", &error))?;
        let Some(row) = idem_repo::find(&conn, &scope, &key, now, None)
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
        let decision = decision_of(&row.decision_blob).map_err(|error| lift(operation, error))?;
        Ok(Some(TransitionOutcome::NoOp(AppliedMutation {
            decision,
            mutation: MutationResult::default(),
            expires_at: row.expires_at,
        })))
    }
}

pub(super) fn actor_of(ctx: &SecurityContext) -> Actor {
    Actor {
        subject_id: ctx.subject_id(),
        subject_type: None,
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "consumption_store_tests.rs"]
mod tests;
