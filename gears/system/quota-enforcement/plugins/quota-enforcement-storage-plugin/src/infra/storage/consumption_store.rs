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
use toolkit_db::{Db, DbError};
use toolkit_security::{AccessScope, SecurityContext};
use uuid::Uuid;

use super::entity::quota_consumption_counter;
use super::policy_store::{decode_version, scope_key};
use super::quota_mapping::{self, MappingError};
use super::repo::operation_log_repo::Entry;
use super::repo::{
    config_repo, consumption_counter_repo as counter_repo, idempotency_repo as idem_repo,
    operation_log_repo, policy_repo, quota_repo,
};
use crate::domain::ports::Actor;
use crate::infra::outbox::{EnqueueError, NotificationEnqueuer};

const LOG_TARGET: &str = "qe.storage";

/// Fallback retention when no configuration row exists: 24 hours (PRD 5.8).
const DEFAULT_RETENTION_SECS: i64 = 86_400;

/// How many times a transaction that lost the record's primary key is retried.
/// Two: one to observe the winner, and the original attempt.
const RACE_ATTEMPTS: u32 = 2;

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
// @cpt-dod:cpt-cf-quota-enforcement-dod-counter-period:p1
#[derive(Clone)]
pub struct SqlConsumptionStore {
    db: Db,
    enqueuer: Arc<dyn NotificationEnqueuer>,
    clock: Clock,
}

#[derive(Debug, thiserror::Error)]
enum TxError {
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

fn unavailable(
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

fn lift(operation: &'static str, error: TxError) -> StorageError {
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
        // The retry loop consumes this; seeing it here means the loop gave up.
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
struct OwnedMutation {
    applicable: ApplicableQuotas,
    request: Value,
    resource: Value,
    user_projection: Option<gts::GtsTypeId>,
    limits: quota_enforcement_sdk::engine::EvaluationLimits,
    amount: u64,
    idempotency: IdempotencyWrite,
    authorized: AttributionDigest,
    evaluate: Arc<quota_enforcement_sdk::engine::TransactionEvaluator>,
}

impl OwnedMutation {
    fn of(mutation: &EvaluatedMutation<'_>) -> Self {
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
struct AppliedEntry {
    quota_id: Uuid,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    period_id: Option<Uuid>,
    amount: u64,
    /// Counter value after the movement, for the caller's mutation result.
    #[serde(default)]
    value: u64,
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
fn window_of(quota: &Quota, now: OffsetDateTime) -> PeriodWindow {
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
async fn ensure_current_period(
    tx: &impl DBRunner,
    scope: &AccessScope,
    quota: &Quota,
    now: OffsetDateTime,
) -> Result<quota_consumption_counter::Model, TxError> {
    let latest = counter_repo::find_latest_for_update(tx, scope, quota.id.as_uuid()).await?;
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
            counter_repo::find_latest_for_update(tx, scope, quota.id.as_uuid())
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
async fn settle_elapsed_rows(
    tx: &impl DBRunner,
    scope: &AccessScope,
    quota: &Quota,
    now: OffsetDateTime,
    enqueuer: &Arc<dyn NotificationEnqueuer>,
) -> Result<(), TxError> {
    let closing =
        counter_repo::find_elapsed_unsettled_for_update(tx, scope, quota.id.as_uuid(), now).await?;
    // @cpt-begin:cpt-cf-quota-enforcement-algo-period-rollover:p1:inst-per-settle
    // @cpt-begin:cpt-cf-quota-enforcement-algo-period-rollover:p1:inst-per-window
    // @cpt-begin:cpt-cf-quota-enforcement-algo-period-rollover:p1:inst-per-forfeit
    // @cpt-begin:cpt-cf-quota-enforcement-algo-period-rollover:p1:inst-per-return
    // @cpt-begin:cpt-cf-quota-enforcement-state-consumption-period:p1:inst-perst-settle
    for row in closing {
        if !counter_repo::mark_settled(tx, scope, row.period_id, now).await? {
            continue;
        }
        let consumed = u64::try_from(row.consumed).unwrap_or(0);
        let event = quota_event(
            quota,
            NotificationEventKind::PeriodRollover,
            json!({
                "closing_period_id": PeriodId::new(row.period_id),
                "closing_consumed": consumed,
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
async fn debit_counter(
    tx: &impl DBRunner,
    scope: &AccessScope,
    quota: &Quota,
    amount: u64,
    now: OffsetDateTime,
    enqueuer: &Arc<dyn NotificationEnqueuer>,
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
        let row = ensure_current_period(tx, scope, quota, now).await?;
        let pre = u64::try_from(row.consumed).unwrap_or(0);
        (
            Some(row.period_id),
            pre,
            row.record_version,
            now >= row.period_end,
        )
    } else {
        let row = counter_repo::find_allocation_for_update(tx, scope, quota.id.as_uuid())
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
    let post = pre.checked_add(amount).ok_or_else(overflow)?;
    let marker = current_marker(tx, scope, quota, period_id).await?;
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
) -> Result<Option<u8>, TxError> {
    let raw = match period_id {
        Some(period_id) => counter_repo::find_by_period_id_for_update(tx, scope, period_id)
            .await?
            .and_then(|row| row.highest_crossed_threshold_pct),
        None => counter_repo::find_allocation_for_update(tx, scope, quota.id.as_uuid())
            .await?
            .and_then(|row| row.highest_crossed_threshold_pct),
    };
    Ok(raw.and_then(|value| u8::try_from(value).ok()))
}

/// Lower one counter, flooring at zero. A downward move emits no threshold: the
/// marker only advances, so a threshold crossed once stays crossed until the
/// period rolls over.
async fn credit_counter(
    tx: &impl DBRunner,
    scope: &AccessScope,
    quota_id: Uuid,
    period_id: Option<Uuid>,
    amount: u64,
    now: OffsetDateTime,
) -> Result<u64, TxError> {
    let (value, version, marker) = if let Some(period_id) = period_id {
        let row = counter_repo::find_by_period_id_for_update(tx, scope, period_id)
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
        let row = counter_repo::find_allocation_for_update(tx, scope, quota_id)
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
    let pre = u64::try_from(value).unwrap_or(0);
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

fn scope_key_of(scope_of: &IdempotencyScope) -> idem_repo::ScopeKey<'_> {
    idem_repo::ScopeKey {
        tenant_id: scope_of.tenant_id.as_uuid(),
        subject_key: scope_of.subject_key.as_bytes(),
        operation_type: scope_of.operation_type.as_str(),
        idem_key: &scope_of.key,
    }
}

/// The decision blob a record stores: the decision plus its schema version,
/// which the decision's own deserializer ignores when reading it back.
fn versioned_blob(decision: &Decision) -> Result<String, TxError> {
    let mut blob = serde_json::to_value(decision)?;
    if let Value::Object(map) = &mut blob {
        map.insert("__version".to_owned(), Value::from(DECISION_BLOB_VERSION));
    }
    Ok(serde_json::to_string(&blob)?)
}

fn decision_of(blob: &str) -> Result<Decision, TxError> {
    Ok(serde_json::from_str(blob)?)
}

/// What an existing record says about this attempt.
enum Replay {
    /// No record: the operation is new.
    Fresh,
    /// The same payload was recorded; return what it decided.
    Stored(Box<idempotency_row::Model>),
}

use super::entity::idempotency_record as idempotency_row;

/// Look up the record under `write`'s scope, refusing a divergent payload.
async fn replay_of(
    tx: &impl DBRunner,
    scope: &AccessScope,
    write: &IdempotencyWrite,
    now: OffsetDateTime,
    lock: bool,
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
async fn expires_at(
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
struct RecordWrite<'a> {
    write: &'a IdempotencyWrite,
    decision: &'a Decision,
    entries: Option<&'a [AppliedEntry]>,
    authorized: Option<AttributionDigest>,
    policy: Option<&'a PolicyVersion>,
    expires_at: OffsetDateTime,
    now: OffsetDateTime,
}

/// Insert the record, reporting a lost primary-key race as [`TxError::Raced`]
/// so the whole transaction rolls back.
///
/// An expired row under the same key is deleted first: a replay past the
/// retention window is a new operation, so the key must be free for it.
async fn write_record(
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
        decision_blob: versioned_blob(record.decision)?,
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
    async fn lock_applicable(
        tx: &impl DBRunner,
        scope: &AccessScope,
        applicable: &ApplicableQuotas,
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
            if let Some(row) = quota_repo::find_by_id(tx, scope, id, true).await? {
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
    async fn evaluate(
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
                settle_elapsed_rows(tx, scope, quota, now, enqueuer).await?;
            }
            // @cpt-begin:cpt-cf-quota-enforcement-algo-evaluation-pipeline:p1:inst-pipe-apply
            let (entry, crossing) =
                debit_counter(tx, scope, quota, plan.amount, now, enqueuer).await?;
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
    let (consumed, period) = if quota.quota_type == QuotaType::Consumption {
        let latest = counter_repo::find_latest(tx, scope, quota.id.as_uuid()).await?;
        let window = window_of(quota, now);
        let consumed = latest
            .filter(|row| row.period_start <= now && now < row.period_end)
            .map_or(0, |row| u64::try_from(row.consumed).unwrap_or(0));
        (consumed, Some(window))
    } else {
        let row = counter_repo::find_allocation(tx, scope, quota.id.as_uuid()).await?;
        (
            row.map_or(0, |row| u64::try_from(row.in_flight).unwrap_or(0)),
            None,
        )
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

fn counters_of(entries: &[AppliedEntry]) -> Vec<CounterSnapshot> {
    entries
        .iter()
        .map(|entry| CounterSnapshot {
            quota_id: QuotaId::new(entry.quota_id),
            period_id: entry.period_id.map(PeriodId::new),
            value: entry.value,
        })
        .collect()
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
        // Each retry is a complete transaction.
        for attempt in 0..RACE_ATTEMPTS {
            let enqueuer = Arc::clone(&self.enqueuer);
            let clock = Arc::clone(&self.clock);
            let actor = actor.clone();
            let scope = scope.clone();
            let events = events.to_vec();
            let mutation = OwnedMutation::of(mutation);
            let result = self
                .db
                .transaction_ref_mapped(move |tx| {
                    Box::pin(async move {
                        let scope = &scope;
                        let mutation = &mutation;
                        let events = &events;
                        let quotas = Self::lock_applicable(tx, scope, &mutation.applicable).await?;
                        // Sample time after locking so boundary waits charge the
                        // period in which the transaction commits.
                        let now = clock();
                        if let Replay::Stored(row) =
                            replay_of(tx, scope, &mutation.idempotency, now, true).await?
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
                        let (policy, decision) =
                            Self::evaluate(tx, scope, &quotas, mutation, now).await?;
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
                            Self::apply_plan(tx, scope, &quotas, &decision, now, &enqueuer).await?
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
                                decision: &decision,
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
                                        actor: &actor,
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
                        Ok::<_, TxError>(TransitionOutcome::Applied(EvaluatedDebit {
                            decision,
                            mutation: result,
                            retention: Retention::Recorded { expires_at },
                        }))
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
        // The Quota row lock normally serializes credits; the arbiter is a
        // defensive fallback.
        for attempt in 0..RACE_ATTEMPTS {
            let actor = actor.clone();
            let enqueuer = Arc::clone(&self.enqueuer);
            let clock = Arc::clone(&self.clock);
            let scope = scope.clone();
            let events = events.to_vec();
            let idempotency = idempotency.clone();
            let result = self
                .db
                .transaction_ref_mapped(move |tx| {
                    Box::pin(async move {
                        let scope = &scope;
                        let events: &[NotificationEvent] = &events;
                        // Check replay before guards that apply only to fresh credits.
                        // @cpt-begin:cpt-cf-quota-enforcement-flow-credit:p1:inst-cre-lock
                        let row = quota_repo::find_by_id(tx, scope, quota_id.as_uuid(), true)
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
                        // @cpt-begin:cpt-cf-quota-enforcement-flow-credit:p1:inst-cre-idem
                        if let Replay::Stored(stored) =
                            replay_of(tx, scope, &write, now, true).await?
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
                            let latest =
                                counter_repo::find_latest_for_update(tx, scope, quota_id.as_uuid())
                                    .await?;
                            if latest.is_some_and(|row| now >= row.period_end) {
                                return Err(StorageError::PeriodClosed.into());
                            }
                            // @cpt-end:cpt-cf-quota-enforcement-flow-credit:p1:inst-cre-guard
                            // @cpt-end:cpt-cf-quota-enforcement-flow-credit:p1:inst-cre-guard-if
                            let row = ensure_current_period(tx, scope, &quota, now).await?;
                            settle_elapsed_rows(tx, scope, &quota, now, &enqueuer).await?;
                            Some(row.period_id)
                        } else {
                            None
                        };
                        // @cpt-begin:cpt-cf-quota-enforcement-flow-credit:p1:inst-cre-apply
                        let value =
                            credit_counter(tx, scope, quota_id.as_uuid(), period_id, amount, now)
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
                                decision: &decision,
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
        // The original row lock normally serializes rollbacks; the arbiter also
        // protects a reused key that reaches different Quotas.
        for attempt in 0..RACE_ATTEMPTS {
            let actor = actor.clone();
            let enqueuer = Arc::clone(&self.enqueuer);
            let clock = Arc::clone(&self.clock);
            let scope = scope.clone();
            let events = events.to_vec();
            let idempotency = idempotency.clone();
            let target = target.clone();
            let result = self
                .db
                .transaction_ref_mapped(move |tx| {
                    Box::pin(async move {
                        let scope = &scope;
                        let events: &[NotificationEvent] = &events;
                        let target = &target;
                        let idempotency = &idempotency;
                        let now = clock();
                        // Check the rollback key first so replay outlives the
                        // original record's retention window.
                        // @cpt-begin:cpt-cf-quota-enforcement-flow-rollback:p1:inst-rlb-idem
                        if let Replay::Stored(stored) =
                            replay_of(tx, scope, idempotency, now, true).await?
                        {
                            return Ok(TransitionOutcome::NoOp(AppliedMutation {
                                decision: decision_of(&stored.decision_blob)?,
                                mutation: MutationResult::default(),
                                expires_at: stored.expires_at,
                            }));
                        }
                        // @cpt-end:cpt-cf-quota-enforcement-flow-rollback:p1:inst-rlb-idem
                        // @cpt-begin:cpt-cf-quota-enforcement-flow-rollback:p1:inst-rlb-unknown-if
                        // @cpt-begin:cpt-cf-quota-enforcement-flow-rollback:p1:inst-rlb-unknown
                        let unknown = || StorageError::OperationNotFound {
                            key: target.original.key.clone(),
                        };
                        // @cpt-begin:cpt-cf-quota-enforcement-flow-rollback:p1:inst-rlb-lookup
                        let original_key = scope_key_of(&target.original);
                        let original = idem_repo::find(tx, scope, &original_key, now, true)
                            .await?
                            .ok_or_else(unknown)?;
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
                            None => return Err(unknown().into()),
                        };
                        if entries.is_empty() {
                            return Err(unknown().into());
                        }
                        // @cpt-end:cpt-cf-quota-enforcement-flow-rollback:p1:inst-rlb-unknown
                        // @cpt-end:cpt-cf-quota-enforcement-flow-rollback:p1:inst-rlb-unknown-if
                        let already_reversed = original.reversed_by_key.is_some();
                        let mut reversed = Vec::new();
                        let mut quotas = Vec::new();
                        if !already_reversed {
                            let mut ordered = entries.clone();
                            ordered.sort_by_key(|entry| entry.quota_id);
                            for entry in &ordered {
                                let row = quota_repo::find_by_id(tx, scope, entry.quota_id, true)
                                    .await?
                                    .ok_or_else(unknown)?;
                                // A Quota deactivated since is still reversed:
                                // deactivation forbids new consumption, not the
                                // undo of a committed one.
                                let quota = quota_mapping::row_to_quota(row)?;
                                if let Some(period_id) = entry.period_id {
                                    let period = counter_repo::find_by_period_id_for_update(
                                        tx, scope, period_id,
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
                            // @cpt-begin:cpt-cf-quota-enforcement-flow-rollback:p1:inst-rlb-apply
                            for (entry, quota) in ordered.iter().zip(&quotas) {
                                let value = credit_counter(
                                    tx,
                                    scope,
                                    entry.quota_id,
                                    entry.period_id,
                                    entry.amount,
                                    now,
                                )
                                .await?;
                                reversed.push(AppliedEntry {
                                    value,
                                    ..entry.clone()
                                });
                                let _ = quota;
                            }
                        }
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
                        let expires_at =
                            expires_at(tx, idempotency.scope.tenant_id, &metric, now).await?;
                        write_record(
                            tx,
                            scope,
                            &RecordWrite {
                                write: idempotency,
                                decision: &decision,
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
                            idem_repo::mark_reversed(
                                tx,
                                scope,
                                &original_key,
                                &idempotency.scope.key,
                            )
                            .await?;
                            for (entry, quota) in reversed.iter().zip(&quotas) {
                                operation_log_repo::append(
                                    tx,
                                    scope,
                                    Entry {
                                        tenant_id: quota.tenant_id.as_uuid(),
                                        quota_id: entry.quota_id,
                                        operation: operation_log_repo::OP_ROLLBACK,
                                        actor: &actor,
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
                                        "principal": actor.subject_id,
                                    }),
                                    now,
                                );
                                enqueuer
                                    .enqueue_all(tx, std::slice::from_ref(&event))
                                    .await?;
                            }
                            enqueuer.enqueue_all(tx, events).await?;
                            // @cpt-end:cpt-cf-quota-enforcement-flow-rollback:p1:inst-rlb-apply
                        }
                        Ok::<_, TxError>(TransitionOutcome::Applied(AppliedMutation {
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
            let Some(row) = quota_repo::find_by_id(&conn, scope, id, false)
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
                            ensure_current_period(tx, &scope, &quota, clock()).await?;
                            Ok::<_, TxError>(())
                        })
                    })
                    .await
                    .map_err(|error| lift(OPERATION, error))?;
            }
            snapshots.push(
                snapshot_of(&conn, scope, &quota, now)
                    .await
                    .map_err(|error| lift(OPERATION, error))?,
            );
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
        let row = idem_repo::find(&conn, &scope, &key, now, false)
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
        let conn = self
            .db
            .conn()
            .map_err(|error| unavailable(OPERATION, "connection", &error))?;
        idem_repo::delete_expired(&conn, &AccessScope::allow_all(), batch_size, before)
            .await
            .map_err(|error| lift(OPERATION, error.into()))
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
    /// After losing the record's primary key, read the record that won.
    ///
    /// `None` means the winner rolled back too, so the operation simply runs
    /// again. A record under the same scope carrying a different payload is
    /// the caller's conflict, not a race to retry.
    async fn winner_of(
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
        let Some(row) = idem_repo::find(&conn, &scope, &key, now, false)
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

fn actor_of(ctx: &SecurityContext) -> Actor {
    Actor {
        subject_id: ctx.subject_id(),
        subject_type: None,
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "consumption_store_tests.rs"]
mod tests;
