//! The lease primitives of the storage contract, on the same rows and the same
//! transaction machinery as the consumption store.
//!
//! # What a lease adds to a debit
//!
//! An acquisition *is* a debit that can be taken back: it evaluates the policy
//! under the Quota locks and applies the plan exactly as
//! [`super::consumption_store`] does, then records a lease and one hold per
//! Quota so a later commit or release can settle it. Everything about the
//! evaluation, the idempotency record and the threshold routine is shared, not
//! restated.
//!
//! Two things are the lease's own:
//!
//! - **The acquisition period is fixed at acquisition** (I5). A hold carries the
//!   `period_id` it was taken against, and every settlement lands there whatever
//!   period the wall clock has since moved into.
//! - **Expiry releases capacity before any sweeper runs** (I4). Past `expiry_at`
//!   a lease is released semantically, so its hold must stop counting at once.
//!   The counter still carries it until someone gives it back, so the first
//!   writer to lock that counter row does exactly that and stamps the hold
//!   `returned_at`; the sweeper credits only what is still unstamped. Readers,
//!   which may not write, subtract the unstamped holds instead. Without that
//!   stamp a credit that floors at zero and a later sweep would between them
//!   erase usage that arrived in the meantime.
//!
//! # Lock rank
//!
//! One order for every primitive in this plugin (ADR-0002, extended past
//! Quota rows): Quota rows ascending by id, then idempotency records, then
//! lease rows ascending by token, then capacity rows, then counter rows, then
//! the hold rows under their counter, then inserts. A primitive may skip a
//! rank and never goes back; anything it must know before locking — a lease's
//! metric and holds, both immutable once written — is read unscoped of rank
//! first and re-read under the lock.
//!
//! Acquire, commit and release carry the contention budget (I8): every lock they
//! take is `NOWAIT`, and a refused one rolls the transaction back and retries it
//! until the budget is spent ([`super::locking`]). The sweeper has no budget and
//! queues.

use std::sync::Arc;
use std::time::Duration;

use quota_enforcement_sdk::{
    AppliedMutation, AttributionDigest, Decision, DecisionResult, EvaluatedLease,
    EvaluatedMutation, ExpiredLease, IdempotencySubjectKey, LeaseHold, LeaseToken, MetricId,
    MutationResult, NO_APPLICABLE_QUOTA, NotificationEvent, NotificationEventKind,
    NotificationScope, OperationType, PartialIdempotencyWrite, Quota, QuotaId, StorageError,
    TenantId, TransitionOutcome, apportion,
};
use time::OffsetDateTime;
use toolkit_db::secure::DBRunner;
use toolkit_security::{AccessScope, SecurityContext};
use uuid::Uuid;

use super::consumption_store::{
    AppliedEntry, OwnedMutation, RACE_ATTEMPTS, RecordBlob, RecordWrite, Replay,
    SqlConsumptionStore, TxError, actor_of, counters_of, credit_counter, debit_counter,
    decision_of, expires_at, lift, replay_of, settle_elapsed_rows, write_record,
};
use super::locking::{lock_scopes, with_budget};
use super::repo::lease_repo;
use super::repo::lease_repo::RowWait;
use super::repo::operation_log_repo::{self, Entry};
use crate::infra::outbox::NotificationEnqueuer;

/// Operation-log verbs of the lease primitives.
const OP_LEASE_ACQUIRE: &str = "lease_acquire";
const OP_LEASE_COMMIT: &str = "lease_commit";
const OP_LEASE_RELEASE: &str = "lease_release";
const OP_LEASE_AUTO_RELEASE: &str = "lease_auto_release";

// @cpt-flow:cpt-cf-quota-enforcement-flow-lease-acquire:p1
// @cpt-flow:cpt-cf-quota-enforcement-flow-lease-commit:p1
// @cpt-flow:cpt-cf-quota-enforcement-flow-lease-release:p1
// @cpt-algo:cpt-cf-quota-enforcement-algo-lazy-expiry:p1
// @cpt-algo:cpt-cf-quota-enforcement-algo-lease-sweep:p1
// @cpt-state:cpt-cf-quota-enforcement-state-lease:p1
// @cpt-dod:cpt-cf-quota-enforcement-dod-lease-acquisition:p1
// @cpt-dod:cpt-cf-quota-enforcement-dod-lease-attribution:p1
impl SqlConsumptionStore {
    /// Hold the evaluated plan on every Quota it names, or hold nothing.
    ///
    /// # Errors
    ///
    /// As the contract documents for `acquire_lease`.
    pub async fn acquire_lease_impl(
        &self,
        ctx: &SecurityContext,
        scope: &AccessScope,
        mutation: &EvaluatedMutation<'_>,
        ttl: Duration,
    ) -> Result<TransitionOutcome<EvaluatedLease>, StorageError> {
        const OPERATION: &str = "acquire lease";
        let actor = actor_of(ctx);
        let tenant = mutation.applicable.tenant_id;
        let metric = mutation.applicable.metric.clone();
        let budget = self.contention_budget(Some(metric.as_str())).await?;
        for attempt in 0..RACE_ATTEMPTS {
            // A refused lock rolls this transaction back and runs it
            // again under the budget; a race goes to the arbiter below.
            let result = with_budget(budget, || {
                let enqueuer = Arc::clone(&self.enqueuer);
                let clock = Arc::clone(&self.clock);
                let actor = actor.clone();
                let scope = scope.clone();
                let metric = metric.clone();
                let owned = OwnedMutation::of(mutation);
                self.db.transaction_ref_mapped(move |tx| {
                    Box::pin(async move {
                        let scope = &scope;
                        let owned = &owned;
                        // Rank 1: the Quota rows of the applicable set.
                        let quotas = SqlConsumptionStore::lock_applicable(
                            tx,
                            scope,
                            &owned.applicable,
                            RowWait::Nowait,
                        )
                        .await?;
                        // Every lock here is `NOWAIT`, so no time passes waiting
                        // inside this transaction: one reading of the clock
                        // serves the live count, the evaluation's period, the
                        // holds and the expiry alike. A refused lock retries the
                        // whole transaction, with a fresh reading.
                        let now = clock();
                        // Rank 2: the scope's stripe (I8), then the record.
                        lock_scopes(tx, &[&owned.idempotency.scope]).await?;
                        if let Replay::Stored(row) =
                            replay_of(tx, scope, &owned.idempotency, now, Some(RowWait::Nowait))
                                .await?
                        {
                            let acquired: EvaluatedLease = serde_json::from_str(&row.decision_blob)
                                .map_err(|error| {
                                    TxError::Storage(StorageError::Internal(error.to_string()))
                                })?;
                            return Ok(TransitionOutcome::NoOp(acquired));
                        }
                        let (policy, decision) =
                            SqlConsumptionStore::evaluate(tx, scope, &quotas, owned, now).await?;
                        if decision.denied_reason() == Some(NO_APPLICABLE_QUOTA) {
                            return Ok(TransitionOutcome::Applied(EvaluatedLease {
                                decision,
                                token: None,
                                expires_at: None,
                            }));
                        }
                        let allowed = matches!(decision.result, DecisionResult::Allowed)
                            && !decision.debit_plan.is_empty();
                        let (token, expiry) = if allowed {
                            // The cap is consulted only now: a verdict the
                            // engine refused is the caller's answer whether or
                            // not the cap is full.
                            let row = lease_repo::lock_capacity_row(
                                tx,
                                scope,
                                tenant.as_uuid(),
                                metric.as_str(),
                                RowWait::Nowait,
                            )
                            .await?
                            .ok_or_else(|| {
                                // Created with the pair's first Quota, and an
                                // acquisition only reaches a pair that has one.
                                TxError::Storage(StorageError::Internal(format!(
                                    "no lease capacity row for tenant {tenant} and metric {metric}"
                                )))
                            })?;
                            // @cpt-begin:cpt-cf-quota-enforcement-flow-lease-acquire:p1:inst-lac-cap-if
                            // @cpt-begin:cpt-cf-quota-enforcement-flow-lease-acquire:p1:inst-lac-cap
                            // @cpt-begin:cpt-cf-quota-enforcement-algo-lazy-expiry:p1:inst-lzy-cap
                            let live = lease_repo::count_live(
                                tx,
                                scope,
                                tenant.as_uuid(),
                                metric.as_str(),
                                now,
                            )
                            .await?;
                            // @cpt-end:cpt-cf-quota-enforcement-algo-lazy-expiry:p1:inst-lzy-cap
                            let cap =
                                SqlConsumptionStore::lease_cap(tx, tenant, metric.as_str()).await?;
                            if live >= cap {
                                return Err(TxError::Storage(
                                    StorageError::LeaseInflightLimitExceeded,
                                ));
                            }
                            // @cpt-end:cpt-cf-quota-enforcement-flow-lease-acquire:p1:inst-lac-cap
                            // @cpt-end:cpt-cf-quota-enforcement-flow-lease-acquire:p1:inst-lac-cap-if
                            // @cpt-begin:cpt-cf-quota-enforcement-flow-lease-acquire:p1:inst-lac-insert
                            let applied = SqlConsumptionStore::hold_plan(
                                tx, scope, &quotas, &decision, now, &enqueuer,
                            )
                            .await?;
                            let token = LeaseToken::from(Uuid::now_v7());
                            let expiry = now + ttl;
                            lease_repo::insert_lease(
                                tx,
                                scope,
                                &lease_repo::NewLease {
                                    token: token.as_uuid(),
                                    tenant_id: tenant.as_uuid(),
                                    metric: metric.as_str(),
                                    subject_key: owned.idempotency.scope.subject_key.as_bytes(),
                                    attribution_hash: owned.authorized.as_bytes(),
                                    idem_key: &owned.idempotency.scope.key,
                                    reserved_amount: i64::try_from(owned.amount).map_err(|_| {
                                        TxError::Storage(StorageError::Internal(
                                            "reserved amount does not fit the column".to_owned(),
                                        ))
                                    })?,
                                    now,
                                    expiry_at: expiry,
                                },
                            )
                            .await?;
                            for entry in &applied {
                                lease_repo::insert_hold(
                                    tx,
                                    scope,
                                    token.as_uuid(),
                                    tenant.as_uuid(),
                                    entry.quota_id,
                                    i64::try_from(entry.amount).map_err(|_| {
                                        TxError::Storage(StorageError::Internal(
                                            "held amount does not fit the column".to_owned(),
                                        ))
                                    })?,
                                    entry.period_id,
                                )
                                .await?;
                                operation_log_repo::append(
                                    tx,
                                    scope,
                                    Entry {
                                        tenant_id: tenant.as_uuid(),
                                        quota_id: entry.quota_id,
                                        operation: OP_LEASE_ACQUIRE,
                                        actor: &actor,
                                        record_version: 1,
                                        detail: String::new(),
                                        occurred_at: now,
                                    },
                                )
                                .await?;
                            }
                            lease_repo::bump_active_count(
                                tx,
                                scope,
                                tenant.as_uuid(),
                                metric.as_str(),
                                row.active_count,
                                1,
                                now,
                            )
                            .await?;
                            (Some(token), Some(expiry))
                            // @cpt-end:cpt-cf-quota-enforcement-flow-lease-acquire:p1:inst-lac-insert
                        } else {
                            (None, None)
                        };
                        let acquired = EvaluatedLease {
                            decision,
                            token,
                            expires_at: expiry,
                        };
                        // The acquisition's outcome is what replays, token and
                        // all: a subject may hold several leases, so replaying
                        // the decision alone would lose which one this was.
                        let blob = serde_json::to_string(&acquired).map_err(|error| {
                            TxError::Storage(StorageError::Internal(error.to_string()))
                        })?;
                        let deadline = expires_at(tx, tenant, metric.as_str(), now).await?;
                        // The blob is the whole outcome, not only the decision,
                        // so a replay returns the token this call issued.
                        write_record(
                            tx,
                            scope,
                            &RecordWrite {
                                write: &owned.idempotency,
                                blob: RecordBlob::Verbatim(&blob),
                                entries: None,
                                authorized: Some(owned.authorized),
                                policy: Some(&policy),
                                expires_at: deadline,
                                now,
                            },
                        )
                        .await?;
                        Ok::<_, TxError>(TransitionOutcome::Applied(acquired))
                    })
                })
            })
            .await;
            match result {
                Ok(outcome) => return Ok(outcome),
                Err(TxError::Raced(lost)) => {
                    if let Some(record) = self.winner_of(&lost).await? {
                        let acquired: EvaluatedLease = serde_json::from_str(&record.decision_blob)
                            .map_err(|error| StorageError::Internal(error.to_string()))?;
                        return Ok(TransitionOutcome::NoOp(acquired));
                    }
                    if attempt + 1 == RACE_ATTEMPTS {
                        return Err(lift(OPERATION, TxError::Raced(lost)));
                    }
                }
                Err(error) => return Err(lift(OPERATION, error)),
            }
        }
        Err(unavailable_arbiter(OPERATION))
    }

    /// Settle a lease: commit keeps its apportioned share, release keeps none.
    ///
    /// # Errors
    ///
    /// As the contract documents for `commit_lease` and `release_lease`.
    pub async fn settle_lease_impl(
        &self,
        ctx: &SecurityContext,
        scope: &AccessScope,
        token: LeaseToken,
        settlement: Settlement,
        idempotency: &PartialIdempotencyWrite,
        events: &[NotificationEvent],
    ) -> Result<TransitionOutcome<AppliedMutation>, StorageError> {
        let op_name = settlement.name();
        let operation = settlement.operation();
        let actor = actor_of(ctx);
        // Rank-free: the metric names the contention budget, which has to start
        // before the first lock, and the subject key completes the idempotency
        // scope. Both are immutable once written and re-read under the lock.
        let conn = self
            .db
            .conn()
            .map_err(|error| lift(op_name, TxError::Db(error)))?;
        let preview = lease_repo::find(&conn, scope, token.as_uuid())
            .await
            .map_err(|error| lift(op_name, error.into()))?
            .ok_or(StorageError::LeaseNotFound { token })?;
        if preview.tenant_id != idempotency.tenant_id.as_uuid() {
            return Err(StorageError::LeaseNotFound { token });
        }
        let subject_key = subject_key_of(&preview.subject_key)?;
        let write = idempotency.clone().complete(subject_key, operation);
        let budget = self.contention_budget(Some(&preview.metric)).await?;
        let tenant = idempotency.tenant_id;
        // A refused lock rolls this transaction back and runs it again under
        // the budget.
        with_budget(budget, || {
            let enqueuer = Arc::clone(&self.enqueuer);
            let clock = Arc::clone(&self.clock);
            let scope_owned = scope.clone();
            let events = events.to_vec();
            let write_owned = write.clone();
            let metric = preview.metric.clone();
            let actor = actor.clone();
            self.db.transaction_ref_mapped(move |tx| {
                Box::pin(async move {
                    let scope = &scope_owned;
                    let now = clock();
                    // Rank 2 first: the scope's stripe (I8), then the record,
                    // so a settlement that already succeeded replays even
                    // though its lease is no longer active.
                    // @cpt-begin:cpt-cf-quota-enforcement-flow-lease-commit:p1:inst-lcm-idem
                    // @cpt-begin:cpt-cf-quota-enforcement-flow-lease-release:p1:inst-lrl-idem
                    lock_scopes(tx, &[&write_owned.scope]).await?;
                    if let Replay::Stored(row) =
                        replay_of(tx, scope, &write_owned, now, Some(RowWait::Nowait)).await?
                    {
                        return Ok(TransitionOutcome::NoOp(AppliedMutation {
                            decision: decision_of(&row.decision_blob)?,
                            mutation: MutationResult::default(),
                            expires_at: row.expires_at,
                        }));
                    }
                    // @cpt-end:cpt-cf-quota-enforcement-flow-lease-release:p1:inst-lrl-idem
                    // @cpt-end:cpt-cf-quota-enforcement-flow-lease-commit:p1:inst-lcm-idem
                    // Rank 3.
                    // @cpt-begin:cpt-cf-quota-enforcement-flow-lease-commit:p1:inst-lcm-lock
                    let lease =
                        lease_repo::find_for_update(tx, scope, token.as_uuid(), RowWait::Nowait)
                            .await?
                            .ok_or(TxError::Storage(StorageError::LeaseNotFound { token }))?;
                    // @cpt-end:cpt-cf-quota-enforcement-flow-lease-commit:p1:inst-lcm-lock
                    // @cpt-begin:cpt-cf-quota-enforcement-flow-lease-commit:p1:inst-lcm-notactive-if
                    // @cpt-begin:cpt-cf-quota-enforcement-flow-lease-commit:p1:inst-lcm-notactive
                    // @cpt-begin:cpt-cf-quota-enforcement-flow-lease-release:p1:inst-lrl-notactive-if
                    // @cpt-begin:cpt-cf-quota-enforcement-flow-lease-release:p1:inst-lrl-notactive
                    // @cpt-begin:cpt-cf-quota-enforcement-algo-lazy-expiry:p1:inst-lzy-write
                    if lease.state != lease_repo::STATE_ACTIVE || lease.expiry_at <= now {
                        return Err(TxError::Storage(StorageError::LeaseNotActive { token }));
                    }
                    // @cpt-end:cpt-cf-quota-enforcement-algo-lazy-expiry:p1:inst-lzy-write
                    // @cpt-end:cpt-cf-quota-enforcement-flow-lease-release:p1:inst-lrl-notactive
                    // @cpt-end:cpt-cf-quota-enforcement-flow-lease-release:p1:inst-lrl-notactive-if
                    // @cpt-end:cpt-cf-quota-enforcement-flow-lease-commit:p1:inst-lcm-notactive
                    // @cpt-end:cpt-cf-quota-enforcement-flow-lease-commit:p1:inst-lcm-notactive-if
                    let holds = lease_repo::holds_of(tx, scope, token.as_uuid()).await?;
                    let held: Vec<u64> = holds
                        .iter()
                        .map(|hold| u64::try_from(hold.held_amount).unwrap_or(0))
                        .collect();
                    let reserved = u64::try_from(lease.reserved_amount).unwrap_or(0);
                    let actual = settlement.kept(reserved);
                    // @cpt-begin:cpt-cf-quota-enforcement-flow-lease-commit:p1:inst-lcm-overcommit-if
                    // @cpt-begin:cpt-cf-quota-enforcement-flow-lease-commit:p1:inst-lcm-overcommit
                    let kept = kept_of(&held, reserved, actual)?;
                    // @cpt-end:cpt-cf-quota-enforcement-flow-lease-commit:p1:inst-lcm-overcommit
                    // @cpt-end:cpt-cf-quota-enforcement-flow-lease-commit:p1:inst-lcm-overcommit-if
                    // @cpt-begin:cpt-cf-quota-enforcement-flow-lease-commit:p1:inst-lcm-apply
                    // @cpt-begin:cpt-cf-quota-enforcement-flow-lease-release:p1:inst-lrl-apply
                    // Rank 4.
                    let capacity = lease_repo::lock_capacity_row(
                        tx,
                        scope,
                        tenant.as_uuid(),
                        &metric,
                        RowWait::Nowait,
                    )
                    .await?;
                    // Rank 5 and 5b, ascending by `quota_id` as `holds_of`
                    // returns them.
                    let mut entries = Vec::new();
                    for (hold, kept) in holds.iter().zip(&kept) {
                        let held_amount = u64::try_from(hold.held_amount).unwrap_or(0);
                        let returned = held_amount.saturating_sub(*kept);
                        // Rank 5 before 5b: the counter row is taken first, and
                        // only then is its hold stamped. Stamping under a row
                        // this transaction does not hold would let a sweeper
                        // returning the same hold interleave between the two.
                        let value =
                            counter_value_of(tx, scope, hold.quota_id, hold.period_id).await?;
                        // The hold is settled here whatever its state: stamping
                        // it keeps a later sweep from returning it again.
                        let owed = lease_repo::mark_hold_returned(
                            tx,
                            scope,
                            token.as_uuid(),
                            hold.quota_id,
                            now,
                        )
                        .await?;
                        // @cpt-begin:cpt-cf-quota-enforcement-flow-lease-commit:p1:inst-lcm-boundary
                        let value = if owed && returned > 0 {
                            credit_counter(
                                tx,
                                scope,
                                hold.quota_id,
                                hold.period_id,
                                returned,
                                now,
                                RowWait::Nowait,
                            )
                            .await?
                        } else {
                            value
                        };
                        // @cpt-end:cpt-cf-quota-enforcement-flow-lease-commit:p1:inst-lcm-boundary
                        if *kept > 0 {
                            entries.push(AppliedEntry {
                                quota_id: hold.quota_id,
                                period_id: hold.period_id,
                                amount: *kept,
                                value,
                            });
                            operation_log_repo::append(
                                tx,
                                scope,
                                Entry {
                                    tenant_id: tenant.as_uuid(),
                                    quota_id: hold.quota_id,
                                    operation: settlement.log_verb(),
                                    actor: &actor,
                                    record_version: 1,
                                    detail: String::new(),
                                    occurred_at: now,
                                },
                            )
                            .await?;
                        }
                    }
                    // @cpt-begin:cpt-cf-quota-enforcement-state-lease:p1:inst-lst-commit
                    // @cpt-begin:cpt-cf-quota-enforcement-state-lease:p1:inst-lst-release
                    let state = settlement.state();
                    if !lease_repo::mark_state(tx, scope, token.as_uuid(), state, now).await? {
                        return Err(TxError::Storage(StorageError::LeaseNotActive { token }));
                    }
                    // @cpt-end:cpt-cf-quota-enforcement-state-lease:p1:inst-lst-release
                    // @cpt-end:cpt-cf-quota-enforcement-state-lease:p1:inst-lst-commit
                    // Diagnostic only, so a missing row costs the count, not the
                    // transition.
                    if let Some(capacity) = &capacity {
                        lease_repo::bump_active_count(
                            tx,
                            scope,
                            tenant.as_uuid(),
                            &metric,
                            capacity.active_count,
                            -1,
                            now,
                        )
                        .await?;
                    }
                    // Settling evaluates nothing: the plan was fixed at
                    // acquisition, so the recorded decision is what it kept.
                    let decision = Decision::allowed_with_plan(
                        entries
                            .iter()
                            .map(|entry| {
                                (
                                    QuotaId::from(entry.quota_id),
                                    quota_enforcement_sdk::QuotaDebitPlan {
                                        amount: entry.amount,
                                    },
                                )
                            })
                            .collect(),
                    );
                    let deadline = expires_at(tx, tenant, &metric, now).await?;
                    // A commit produces a debit its own key can reverse, so it
                    // records what it kept and the attribution it was
                    // authorized under. A release reverses nothing.
                    let authorized = (operation == OperationType::Commit)
                        .then(|| attribution_of(&lease.attribution_hash))
                        .transpose()?;
                    write_record(
                        tx,
                        scope,
                        &RecordWrite {
                            write: &write_owned,
                            blob: RecordBlob::Decision(&decision),
                            entries: (operation == OperationType::Commit).then_some(&entries[..]),
                            authorized,
                            policy: None,
                            expires_at: deadline,
                            now,
                        },
                    )
                    .await?;
                    let mut result = MutationResult {
                        counters: counters_of(&entries),
                        threshold_crossings: Vec::new(),
                        event_ids: Vec::new(),
                    };
                    if !events.is_empty() {
                        enqueuer.enqueue_all(tx, &events).await?;
                        result.event_ids = events.iter().map(|event| event.event_id).collect();
                    }
                    Ok::<_, TxError>(TransitionOutcome::Applied(AppliedMutation {
                        decision,
                        mutation: result,
                        expires_at: deadline,
                    }))
                    // @cpt-end:cpt-cf-quota-enforcement-flow-lease-release:p1:inst-lrl-apply
                    // @cpt-end:cpt-cf-quota-enforcement-flow-lease-commit:p1:inst-lcm-apply
                })
            })
        })
        .await
        .map_err(|error| lift(op_name, error))
    }

    /// Transition expired leases and give back what they still hold.
    ///
    /// # Errors
    ///
    /// As the contract documents for `reclaim_expired_leases`.
    pub async fn reclaim_expired_leases_impl(
        &self,
        batch_size: u32,
        before: OffsetDateTime,
    ) -> Result<Vec<ExpiredLease>, StorageError> {
        const OPERATION: &str = "reclaim expired leases";
        let enqueuer = Arc::clone(&self.enqueuer);
        self.db
            .transaction_ref_mapped(move |tx| {
                Box::pin(async move {
                    // The sweeper runs outside any tenant's request, so it
                    // works unscoped, like the other reclamation paths.
                    let scope = AccessScope::allow_all();
                    // @cpt-begin:cpt-cf-quota-enforcement-algo-lease-sweep:p1:inst-swp-reclaim
                    let leases =
                        lease_repo::expired_for_update(tx, &scope, batch_size, before).await?;
                    let mut reclaimed = Vec::with_capacity(leases.len());
                    for lease in leases {
                        let token = LeaseToken::from(lease.token);
                        let holds = lease_repo::holds_of(tx, &scope, lease.token).await?;
                        let capacity = lease_repo::lock_capacity_row(
                            tx,
                            &scope,
                            lease.tenant_id,
                            &lease.metric,
                            RowWait::Wait,
                        )
                        .await?;
                        let mut returned = Vec::with_capacity(holds.len());
                        for hold in &holds {
                            let amount = u64::try_from(hold.held_amount).unwrap_or(0);
                            // Only what nobody has given back: a writer that met
                            // this hold first already returned it.
                            if lease_repo::mark_hold_returned(
                                tx,
                                &scope,
                                lease.token,
                                hold.quota_id,
                                before,
                            )
                            .await?
                                && amount > 0
                            {
                                // The sweeper is outside I8: it has no budget,
                                // so it queues rather than giving up on a row.
                                credit_counter(
                                    tx,
                                    &scope,
                                    hold.quota_id,
                                    hold.period_id,
                                    amount,
                                    before,
                                    RowWait::Wait,
                                )
                                .await?;
                            }
                            returned.push(LeaseHold {
                                quota_id: QuotaId::from(hold.quota_id),
                                held_amount: amount,
                                period_id: hold.period_id.map(Into::into),
                            });
                        }
                        // @cpt-begin:cpt-cf-quota-enforcement-state-lease:p1:inst-lst-autorelease
                        if !lease_repo::mark_state(
                            tx,
                            &scope,
                            lease.token,
                            lease_repo::STATE_AUTO_RELEASED,
                            before,
                        )
                        .await?
                        {
                            // Another sweep body resolved it between the lock
                            // and here; leave it to whoever did.
                            continue;
                        }
                        // @cpt-end:cpt-cf-quota-enforcement-state-lease:p1:inst-lst-autorelease
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
                                before,
                            )
                            .await?;
                        }
                        operation_log_repo::append(
                            tx,
                            &scope,
                            Entry {
                                tenant_id: lease.tenant_id,
                                quota_id: returned
                                    .first()
                                    .map_or(lease.token, |hold| hold.quota_id.as_uuid()),
                                operation: OP_LEASE_AUTO_RELEASE,
                                actor: &crate::domain::ports::Actor {
                                    subject_id: lease.tenant_id,
                                    subject_type: None,
                                },
                                record_version: 1,
                                detail: String::new(),
                                occurred_at: before,
                            },
                        )
                        .await?;
                        // The sweeper is the canonical emission point for this
                        // event, whoever returned the capacity (I11).
                        // @cpt-begin:cpt-cf-quota-enforcement-algo-lease-sweep:p1:inst-swp-emit
                        let held: u64 = returned.iter().map(|hold| hold.held_amount).sum();
                        let event = NotificationEvent {
                            event_id: quota_enforcement_sdk::EventId::generate(),
                            kind: NotificationEventKind::LeaseAutoReleased,
                            scope: NotificationScope::Tenant {
                                tenant_id: TenantId::from(lease.tenant_id),
                            },
                            quota_id: returned.first().map(|hold| hold.quota_id),
                            policy_id: None,
                            subject: None,
                            payload: serde_json::json!({
                                "lease_token": token,
                                "held_amount": held,
                                "affected_quotas": returned
                                    .iter()
                                    .map(|hold| hold.quota_id)
                                    .collect::<Vec<_>>(),
                                "expired_at": lease.expiry_at,
                            }),
                            emitted_at: before,
                        };
                        enqueuer
                            .enqueue_all(tx, std::slice::from_ref(&event))
                            .await?;
                        // @cpt-end:cpt-cf-quota-enforcement-algo-lease-sweep:p1:inst-swp-emit
                        reclaimed.push(ExpiredLease {
                            token,
                            tenant_id: TenantId::from(lease.tenant_id),
                            subject_key: subject_key_of(&lease.subject_key)
                                .map_err(TxError::Storage)?,
                            holds: returned,
                            expired_at: lease.expiry_at,
                        });
                    }
                    Ok::<_, TxError>(reclaimed)
                    // @cpt-end:cpt-cf-quota-enforcement-algo-lease-sweep:p1:inst-swp-reclaim
                })
            })
            .await
            .map_err(|error| lift(OPERATION, error))
    }

    /// The backlog behind the `lease_unreclaimed_expired` gauge.
    ///
    /// # Errors
    ///
    /// `Unavailable` when the backend cannot answer.
    pub async fn count_expired_unreclaimed_leases_impl(
        &self,
        before: OffsetDateTime,
    ) -> Result<Vec<(MetricId, u64)>, StorageError> {
        const OPERATION: &str = "count unreclaimed leases";
        let scope = AccessScope::allow_all();
        let conn = self
            .db
            .conn()
            .map_err(|error| lift(OPERATION, TxError::Db(error)))?;
        let rows = lease_repo::expired_by_metric(&conn, &scope, before)
            .await
            .map_err(|error| lift(OPERATION, error.into()))?;
        rows.into_iter()
            .map(|(metric, count)| {
                MetricId::parse(&metric)
                    .map(|metric| (metric, count))
                    .map_err(|error| {
                        lift(
                            OPERATION,
                            TxError::Storage(StorageError::Internal(error.to_string())),
                        )
                    })
            })
            .collect()
    }

    /// The `(tenant, metric)` cap, most specific configured row first.
    async fn lease_cap(tx: &impl DBRunner, tenant: TenantId, metric: &str) -> Result<u64, TxError> {
        let configured = super::repo::config_repo::read_lease_capacity(
            tx,
            &tenant.as_uuid().to_string(),
            metric,
        )
        .await?;
        Ok(u64::try_from(configured.unwrap_or(1000)).unwrap_or(1000))
    }

    /// Apply an acquisition's plan: the same counter movement a debit makes,
    /// returning what each Quota now holds.
    async fn hold_plan(
        tx: &impl DBRunner,
        scope: &AccessScope,
        quotas: &[Quota],
        decision: &Decision,
        now: OffsetDateTime,
        enqueuer: &Arc<dyn NotificationEnqueuer>,
    ) -> Result<Vec<AppliedEntry>, TxError> {
        let mut entries = Vec::with_capacity(decision.debit_plan.len());
        for (quota_id, planned) in &decision.debit_plan {
            let Some(quota) = quotas.iter().find(|quota| quota.id == *quota_id) else {
                return Err(TxError::Storage(StorageError::QuotaNotFound {
                    id: *quota_id,
                }));
            };
            settle_elapsed_rows(tx, scope, quota, now, enqueuer, RowWait::Nowait).await?;
            let (entry, _) = debit_counter(
                tx,
                scope,
                quota,
                planned.amount,
                now,
                enqueuer,
                RowWait::Nowait,
            )
            .await?;
            entries.push(entry);
        }
        Ok(entries)
    }
}

/// The counter value of one row, for a settlement's mutation result.
async fn counter_value_of(
    tx: &impl DBRunner,
    scope: &AccessScope,
    quota_id: Uuid,
    period_id: Option<Uuid>,
) -> Result<u64, TxError> {
    let value = match period_id {
        Some(period_id) => super::repo::consumption_counter_repo::find_by_period_id_for_update(
            tx,
            scope,
            period_id,
            RowWait::Nowait,
        )
        .await?
        .map_or(0, |row| row.consumed),
        None => super::repo::allocation_counter_repo::read_in_flight_for_update(
            tx,
            scope,
            quota_id,
            RowWait::Nowait,
        )
        .await?
        .unwrap_or(0),
    };
    Ok(u64::try_from(value).unwrap_or(0))
}

/// Read a stored digest back into its domain type.
fn subject_key_of(bytes: &[u8]) -> Result<IdempotencySubjectKey, StorageError> {
    <[u8; 32]>::try_from(bytes)
        .map(IdempotencySubjectKey::from_bytes)
        .map_err(|_| StorageError::Internal("a stored subject key is not 32 bytes".to_owned()))
}

fn attribution_of(bytes: &[u8]) -> Result<AttributionDigest, TxError> {
    <[u8; 32]>::try_from(bytes)
        .map(AttributionDigest::from_bytes)
        .map_err(|_| {
            TxError::Storage(StorageError::Internal(
                "a stored attribution digest is not 32 bytes".to_owned(),
            ))
        })
}

/// How a lease is settled: a commit keeps `actual` of the reserved amount
/// (all of it when absent), a release keeps nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Settlement {
    /// Convert the lease into a debit of `actual`.
    Commit {
        /// What was used; absent keeps the whole reservation.
        actual: Option<u64>,
    },
    /// Return every hold.
    Release,
}

impl Settlement {
    fn operation(self) -> OperationType {
        match self {
            Self::Commit { .. } => OperationType::Commit,
            Self::Release => OperationType::Release,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Commit { .. } => "commit lease",
            Self::Release => "release lease",
        }
    }

    fn log_verb(self) -> &'static str {
        match self {
            Self::Commit { .. } => OP_LEASE_COMMIT,
            Self::Release => OP_LEASE_RELEASE,
        }
    }

    fn state(self) -> &'static str {
        match self {
            Self::Commit { .. } => lease_repo::STATE_COMMITTED,
            Self::Release => lease_repo::STATE_RELEASED,
        }
    }

    /// What this settlement keeps of `reserved`. A release keeps nothing,
    /// which the apportionment reads as a commit of zero.
    fn kept(self, reserved: u64) -> u64 {
        match self {
            Self::Commit { actual } => actual.unwrap_or(reserved),
            Self::Release => 0,
        }
    }
}

/// Split `actual` across a lease's holds by the conserving apportionment of
/// DESIGN I5.
///
/// # Errors
///
/// `OverCommitNotAuthorized` above the reserved amount; `Internal` on an
/// arithmetic overflow no bounded plan can reach.
fn kept_of(held: &[u64], reserved: u64, actual: u64) -> Result<Vec<u64>, TxError> {
    let Some(reserved) = std::num::NonZeroU64::new(reserved) else {
        // An acquisition never reserves zero; returning everything is the
        // safe reading of "kept nothing".
        return Ok(vec![0; held.len()]);
    };
    apportion(held, actual, reserved).map_err(|error| {
        TxError::Storage(match error {
            quota_enforcement_sdk::ApportionError::OverCommit => {
                StorageError::OverCommitNotAuthorized {
                    reserved: reserved.get(),
                    actual,
                }
            }
            other @ quota_enforcement_sdk::ApportionError::Overflow => {
                StorageError::Internal(other.to_string())
            }
        })
    })
}

fn unavailable_arbiter(operation: &'static str) -> StorageError {
    super::consumption_store::unavailable(
        operation,
        "idempotency arbiter",
        &"the key is being written concurrently",
    )
}

#[async_trait::async_trait]
impl crate::domain::ports::LeaseStore for SqlConsumptionStore {
    async fn acquire_lease(
        &self,
        ctx: &SecurityContext,
        scope: &AccessScope,
        mutation: &EvaluatedMutation<'_>,
        ttl: Duration,
    ) -> Result<TransitionOutcome<EvaluatedLease>, StorageError> {
        self.acquire_lease_impl(ctx, scope, mutation, ttl).await
    }

    async fn commit_lease(
        &self,
        ctx: &SecurityContext,
        scope: &AccessScope,
        token: LeaseToken,
        actual_amount: Option<u64>,
        idempotency: &PartialIdempotencyWrite,
        events: &[NotificationEvent],
    ) -> Result<TransitionOutcome<AppliedMutation>, StorageError> {
        self.settle_lease_impl(
            ctx,
            scope,
            token,
            Settlement::Commit {
                actual: actual_amount,
            },
            idempotency,
            events,
        )
        .await
    }

    async fn release_lease(
        &self,
        ctx: &SecurityContext,
        scope: &AccessScope,
        token: LeaseToken,
        idempotency: &PartialIdempotencyWrite,
        events: &[NotificationEvent],
    ) -> Result<TransitionOutcome<AppliedMutation>, StorageError> {
        self.settle_lease_impl(ctx, scope, token, Settlement::Release, idempotency, events)
            .await
    }

    async fn reclaim_expired_leases(
        &self,
        batch_size: u32,
        before: OffsetDateTime,
    ) -> Result<Vec<ExpiredLease>, StorageError> {
        self.reclaim_expired_leases_impl(batch_size, before).await
    }

    async fn count_expired_unreclaimed_leases(
        &self,
        before: OffsetDateTime,
    ) -> Result<Vec<(MetricId, u64)>, StorageError> {
        self.count_expired_unreclaimed_leases_impl(before).await
    }
}
