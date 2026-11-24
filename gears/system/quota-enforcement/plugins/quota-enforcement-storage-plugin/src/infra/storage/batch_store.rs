//! The atomic batch debit: several debits of one logical operation, admitted
//! or refused as a whole, in one transaction.
//!
//! # Lock order
//!
//! Every item's applicable Quotas are discovered under that item's own scope,
//! merged into one set, and locked once in ascending `quota_id` order (rank
//! 1); locking per item would sort each item's set on its own, and two
//! batches could then take the same rows in opposite orders. The stripes of
//! the envelope and of every item key follow (rank 2), so no single debit
//! under an item key commits beside the batch, then every counter row a write
//! can touch (rank 5):
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
//! union of the plans is applied through the single debit's own write path
//! and every item writes a `debit` charge under its own key; otherwise only
//! the envelope's record is written.
//!
//! # Item keys
//!
//! Items share the key space of single debits. Under the item keys' stripes,
//! and after the envelope's record has had its chance to replay, an item key
//! that already names a `debit` charge or the record of a denied debit fails
//! the whole batch as a payload mismatch: a batch is replayed only by its
//! envelope key, never from a single debit's result.
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
    NotificationEvent, OperationType, PolicyVersion, Quota, QuotaSnapshot, QuotaStatus, QuotaType,
    Retention, StorageError, TransitionOutcome, conditions_hold,
};
use time::OffsetDateTime;
use toolkit_db::secure::DBRunner;
use toolkit_security::AccessScope;
use uuid::Uuid;

use super::config_cache::ConfigSnapshot;
use super::consumption_store::{
    Applied, ChargeWrite, Clock, LockedCounter, OwnedMutation, RACE_ATTEMPTS, RecordBlob,
    RecordWrite, Replay, SqlConsumptionStore, TxError, charge_rows, counters_of, expires_at, lift,
    lock_counters_of, replay_of, snapshot_of_locked, write_record,
};
use super::locking::{ContentionBudget, lock_scopes, with_budget_within};
use super::quota_mapping;
use super::repo::RowWait;
use super::repo::idempotency_repo::{self as idem_repo, Inserted};
use super::repo::operation_log_repo::{self, Entry, OP_BATCH_DEBIT};
use super::repo::{charge_repo, quota_repo};
use crate::domain::ports::Actor;
use crate::infra::outbox::{AttemptWakes, NotificationEnqueuer, settling};

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
                OwnedItem {
                    mutation: OwnedMutation::of(&EvaluatedMutation {
                        applicable: &item.applicable,
                        amount: item.amount,
                        request: &item.request,
                        metric_id: item.metric_id.as_deref(),
                        limits: batch.limits,
                        idempotency: &item.idempotency,
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
    config: &'a ConfigSnapshot,
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

/// Every item's decision and the policy it selected; the envelope's record
/// names the first item's.
struct Evaluated {
    decisions: Vec<Decision>,
    policies: Vec<PolicyVersion>,
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
        actor: &Actor,
        scope: &AccessScope,
        batch: &EvaluatedBatch<'_>,
        events: &[NotificationEvent],
    ) -> Result<TransitionOutcome<Vec<EvaluatedDebit>>, StorageError> {
        let actor = actor.clone();
        let config = self.config().await?;
        // A batch waits on a row no longer than any of its metrics allows (I8).
        let metrics = batch
            .items
            .iter()
            .map(|entry| entry.item.applicable.metric.as_str());
        let budget = ContentionBudget::of(&config, metrics);
        for attempt in 0..RACE_ATTEMPTS {
            // A refused lock rolls the transaction back and runs it again,
            // within the contention budget and, once armed, the batch timer.
            let result = with_budget_within(budget, Some(batch.timer.as_ref()), || {
                let wakes = AttemptWakes::begin(&self.enqueuer);
                let enqueuer = wakes.enqueuer();
                let clock = Arc::clone(&self.clock);
                let actor = actor.clone();
                let scope = scope.clone();
                let events = events.to_vec();
                let owned = OwnedBatch::of(batch);
                let config = Arc::clone(&config);
                settling(
                    wakes,
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
                                config: &config,
                            };
                            Self::batch_in_tx(tx, &env, &owned).await
                        })
                    }),
                )
            })
            .await;
            match result {
                Ok(outcome) => return Ok(outcome),
                Err(TxError::Raced(lost)) => {
                    // The winner's record answers; if it has gone, run again.
                    if let Some(record) = self.winner_of(&lost).await? {
                        return replayed(&record.blob, record.expires_at)
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

    /// One attempt, in the caller's transaction.
    async fn batch_in_tx(
        tx: &impl DBRunner,
        env: &BatchTx<'_>,
        batch: &OwnedBatch,
    ) -> Result<TransitionOutcome<Vec<EvaluatedDebit>>, TxError> {
        // @cpt-begin:cpt-cf-quota-enforcement-algo-batch-envelope-evaluation:p1:inst-bev-resolve
        // @cpt-begin:cpt-cf-quota-enforcement-algo-batch-envelope-evaluation:p1:inst-bev-union
        let union = lock_union(tx, batch).await?;
        // @cpt-end:cpt-cf-quota-enforcement-algo-batch-envelope-evaluation:p1:inst-bev-union
        // @cpt-end:cpt-cf-quota-enforcement-algo-batch-envelope-evaluation:p1:inst-bev-resolve
        // @cpt-begin:cpt-cf-quota-enforcement-algo-batch-envelope-evaluation:p1:inst-bev-keylocks
        let keys: Vec<_> = std::iter::once(&batch.envelope.scope)
            .chain(
                batch
                    .items
                    .iter()
                    .map(|item| &item.mutation.idempotency.scope),
            )
            .collect();
        lock_scopes::<TxError>(tx, &keys).await?;
        let now = (env.clock)();
        // @cpt-begin:cpt-cf-quota-enforcement-state-batch-envelope:p1:inst-best-enter
        // A record committed since the gear's lookup short-circuits here;
        // only a miss enters evaluation.
        if let Replay::Stored(row) =
            replay_of(tx, env.scope, &batch.envelope, now, Some(RowWait::Nowait)).await?
        {
            return Ok(TransitionOutcome::NoOp(replayed(
                &row.blob,
                row.expires_at,
            )?));
        }
        // @cpt-end:cpt-cf-quota-enforcement-state-batch-envelope:p1:inst-best-enter
        // @cpt-end:cpt-cf-quota-enforcement-algo-batch-envelope-evaluation:p1:inst-bev-keylocks
        if let Some(index) = first_taken_item(tx, batch, now).await? {
            return Err(StorageError::IdempotencyPayloadMismatch
                .at_item(index)
                .into());
        }
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
        let deadline = longest_retention(env.config, batch, now);
        let blob = serde_json::to_string(&BatchRecord::new(evaluated.decisions.clone()))?;
        write_record(
            tx,
            env.scope,
            &RecordWrite {
                write: &batch.envelope,
                blob: RecordBlob::Verbatim(&blob),
                policy: evaluated.policies.first(),
                expires_at: deadline,
                now,
            },
        )
        .await?;
        if allowed {
            Self::charge_items(tx, env, union, batch, &evaluated, &applied, now).await?;
        }
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

    /// Write one `debit` charge per item under the item's key, all of them in
    /// two inserts under the envelope's scope: the batch is of one tenant, and
    /// a charge carries no other scope. The item keys were found free under
    /// their stripes, so a charge already under one is a writer that took no
    /// stripe, and the batch fails as a payload mismatch rather than answering
    /// with that charge.
    async fn charge_items(
        tx: &impl DBRunner,
        env: &BatchTx<'_>,
        union: &LockedUnion,
        batch: &OwnedBatch,
        evaluated: &Evaluated,
        applied: &[Applied],
        now: OffsetDateTime,
    ) -> Result<(), TxError> {
        let mut charges = Vec::with_capacity(batch.items.len());
        for (((item, decision), policy), moved) in batch
            .items
            .iter()
            .zip(&evaluated.decisions)
            .zip(&evaluated.policies)
            .zip(applied)
        {
            let mutation = &item.mutation;
            charges.push(charge_rows(&ChargeWrite {
                write: &mutation.idempotency,
                decision,
                policy: Some(policy),
                authorized: mutation.authorized,
                metric: mutation.applicable.metric.as_str(),
                amount: mutation.amount,
                entries: &moved.entries,
                return_window_end: expires_at(
                    env.config,
                    mutation.applicable.tenant_id,
                    mutation.applicable.metric.as_str(),
                    now,
                ),
                now,
            })?);
        }
        let (tenant_id, operation, keys) = item_keys(batch);
        charge_repo::delete_deleted_at_keys(tx, tenant_id, operation, &keys, now).await?;
        // Every Quota of an entry is one of the union's locked, active rows.
        let allocation = |quota_id| {
            union
                .quotas
                .get(&quota_id)
                .is_some_and(|quota| quota.quota_type == QuotaType::Allocation)
        };
        match charge_repo::insert_all(tx, env.scope, charges, allocation).await? {
            Inserted::Yes => Ok(()),
            Inserted::Raced => Err(StorageError::IdempotencyPayloadMismatch.into()),
        }
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
            let moved = Self::apply_plan(
                tx,
                &item.scope,
                env.actor,
                &quotas,
                decision,
                now,
                env.enqueuer,
            )
            .await?;
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
            .map(|s| (s.subject_type.to_string(), s.subject_id.clone()))
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
    // Each item keeps only the Quotas whose conditions hold on its own
    // request (I15), judged on the locked rows.
    for (ids, item) in members.iter_mut().zip(&batch.items) {
        ids.retain(|id| {
            quotas
                .get(id)
                .is_some_and(|quota| conditions_hold(&quota.metadata, &item.mutation.request))
        });
    }
    Ok(LockedUnion {
        quotas,
        owner,
        members,
    })
}

/// The tenant, the operation and the keys of the batch's items. A batch is of
/// one tenant, and every item is a debit.
fn item_keys(batch: &OwnedBatch) -> (Uuid, &'static str, Vec<&str>) {
    let keys = batch
        .items
        .iter()
        .map(|item| item.mutation.idempotency.scope.key.as_str())
        .collect();
    (
        batch.envelope.scope.tenant_id.as_uuid(),
        OperationType::Debit.as_str(),
        keys,
    )
}

/// The index of the first item whose key is taken: by a live charge or by an
/// unexpired record. Two reads for the whole batch, under the items' stripes.
async fn first_taken_item(
    tx: &impl DBRunner,
    batch: &OwnedBatch,
    now: OffsetDateTime,
) -> Result<Option<usize>, TxError> {
    let (tenant_id, operation, keys) = item_keys(batch);
    let mut taken = charge_repo::live_keys(tx, tenant_id, operation, &keys, now).await?;
    taken.extend(idem_repo::unexpired_keys(tx, tenant_id, operation, &keys, now).await?);
    Ok(keys.iter().position(|key| taken.iter().any(|t| t == key)))
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
    let mut policies = Vec::with_capacity(batch.items.len());
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
        policies.push(selected);
        if matches!(decision.result, DecisionResult::Allowed) {
            for plan in decision.debit_plan.iter().flatten() {
                if let Some(snapshot) = snapshots.get_mut(&plan.quota_id.as_uuid()) {
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
    Ok(Evaluated {
        decisions,
        policies,
    })
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
fn longest_retention(
    config: &ConfigSnapshot,
    batch: &OwnedBatch,
    now: OffsetDateTime,
) -> OffsetDateTime {
    now + config.longest_retention(batch.items.iter().map(|item| {
        let applicable = &item.mutation.applicable;
        (applicable.tenant_id, applicable.metric.as_str())
    }))
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
