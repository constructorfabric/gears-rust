//! The atomic batch debit: several debits of one logical operation, admitted
//! or refused as a whole, in one transaction.
//!
//! # Lock order
//!
//! Every item's applicable Quotas are discovered under that item's own scope,
//! merged into one set, and locked once in ascending `quota_id` order (rank
//! 1); locking per item would sort each item's set on its own, and two
//! batches could then take the same rows in opposite orders. The envelope's
//! stripe follows (rank 2), then every counter row a write can touch (rank 5):
//! elapsed unsettled periods, the latest period, or the allocation row, the
//! rows settlement, materialization and the debit itself would lock later.
//!
//! # Evaluation
//!
//! Holding those counter rows, the batch reads each snapshot once; a writer
//! that lowers a counter (a lease settlement, the sweeper, the cascade) holds
//! the row before it stamps a hold, so counter and correction come from one
//! state. Items are then evaluated in submission order against those
//! snapshots, each allowed item's plan added in memory for the items after
//! it. A denial does not stop the loop: every item gets its own decision.
//! Nothing is written until the loop ends. When every item is allowed the
//! union of the plans is applied through the single debit's own write path;
//! otherwise only the envelope's record is written.
//!
//! # Batch timeout
//!
//! The batch timer is armed once the locks are held, and the evaluation (the
//! snapshot reads and the item loop) runs under what remains of it. Each
//! engine gets that remainder as its budget. The write phase and the commit
//! are never cancelled, so a batch is never reported timed out after its
//! effects committed.

use std::collections::BTreeMap;
use std::num::NonZeroU64;
use std::sync::Arc;

use quota_enforcement_sdk::engine::{EngineError, EvaluationBudget};
use quota_enforcement_sdk::{
    BatchRecord, BatchTimer, Decision, DecisionResult, EvaluatedBatch, EvaluatedDebit,
    EvaluatedMutation, EvaluationFailure, IdempotencyWrite, MutationResult, NO_APPLICABLE_QUOTA,
    NotificationEvent, PolicyVersion, Quota, QuotaSnapshot, QuotaStatus, Retention, StorageError,
    TransitionOutcome,
};
use time::OffsetDateTime;
use toolkit_db::secure::DBRunner;
use toolkit_security::{AccessScope, SecurityContext};
use uuid::Uuid;

use super::consumption_store::{
    Applied, Clock, LockedCounter, OwnedMutation, RACE_ATTEMPTS, RecordBlob, RecordWrite, Replay,
    SqlConsumptionStore, TxError, actor_of, counters_of, expires_at, lift, lock_counters_of,
    replay_of, snapshot_of_locked, write_record,
};
use super::locking::{ContentionBudget, lock_scopes, with_budget_within};
use super::quota_mapping;
use super::repo::RowWait;
use super::repo::operation_log_repo::{self, Entry, OP_BATCH_DEBIT};
use super::repo::quota_repo;
use crate::domain::ports::Actor;
use crate::infra::outbox::NotificationEnqueuer;

const OPERATION: &str = "apply batch debit";

/// A batch owned by value, so the transaction closure can hold it.
struct OwnedBatch {
    envelope: IdempotencyWrite,
    items: Vec<OwnedItem>,
    cost_limit: NonZeroU64,
    timer: Arc<BatchTimer>,
}

/// One item: its mutation, and the scope its own authorization produced.
struct OwnedItem {
    mutation: OwnedMutation,
    scope: AccessScope,
}

impl OwnedBatch {
    fn of(batch: &EvaluatedBatch<'_>) -> Self {
        let items = batch
            .items
            .iter()
            .map(|entry| {
                let item = entry.item;
                let idempotency = IdempotencyWrite {
                    scope: item
                        .item_scope
                        .clone()
                        .unwrap_or_else(|| batch.envelope.scope.clone()),
                    payload_hash: batch.envelope.payload_hash,
                };
                OwnedItem {
                    mutation: OwnedMutation::of(&EvaluatedMutation {
                        applicable: &item.applicable,
                        amount: item.amount,
                        request: &item.request,
                        resource: &item.resource,
                        user_projection: entry.user_projection,
                        limits: batch.limits,
                        idempotency: &idempotency,
                        authorized: item.authorized,
                        evaluate: Arc::clone(&batch.evaluate),
                    }),
                    scope: entry.scope.clone(),
                }
            })
            .collect();
        Self {
            envelope: batch.envelope.clone(),
            items,
            cost_limit: batch.limits.cost_limit,
            timer: Arc::clone(&batch.timer),
        }
    }
}

/// What the transaction acts as and with.
struct BatchTx<'a> {
    /// The envelope's scope: the stripe and the record.
    scope: &'a AccessScope,
    events: &'a [NotificationEvent],
    actor: &'a Actor,
    clock: &'a Clock,
    enqueuer: &'a Arc<dyn NotificationEnqueuer>,
}

/// Every applicable Quota of the batch, locked once.
struct LockedUnion {
    /// The active Quotas, by id.
    quotas: BTreeMap<Uuid, Quota>,
    /// For each Quota, the index of the first item that discovered it: its
    /// rows are read and locked under that item's scope.
    owner: BTreeMap<Uuid, usize>,
    /// Each item's own applicable Quota ids.
    members: Vec<Vec<Uuid>>,
}

impl LockedUnion {
    /// The locked, active Quotas of item `index`.
    fn quotas_of(&self, index: usize) -> Vec<Quota> {
        self.members
            .get(index)
            .into_iter()
            .flatten()
            .filter_map(|id| self.quotas.get(id).cloned())
            .collect()
    }
}

/// Every item's decision, and the policy the record names.
struct Evaluated {
    decisions: Vec<Decision>,
    policy: Option<PolicyVersion>,
}

// @cpt-algo:cpt-cf-quota-enforcement-algo-batch-envelope-evaluation:p1
// @cpt-state:cpt-cf-quota-enforcement-state-batch-envelope:p1
// @cpt-dod:cpt-cf-quota-enforcement-dod-batch-atomic-envelope:p1
impl SqlConsumptionStore {
    /// Evaluate and apply an atomic batch.
    ///
    /// # Errors
    ///
    /// As the contract documents for `apply_batch_debit`.
    pub async fn apply_batch_debit_impl(
        &self,
        ctx: &SecurityContext,
        scope: &AccessScope,
        batch: &EvaluatedBatch<'_>,
        events: &[NotificationEvent],
    ) -> Result<TransitionOutcome<Vec<EvaluatedDebit>>, StorageError> {
        let actor = actor_of(ctx);
        let budget = self.batch_budget(batch).await?;
        for attempt in 0..RACE_ATTEMPTS {
            // A refused lock rolls the transaction back and runs it again,
            // within the contention budget and, once armed, the batch timer.
            let result = with_budget_within(budget, Some(batch.timer.as_ref()), || {
                let enqueuer = Arc::clone(&self.enqueuer);
                let clock = Arc::clone(&self.clock);
                let actor = actor.clone();
                let scope = scope.clone();
                let events = events.to_vec();
                let owned = OwnedBatch::of(batch);
                self.db.transaction_ref_mapped(move |tx| {
                    Box::pin(async move {
                        if owned.timer.expired() {
                            return Err(TxError::Storage(StorageError::BatchTimeout));
                        }
                        let env = BatchTx {
                            scope: &scope,
                            events: &events,
                            actor: &actor,
                            clock: &clock,
                            enqueuer: &enqueuer,
                        };
                        Self::batch_in_tx(tx, &env, &owned).await
                    })
                })
            })
            .await;
            match result {
                Ok(outcome) => return Ok(outcome),
                Err(TxError::Raced(lost)) => {
                    // The winner's record answers; if it has gone, run again.
                    if let Some(record) = self.winner_of(&lost).await? {
                        return replayed(&record.decision_blob, record.expires_at)
                            .map(TransitionOutcome::NoOp)
                            .map_err(|error| lift(OPERATION, error));
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
            TxError::Raced(Box::new(batch.envelope.clone())),
        ))
    }

    /// The strictest contention budget of the batch's metrics: a batch waits
    /// on a row no longer than any of its metrics allows (I8).
    async fn batch_budget(
        &self,
        batch: &EvaluatedBatch<'_>,
    ) -> Result<ContentionBudget, StorageError> {
        let mut metrics: Vec<&str> = batch
            .items
            .iter()
            .map(|entry| entry.item.applicable.metric.as_str())
            .collect();
        metrics.sort_unstable();
        metrics.dedup();
        let mut strictest: Option<ContentionBudget> = None;
        for metric in metrics {
            let budget = self.contention_budget(Some(metric)).await?;
            strictest = Some(strictest.map_or(budget, |current| current.stricter(budget)));
        }
        match strictest {
            Some(budget) => Ok(budget),
            None => self.contention_budget(None).await,
        }
    }

    /// One attempt, in the caller's transaction.
    async fn batch_in_tx(
        tx: &impl DBRunner,
        env: &BatchTx<'_>,
        batch: &OwnedBatch,
    ) -> Result<TransitionOutcome<Vec<EvaluatedDebit>>, TxError> {
        // @cpt-begin:cpt-cf-quota-enforcement-algo-batch-envelope-evaluation:p1:inst-bev-resolve
        // @cpt-begin:cpt-cf-quota-enforcement-algo-batch-envelope-evaluation:p1:inst-bev-union
        let union = lock_union(tx, batch).await?;
        lock_scopes(tx, &[&batch.envelope.scope]).await?;
        // @cpt-end:cpt-cf-quota-enforcement-algo-batch-envelope-evaluation:p1:inst-bev-union
        // @cpt-end:cpt-cf-quota-enforcement-algo-batch-envelope-evaluation:p1:inst-bev-resolve
        let now = (env.clock)();
        // @cpt-begin:cpt-cf-quota-enforcement-state-batch-envelope:p1:inst-best-enter
        // A record committed since the gear's lookup short-circuits here;
        // only a miss enters evaluation.
        if let Replay::Stored(row) =
            replay_of(tx, env.scope, &batch.envelope, now, Some(RowWait::Nowait)).await?
        {
            return Ok(TransitionOutcome::NoOp(replayed(
                &row.decision_blob,
                row.expires_at,
            )?));
        }
        // @cpt-end:cpt-cf-quota-enforcement-state-batch-envelope:p1:inst-best-enter
        let counters = lock_union_counters(tx, batch, &union, now).await?;
        // @cpt-begin:cpt-cf-quota-enforcement-algo-batch-envelope-evaluation:p1:inst-bev-timeout
        let remaining = batch.timer.arm();
        let evaluated =
            tokio::time::timeout(remaining, evaluate_items(tx, batch, &union, &counters, now))
                .await
                .map_err(|_| TxError::Storage(StorageError::BatchTimeout))??;
        // @cpt-end:cpt-cf-quota-enforcement-algo-batch-envelope-evaluation:p1:inst-bev-timeout
        // @cpt-begin:cpt-cf-quota-enforcement-algo-batch-envelope-evaluation:p1:inst-bev-fire-if
        // @cpt-begin:cpt-cf-quota-enforcement-algo-batch-envelope-evaluation:p1:inst-bev-fire
        // @cpt-begin:cpt-cf-quota-enforcement-state-batch-envelope:p1:inst-best-rollback
        // The last check before the write phase: past it nothing is cancelled.
        if batch.timer.expired() {
            return Err(TxError::Storage(StorageError::BatchTimeout));
        }
        // @cpt-end:cpt-cf-quota-enforcement-state-batch-envelope:p1:inst-best-rollback
        // @cpt-end:cpt-cf-quota-enforcement-algo-batch-envelope-evaluation:p1:inst-bev-fire
        // @cpt-end:cpt-cf-quota-enforcement-algo-batch-envelope-evaluation:p1:inst-bev-fire-if
        Self::write_batch(tx, env, batch, &union, evaluated, now).await
    }

    /// The write phase: apply the union when every item is allowed, then
    /// record the envelope.
    async fn write_batch(
        tx: &impl DBRunner,
        env: &BatchTx<'_>,
        batch: &OwnedBatch,
        union: &LockedUnion,
        evaluated: Evaluated,
        now: OffsetDateTime,
    ) -> Result<TransitionOutcome<Vec<EvaluatedDebit>>, TxError> {
        let allowed = evaluated
            .decisions
            .iter()
            .all(|decision| matches!(decision.result, DecisionResult::Allowed));
        // @cpt-begin:cpt-cf-quota-enforcement-algo-batch-envelope-evaluation:p1:inst-bev-apply-if
        // @cpt-begin:cpt-cf-quota-enforcement-algo-batch-envelope-evaluation:p1:inst-bev-apply
        // @cpt-begin:cpt-cf-quota-enforcement-state-batch-envelope:p1:inst-best-applied
        let applied = if allowed {
            Self::apply_items(tx, env, batch, union, &evaluated.decisions, now).await?
        } else {
            Vec::new()
        };
        // @cpt-end:cpt-cf-quota-enforcement-state-batch-envelope:p1:inst-best-applied
        // @cpt-end:cpt-cf-quota-enforcement-algo-batch-envelope-evaluation:p1:inst-bev-apply
        // @cpt-end:cpt-cf-quota-enforcement-algo-batch-envelope-evaluation:p1:inst-bev-apply-if
        // @cpt-begin:cpt-cf-quota-enforcement-algo-batch-envelope-evaluation:p1:inst-bev-denied-if
        // @cpt-begin:cpt-cf-quota-enforcement-algo-batch-envelope-evaluation:p1:inst-bev-denied
        // @cpt-begin:cpt-cf-quota-enforcement-state-batch-envelope:p1:inst-best-denied
        // Like a single debit, a denial for want of any Quota records nothing:
        // provisioning one must change the answer.
        if evaluated
            .decisions
            .iter()
            .any(|decision| decision.denied_reason() == Some(NO_APPLICABLE_QUOTA))
        {
            return Ok(TransitionOutcome::Applied(unrecorded(evaluated.decisions)));
        }
        let deadline = longest_retention(tx, batch, now).await?;
        let blob = serde_json::to_string(&BatchRecord::new(evaluated.decisions.clone()))?;
        write_record(
            tx,
            env.scope,
            &RecordWrite {
                write: &batch.envelope,
                blob: RecordBlob::Verbatim(&blob),
                // Not reversible by a rollback in P1, so no entries.
                entries: None,
                authorized: None,
                policy: evaluated.policy.as_ref(),
                expires_at: deadline,
                now,
            },
        )
        .await?;
        // @cpt-end:cpt-cf-quota-enforcement-state-batch-envelope:p1:inst-best-denied
        // @cpt-end:cpt-cf-quota-enforcement-algo-batch-envelope-evaluation:p1:inst-bev-denied
        // @cpt-end:cpt-cf-quota-enforcement-algo-batch-envelope-evaluation:p1:inst-bev-denied-if
        let moved = applied.iter().any(|item| !item.entries.is_empty());
        if moved {
            env.enqueuer.enqueue_all(tx, env.events).await?;
        }
        let event_ids: Vec<_> = if moved {
            env.events.iter().map(|event| event.event_id).collect()
        } else {
            Vec::new()
        };
        // @cpt-begin:cpt-cf-quota-enforcement-algo-batch-envelope-evaluation:p1:inst-bev-return
        Ok(TransitionOutcome::Applied(
            evaluated
                .decisions
                .into_iter()
                .enumerate()
                .map(|(index, decision)| EvaluatedDebit {
                    decision,
                    mutation: applied
                        .get(index)
                        .map_or_else(MutationResult::default, |item| MutationResult {
                            counters: counters_of(&item.entries),
                            threshold_crossings: item.crossings.clone(),
                            event_ids: event_ids.clone(),
                        }),
                    retention: Retention::Recorded {
                        expires_at: deadline,
                    },
                })
                .collect(),
        ))
        // @cpt-end:cpt-cf-quota-enforcement-algo-batch-envelope-evaluation:p1:inst-bev-return
    }

    /// Apply every item's plan in submission order through the single
    /// debit's write path, logging each counter movement.
    async fn apply_items(
        tx: &impl DBRunner,
        env: &BatchTx<'_>,
        batch: &OwnedBatch,
        union: &LockedUnion,
        decisions: &[Decision],
        now: OffsetDateTime,
    ) -> Result<Vec<Applied>, TxError> {
        let mut applied = Vec::with_capacity(decisions.len());
        for (index, (item, decision)) in batch.items.iter().zip(decisions).enumerate() {
            let quotas = union.quotas_of(index);
            let moved =
                Self::apply_plan(tx, &item.scope, &quotas, decision, now, env.enqueuer).await?;
            for entry in &moved.entries {
                operation_log_repo::append(
                    tx,
                    &item.scope,
                    Entry {
                        tenant_id: item.mutation.applicable.tenant_id.as_uuid(),
                        quota_id: entry.quota_id,
                        operation: OP_BATCH_DEBIT,
                        actor: env.actor,
                        record_version: 1,
                        detail: String::new(),
                        occurred_at: now,
                    },
                )
                .await?;
            }
            applied.push(moved);
        }
        Ok(applied)
    }
}

/// Discover every item's applicable Quotas under its own scope, then lock the
/// union once, ascending, dropping any row no longer active.
async fn lock_union(tx: &impl DBRunner, batch: &OwnedBatch) -> Result<LockedUnion, TxError> {
    let mut owner = BTreeMap::new();
    let mut members = Vec::with_capacity(batch.items.len());
    for (index, item) in batch.items.iter().enumerate() {
        let applicable = &item.mutation.applicable;
        let subjects: Vec<(String, String)> = applicable
            .subjects
            .iter()
            .map(|s| (s.projection_type.to_string(), s.subject_id.clone()))
            .collect();
        let ids = quota_repo::find_applicable_ids(
            tx,
            &item.scope,
            applicable.tenant_id.as_uuid(),
            applicable.metric.as_str(),
            &subjects,
        )
        .await?;
        for id in &ids {
            owner.entry(*id).or_insert(index);
        }
        members.push(ids);
    }
    let mut quotas = BTreeMap::new();
    for (id, index) in &owner {
        let scope = &batch.items[*index].scope;
        if let Some(row) = quota_repo::find_by_id(tx, scope, *id, Some(RowWait::Nowait)).await? {
            let quota = quota_mapping::row_to_quota(row)?;
            if quota.status == QuotaStatus::Active {
                quotas.insert(*id, quota);
            }
        }
    }
    Ok(LockedUnion {
        quotas,
        owner,
        members,
    })
}

/// Lock the counter rows of the union, ascending by Quota, each Quota's rows
/// in the order every writer takes them.
async fn lock_union_counters(
    tx: &impl DBRunner,
    batch: &OwnedBatch,
    union: &LockedUnion,
    now: OffsetDateTime,
) -> Result<BTreeMap<Uuid, LockedCounter>, TxError> {
    let mut counters = BTreeMap::new();
    for (id, quota) in &union.quotas {
        let scope = owner_scope(batch, union, *id);
        counters.insert(
            *id,
            lock_counters_of(tx, scope, quota, now, RowWait::Nowait).await?,
        );
    }
    Ok(counters)
}

/// Read the snapshots, then evaluate every item in order, each against what
/// the allowed items before it would leave.
async fn evaluate_items(
    tx: &impl DBRunner,
    batch: &OwnedBatch,
    union: &LockedUnion,
    counters: &BTreeMap<Uuid, LockedCounter>,
    now: OffsetDateTime,
) -> Result<Evaluated, TxError> {
    let mut snapshots = BTreeMap::new();
    for (id, quota) in &union.quotas {
        if let Some(counter) = counters.get(id) {
            let scope = owner_scope(batch, union, *id);
            snapshots.insert(
                *id,
                snapshot_of_locked(tx, scope, quota, counter, now).await?,
            );
        }
    }
    let mut decisions = Vec::with_capacity(batch.items.len());
    let mut policy = None;
    // @cpt-begin:cpt-cf-quota-enforcement-algo-batch-envelope-evaluation:p1:inst-bev-loop
    for (index, item) in batch.items.iter().enumerate() {
        // @cpt-begin:cpt-cf-quota-enforcement-algo-batch-envelope-evaluation:p1:inst-bev-deadline
        let budget = EvaluationBudget::within(batch.timer.remaining(), batch.cost_limit)
            .ok_or(TxError::Storage(StorageError::BatchTimeout))?;
        // @cpt-end:cpt-cf-quota-enforcement-algo-batch-envelope-evaluation:p1:inst-bev-deadline
        let seen: Vec<QuotaSnapshot> = union
            .members
            .get(index)
            .into_iter()
            .flatten()
            .filter_map(|id| snapshots.get(id).cloned())
            .collect();
        // @cpt-begin:cpt-cf-quota-enforcement-algo-batch-envelope-evaluation:p1:inst-bev-evaluate
        let evaluated =
            SqlConsumptionStore::decide(tx, &seen, &item.mutation, now, Some(budget)).await;
        // @cpt-end:cpt-cf-quota-enforcement-algo-batch-envelope-evaluation:p1:inst-bev-evaluate
        // @cpt-begin:cpt-cf-quota-enforcement-algo-batch-envelope-evaluation:p1:inst-bev-error-if
        // @cpt-begin:cpt-cf-quota-enforcement-algo-batch-envelope-evaluation:p1:inst-bev-error
        // A canonical error on any item fails the envelope; the transaction
        // rolls back and nothing persists.
        let (selected, decision) = evaluated.map_err(batch_timeout)?;
        // @cpt-end:cpt-cf-quota-enforcement-algo-batch-envelope-evaluation:p1:inst-bev-error
        // @cpt-end:cpt-cf-quota-enforcement-algo-batch-envelope-evaluation:p1:inst-bev-error-if
        // @cpt-begin:cpt-cf-quota-enforcement-algo-batch-envelope-evaluation:p1:inst-bev-record
        // The record names the first item's policy; each decision carries its
        // own attribution.
        policy.get_or_insert(selected);
        if matches!(decision.result, DecisionResult::Allowed) {
            for (quota_id, plan) in &decision.debit_plan {
                if let Some(snapshot) = snapshots.get_mut(&quota_id.as_uuid()) {
                    snapshot.consumed = snapshot.consumed.saturating_add(plan.amount);
                    snapshot.remaining = snapshot
                        .cap
                        .map(|cap| cap.saturating_sub(snapshot.consumed));
                }
            }
        }
        decisions.push(decision);
        // @cpt-end:cpt-cf-quota-enforcement-algo-batch-envelope-evaluation:p1:inst-bev-record
    }
    // @cpt-end:cpt-cf-quota-enforcement-algo-batch-envelope-evaluation:p1:inst-bev-loop
    Ok(Evaluated { decisions, policy })
}

/// The scope a union Quota's rows are read under: the first item that found it.
fn owner_scope<'a>(batch: &'a OwnedBatch, union: &LockedUnion, id: Uuid) -> &'a AccessScope {
    union
        .owner
        .get(&id)
        .and_then(|index| batch.items.get(*index))
        .map_or_else(|| &batch.items[0].scope, |item| &item.scope)
}

/// Now plus the longest idempotency retention of any item's metric, so the
/// envelope replays for as long as any of its metrics promises.
async fn longest_retention(
    tx: &impl DBRunner,
    batch: &OwnedBatch,
    now: OffsetDateTime,
) -> Result<OffsetDateTime, TxError> {
    let mut longest = now;
    let mut seen: Vec<&str> = Vec::new();
    for item in &batch.items {
        let applicable = &item.mutation.applicable;
        let metric = applicable.metric.as_str();
        if seen.contains(&metric) {
            continue;
        }
        seen.push(metric);
        longest = longest.max(expires_at(tx, applicable.tenant_id, metric, now).await?);
    }
    Ok(longest)
}

/// An engine that ran out of the batch's time is the batch timing out.
fn batch_timeout(error: TxError) -> TxError {
    match error {
        TxError::Storage(StorageError::EvaluationFailed {
            failure: EvaluationFailure::Engine(EngineError::Timeout),
            ..
        }) => TxError::Storage(StorageError::BatchTimeout),
        other => other,
    }
}

/// The decisions of a stored envelope, as a replay answers them.
fn replayed(blob: &str, expires_at: OffsetDateTime) -> Result<Vec<EvaluatedDebit>, TxError> {
    let record: BatchRecord = serde_json::from_str(blob)?;
    Ok(record
        .decisions
        .into_iter()
        .map(|decision| EvaluatedDebit {
            decision,
            mutation: MutationResult::default(),
            retention: Retention::Recorded { expires_at },
        })
        .collect())
}

/// Decisions no record keeps.
fn unrecorded(decisions: Vec<Decision>) -> Vec<EvaluatedDebit> {
    decisions
        .into_iter()
        .map(|decision| EvaluatedDebit {
            decision,
            mutation: MutationResult::default(),
            retention: Retention::Unrecorded,
        })
        .collect()
}
