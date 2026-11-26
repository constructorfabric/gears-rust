//! The lease primitives of the storage contract, on the same rows and the same
//! transaction machinery as the consumption store.
//!
//! # What a lease adds to a debit
//!
//! An acquisition *is* a debit that can be taken back: it evaluates the policy
//! under the Quota locks and applies the plan exactly as
//! [`super::consumption_store`] does, then records a lease and one hold per
//! plan entry so a later commit or release can settle it. Everything about the
//! evaluation, the idempotency record and the threshold routine is shared, not
//! restated. A commit writes the charge of what it keeps, which is its record.
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
//! Quota rows): Quota rows ascending by id, then key locks (the stripes of
//! idempotency scopes), then lease rows ascending by token, then capacity rows
//! (acquisitions only), then counter rows (the waiting paths ascending by
//! `(quota_id, period_id)`), then the hold rows under their counter, then
//! inserts. A primitive may skip a rank and never goes back; anything it must
//! know before locking — a lease's
//! metric and holds, both immutable once written — is read unscoped of rank
//! first and re-read under the lock.
//!
//! Acquire, commit and release carry the contention budget (I8): every lock they
//! take is `NOWAIT`, and a refused one rolls the transaction back and retries it
//! until the budget is spent ([`super::locking`]). The sweeper has no budget and
//! queues.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use quota_enforcement_sdk::{
    Actor, AppliedMutation, AttributionDigest, Decision, DecisionResult, EvaluatedLease,
    EvaluatedMutation, ExpiredLease, HeldEntry, LeaseHold, LeaseToken, MetricId, MutationResult,
    NO_APPLICABLE_QUOTA, NotificationEvent, NotificationEventKind, NotificationScope,
    OperationType, PartialIdempotencyWrite, Quota, QuotaId, StorageError, TenantId,
    TransitionOutcome, plan_entries, plan_of, unwind,
};
use time::OffsetDateTime;
use toolkit_db::secure::DBRunner;
use toolkit_security::AccessScope;
use uuid::Uuid;

use super::consumption_store::{
    AppliedEntry, ChargeWrite, OwnedMutation, RACE_ATTEMPTS, RecordBlob, RecordWrite, Replay,
    SqlConsumptionStore, TxError, counters_of, credit_counter, debit_counter, decision_of, lift,
    replay_of, settle_elapsed_rows, write_charge, write_record,
};
use super::entity::lease_hold;
use super::locking::{ContentionBudget, lock_scopes, with_budget};
use super::repo::lease_repo;
use super::repo::lease_repo::RowWait;
use super::repo::operation_log_repo::{self, Entry};
use crate::infra::outbox::{AttemptWakes, NotificationEnqueuer, settling};

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
        actor: &Actor,
        scope: &AccessScope,
        mutation: &EvaluatedMutation<'_>,
        ttl: Duration,
    ) -> Result<TransitionOutcome<EvaluatedLease>, StorageError> {
        const OPERATION: &str = "acquire lease";
        let actor = actor.clone();
        let tenant = mutation.applicable.tenant_id;
        let metric = mutation.applicable.metric.clone();
        let config = self.config().await?;
        let budget = ContentionBudget::of(&config, [metric.as_str()]);
        let cap = config.lease_cap(tenant, metric.as_str());
        let retention = config.idempotency_retention(tenant, metric.as_str());
        for attempt in 0..RACE_ATTEMPTS {
            // A refused lock rolls this transaction back and runs it
            // again under the budget; a race goes to the arbiter below.
            let result = with_budget(budget, || {
                let wakes = AttemptWakes::begin(&self.enqueuer);
                let enqueuer = wakes.enqueuer();
                let clock = Arc::clone(&self.clock);
                let actor = actor.clone();
                let scope = scope.clone();
                let metric = metric.clone();
                let owned = OwnedMutation::of(mutation);
                settling(
                    wakes,
                    self.db.transaction_ref_mapped(move |tx| {
                        Box::pin(async move {
                            let (scope, owned) = (&scope, &owned);
                            // Rank 1: the Quota rows of the applicable set.
                            let quotas = SqlConsumptionStore::lock_applicable(
                                tx,
                                scope,
                                owned,
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
                            lock_scopes::<TxError>(tx, &[&owned.idempotency.scope]).await?;
                            if let Replay::Stored(row) =
                                replay_of(tx, scope, &owned.idempotency, now, Some(RowWait::Nowait))
                                    .await?
                            {
                                let acquired: EvaluatedLease = serde_json::from_str(&row.blob)
                                    .map_err(|error| {
                                        TxError::Storage(StorageError::Internal(error.to_string()))
                                    })?;
                                return Ok(TransitionOutcome::NoOp(acquired));
                            }
                            let (policy, decision) =
                                SqlConsumptionStore::evaluate(tx, scope, &quotas, owned, now)
                                    .await?;
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
                                lease_repo::lock_capacity_row(
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
                                if live >= cap {
                                    return Err(TxError::Storage(
                                        StorageError::LeaseInflightLimitExceeded,
                                    ));
                                }
                                // @cpt-end:cpt-cf-quota-enforcement-flow-lease-acquire:p1:inst-lac-cap
                                // @cpt-end:cpt-cf-quota-enforcement-flow-lease-acquire:p1:inst-lac-cap-if
                                // @cpt-begin:cpt-cf-quota-enforcement-flow-lease-acquire:p1:inst-lac-insert
                                let applied = SqlConsumptionStore::hold_plan(
                                    tx, scope, &actor, &quotas, &decision, now, &enqueuer,
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
                                        attribution_digest: owned.authorized.as_bytes(),
                                        idem_key: &owned.idempotency.scope.key,
                                        reserved_amount: i64::try_from(owned.amount).map_err(
                                            |_| {
                                                TxError::Storage(StorageError::Internal(
                                                    "reserved amount does not fit the column"
                                                        .to_owned(),
                                                ))
                                            },
                                        )?,
                                        now,
                                        expiry_at: expiry,
                                    },
                                )
                                .await?;
                                for entry in &applied {
                                    lease_repo::insert_hold(
                                        tx,
                                        scope,
                                        hold_row(token, tenant, entry)?,
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
                            let deadline = now + retention;
                            // The blob is the whole outcome, not only the decision,
                            // so a replay returns the token this call issued.
                            write_record(
                                tx,
                                scope,
                                &RecordWrite {
                                    write: &owned.idempotency,
                                    blob: RecordBlob::Verbatim(&blob),
                                    policy: Some(&policy),
                                    expires_at: deadline,
                                    now,
                                },
                            )
                            .await?;
                            Ok::<_, TxError>(TransitionOutcome::Applied(acquired))
                        })
                    }),
                )
            })
            .await;
            match result {
                Ok(outcome) => return Ok(outcome),
                Err(TxError::Raced(lost)) => {
                    if let Some(record) = self.winner_of(&lost).await? {
                        let acquired: EvaluatedLease = serde_json::from_str(&record.blob)
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

    /// Settle a lease: commit unwinds the unused part from the holds, release
    /// keeps none.
    ///
    /// # Errors
    ///
    /// As the contract documents for `commit_lease` and `release_lease`.
    pub async fn settle_lease_impl(
        &self,
        actor: &Actor,
        scope: &AccessScope,
        token: LeaseToken,
        settlement: Settlement,
        idempotency: &PartialIdempotencyWrite,
        events: &[NotificationEvent],
    ) -> Result<TransitionOutcome<AppliedMutation>, StorageError> {
        let op_name = settlement.name();
        let operation = settlement.operation();
        let actor = actor.clone();
        // Rank-free: the metric names the contention budget, which has to start
        // before the first lock. It is immutable once written.
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
        let write = idempotency.clone().complete(operation);
        let config = self.config().await?;
        let budget = ContentionBudget::of(&config, [preview.metric.as_str()]);
        let tenant = idempotency.tenant_id;
        let retention = config.idempotency_retention(tenant, &preview.metric);
        // A refused lock rolls this transaction back and runs it again under
        // the budget.
        with_budget(budget, || {
            let wakes = AttemptWakes::begin(&self.enqueuer);
            let enqueuer = wakes.enqueuer();
            let clock = Arc::clone(&self.clock);
            let grant = scope.clone();
            let events = events.to_vec();
            let write_owned = write.clone();
            let metric = preview.metric.clone();
            let settlement = settlement.clone();
            let actor = actor.clone();
            settling(
                wakes,
                self.db.transaction_ref_mapped(move |tx| {
                    Box::pin(async move {
                        // The caller's grant admits the lease row and nothing
                        // else. A grant for one lease names it by token, and
                        // only `qe_leases` keys its resource by token: on a hold
                        // or a counter the resource is a Quota, and a table
                        // without one drops the constraint and denies
                        // everything. So the rows this lease owns (its holds,
                        // their counters, the record and the log) are reached
                        // under its tenant, which the preview matched, and the
                        // grant still decides which lease that is.
                        let scope = &AccessScope::for_tenant(tenant.as_uuid());
                        let now = clock();
                        // Rank 2 first: the scope's stripe (I8), then the record,
                        // so a settlement that already succeeded replays even
                        // though its lease is no longer active.
                        // @cpt-begin:cpt-cf-quota-enforcement-flow-lease-commit:p1:inst-lcm-idem
                        // @cpt-begin:cpt-cf-quota-enforcement-flow-lease-release:p1:inst-lrl-idem
                        lock_scopes::<TxError>(tx, &[&write_owned.scope]).await?;
                        if let Replay::Stored(row) =
                            replay_of(tx, scope, &write_owned, now, Some(RowWait::Nowait)).await?
                        {
                            return Ok(TransitionOutcome::NoOp(AppliedMutation {
                                decision: decision_of(&row.blob)?,
                                mutation: MutationResult::default(),
                            }));
                        }
                        // @cpt-end:cpt-cf-quota-enforcement-flow-lease-release:p1:inst-lrl-idem
                        // @cpt-end:cpt-cf-quota-enforcement-flow-lease-commit:p1:inst-lcm-idem
                        // Rank 3.
                        // @cpt-begin:cpt-cf-quota-enforcement-flow-lease-commit:p1:inst-lcm-lock
                        let lease = lease_repo::find_for_update(
                            tx,
                            &grant,
                            token.as_uuid(),
                            RowWait::Nowait,
                        )
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
                        let reserved = u64::try_from(lease.reserved_amount).unwrap_or(0);
                        // @cpt-begin:cpt-cf-quota-enforcement-flow-lease-commit:p1:inst-lcm-metric
                        let actual = settlement.kept(reserved, &lease.metric)?;
                        // @cpt-end:cpt-cf-quota-enforcement-flow-lease-commit:p1:inst-lcm-metric
                        // @cpt-begin:cpt-cf-quota-enforcement-flow-lease-commit:p1:inst-lcm-overcommit-if
                        // @cpt-begin:cpt-cf-quota-enforcement-flow-lease-commit:p1:inst-lcm-overcommit
                        let returns = returns_of(&holds, reserved, actual)?;
                        // @cpt-end:cpt-cf-quota-enforcement-flow-lease-commit:p1:inst-lcm-overcommit
                        // @cpt-end:cpt-cf-quota-enforcement-flow-lease-commit:p1:inst-lcm-overcommit-if
                        // @cpt-begin:cpt-cf-quota-enforcement-flow-lease-commit:p1:inst-lcm-apply
                        // @cpt-begin:cpt-cf-quota-enforcement-flow-lease-release:p1:inst-lrl-apply
                        // Rank 5 and 5b, ascending by `quota_id` as `holds_of`
                        // returns them.
                        let mut entries = Vec::new();
                        for (hold, returned) in holds.iter().zip(returns) {
                            let kept = u64::try_from(hold.held_amount).unwrap_or(0) - returned;
                            // Rank 5 before 5b: the counter row is taken first, and
                            // only then is its hold stamped. Stamping under a row
                            // this transaction does not hold would let a sweeper
                            // returning the same hold interleave between the two.
                            let value = counter_value_of(
                                tx,
                                scope,
                                hold.quota_id,
                                hold.period_id,
                                RowWait::Nowait,
                            )
                            .await?;
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
                            // Already stamped: a writer on a later clock expired this lease.
                            if !owed {
                                return Err(StorageError::LeaseNotActive { token }.into());
                            }
                            // @cpt-begin:cpt-cf-quota-enforcement-flow-lease-commit:p1:inst-lcm-boundary
                            let value = if returned > 0 {
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
                            if kept > 0 {
                                entries.push(AppliedEntry {
                                    quota_id: hold.quota_id,
                                    period_id: hold.period_id,
                                    amount: kept,
                                    value,
                                    chain: u32::try_from(hold.chain).unwrap_or(0),
                                    position: u32::try_from(hold.position).unwrap_or(0),
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
                        if !lease_repo::mark_state(tx, &grant, token.as_uuid(), state, now).await? {
                            return Err(TxError::Storage(StorageError::LeaseNotActive { token }));
                        }
                        // @cpt-end:cpt-cf-quota-enforcement-state-lease:p1:inst-lst-release
                        // @cpt-end:cpt-cf-quota-enforcement-state-lease:p1:inst-lst-commit
                        // Settling evaluates nothing: the plan was fixed at
                        // acquisition, so the recorded decision is what it kept.
                        let decision = Decision::allowed_with_plan(plan_of(
                            entries.iter().map(AppliedEntry::planned),
                        ));
                        let deadline = now + retention;
                        if operation == OperationType::Commit {
                            write_charge(
                                tx,
                                scope,
                                &ChargeWrite {
                                    write: &write_owned,
                                    decision: &decision,
                                    policy: None,
                                    authorized: attribution_of(&lease.attribution_digest)?,
                                    metric: &metric,
                                    amount: actual,
                                    entries: &entries,
                                    return_window_end: deadline,
                                    now,
                                },
                            )
                            .await?;
                        } else {
                            write_record(
                                tx,
                                scope,
                                &RecordWrite {
                                    write: &write_owned,
                                    blob: RecordBlob::Decision(&decision),
                                    policy: None,
                                    expires_at: deadline,
                                    now,
                                },
                            )
                            .await?;
                        }
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
                        }))
                        // @cpt-end:cpt-cf-quota-enforcement-flow-lease-release:p1:inst-lrl-apply
                        // @cpt-end:cpt-cf-quota-enforcement-flow-lease-commit:p1:inst-lcm-apply
                    })
                }),
            )
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
        let wakes = AttemptWakes::begin(&self.enqueuer);
        let enqueuer = wakes.enqueuer();
        settling(
            wakes,
            self.db.transaction_ref_mapped(move |tx| {
                Box::pin(async move {
                    // The sweeper runs outside any tenant's request, so it
                    // works unscoped, like the other reclamation paths.
                    let scope = AccessScope::allow_all();
                    // @cpt-begin:cpt-cf-quota-enforcement-algo-lease-sweep:p1:inst-swp-reclaim
                    let leases =
                        lease_repo::expired_for_update(tx, &scope, batch_size, before).await?;
                    let mut held = Vec::with_capacity(leases.len());
                    for lease in leases {
                        let holds = lease_repo::holds_of(tx, &scope, lease.token).await?;
                        held.push((lease, holds));
                    }
                    lock_hold_counters(
                        tx,
                        &scope,
                        held.iter().flat_map(|(_, holds)| holds),
                        RowWait::Wait,
                    )
                    .await?;
                    let mut reclaimed = Vec::with_capacity(held.len());
                    for (lease, holds) in held {
                        let token = LeaseToken::from(lease.token);
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
                        operation_log_repo::append(
                            tx,
                            &scope,
                            Entry {
                                tenant_id: lease.tenant_id,
                                quota_id: returned
                                    .first()
                                    .map_or(lease.token, |hold| hold.quota_id.as_uuid()),
                                operation: OP_LEASE_AUTO_RELEASE,
                                actor: &Actor::Task {
                                    task: quota_enforcement_sdk::BackgroundTask::LeaseSweeper,
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
                            actor: Actor::Task {
                                task: quota_enforcement_sdk::BackgroundTask::LeaseSweeper,
                            },
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
                            holds: returned,
                            expired_at: lease.expiry_at,
                        });
                    }
                    Ok::<_, TxError>(reclaimed)
                    // @cpt-end:cpt-cf-quota-enforcement-algo-lease-sweep:p1:inst-swp-reclaim
                })
            }),
        )
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

    /// Apply an acquisition's plan: the same counter movement a debit makes, in
    /// ascending `quota_id` order (ADR-0002), returning what each entry holds.
    async fn hold_plan(
        tx: &impl DBRunner,
        scope: &AccessScope,
        actor: &Actor,
        quotas: &[Quota],
        decision: &Decision,
        now: OffsetDateTime,
        enqueuer: &Arc<dyn NotificationEnqueuer>,
    ) -> Result<Vec<AppliedEntry>, TxError> {
        let mut planned: Vec<_> = plan_entries(&decision.debit_plan).collect();
        planned.sort_by_key(|(_, plan)| plan.quota_id);
        let mut entries = Vec::with_capacity(planned.len());
        for ((chain, position), planned) in planned {
            let Some(quota) = quotas.iter().find(|quota| quota.id == planned.quota_id) else {
                return Err(TxError::Storage(StorageError::QuotaNotFound {
                    id: planned.quota_id,
                }));
            };
            settle_elapsed_rows(tx, scope, actor, quota, now, enqueuer, RowWait::Nowait).await?;
            let (entry, _) = debit_counter(
                tx,
                scope,
                actor,
                quota,
                planned.amount,
                now,
                enqueuer,
                RowWait::Nowait,
            )
            .await?;
            entries.push(AppliedEntry {
                chain,
                position,
                ..entry
            });
        }
        Ok(entries)
    }
}

/// Rank 5 for a whole waiting transaction: each counter under `holds` once, ascending, so two never cross.
pub(super) async fn lock_hold_counters<'a>(
    tx: &impl DBRunner,
    scope: &AccessScope,
    holds: impl IntoIterator<Item = &'a lease_hold::Model>,
    wait: RowWait,
) -> Result<(), TxError> {
    let counters: BTreeSet<(Uuid, Option<Uuid>)> = holds
        .into_iter()
        .map(|hold| (hold.quota_id, hold.period_id))
        .collect();
    for (quota_id, period_id) in counters {
        counter_value_of(tx, scope, quota_id, period_id, wait).await?;
    }
    Ok(())
}

/// The counter value of one row, read under its rank-5 lock, which a hold stamp must follow.
pub(super) async fn counter_value_of(
    tx: &impl DBRunner,
    scope: &AccessScope,
    quota_id: Uuid,
    period_id: Option<Uuid>,
    wait: RowWait,
) -> Result<u64, TxError> {
    let missing = |what: &str| {
        TxError::Storage(StorageError::Internal(format!(
            "the {what} of a hold on quota {quota_id} is missing"
        )))
    };
    let value = match period_id {
        Some(period_id) => {
            super::repo::consumption_counter_repo::find_by_period_id_for_update(
                tx, scope, period_id, wait,
            )
            .await?
            .ok_or_else(|| missing("period row"))?
            .consumed
        }
        None => super::repo::allocation_counter_repo::read_in_flight_for_update(
            tx, scope, quota_id, wait,
        )
        .await?
        .ok_or_else(|| missing("allocation counter"))?,
    };
    Ok(u64::try_from(value).unwrap_or(0))
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
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Settlement {
    /// Convert the lease into a debit of `actual`.
    Commit {
        /// What was used; absent keeps the whole reservation.
        actual: Option<u64>,
        /// The metric type the request names; it must be the lease's.
        metric: Option<String>,
    },
    /// Return every hold.
    Release,
}

impl Settlement {
    fn operation(&self) -> OperationType {
        match self {
            Self::Commit { .. } => OperationType::Commit,
            Self::Release => OperationType::Release,
        }
    }

    fn name(&self) -> &'static str {
        match self {
            Self::Commit { .. } => "commit lease",
            Self::Release => "release lease",
        }
    }

    fn log_verb(&self) -> &'static str {
        match self {
            Self::Commit { .. } => OP_LEASE_COMMIT,
            Self::Release => OP_LEASE_RELEASE,
        }
    }

    fn state(&self) -> &'static str {
        match self {
            Self::Commit { .. } => lease_repo::STATE_COMMITTED,
            Self::Release => lease_repo::STATE_RELEASED,
        }
    }

    /// What this settlement keeps of `reserved` on a lease of the metric
    /// type `leased`. A release keeps nothing, which the unwinding reads as a
    /// commit of zero; a commit naming another metric type is
    /// `MetricTypeMismatch`.
    fn kept(&self, reserved: u64, leased: &str) -> Result<u64, TxError> {
        match self {
            Self::Commit {
                metric: Some(requested),
                ..
            } if requested != leased => Err(TxError::Storage(StorageError::MetricTypeMismatch {
                requested: requested.clone(),
                charged: leased.to_owned(),
            })),
            Self::Commit { actual, .. } => Ok(actual.unwrap_or(reserved)),
            Self::Release => Ok(0),
        }
    }
}

/// The hold row of one applied plan entry of the lease `token`.
fn hold_row(
    token: LeaseToken,
    tenant: TenantId,
    entry: &AppliedEntry,
) -> Result<lease_hold::Model, TxError> {
    let column = |what: &str| {
        TxError::Storage(StorageError::Internal(format!(
            "{what} does not fit the column"
        )))
    };
    Ok(lease_hold::Model {
        lease_token: token.as_uuid(),
        quota_id: entry.quota_id,
        tenant_id: tenant.as_uuid(),
        held_amount: i64::try_from(entry.amount).map_err(|_| column("held amount"))?,
        period_id: entry.period_id,
        chain: i32::try_from(entry.chain).map_err(|_| column("chain"))?,
        position: i32::try_from(entry.position).map_err(|_| column("position"))?,
        returned_at: None,
    })
}

/// What each hold gives back when the lease keeps `actual` of `reserved`: the
/// unused part unwound from the holds (ADR-0010).
///
/// # Errors
///
/// `OverCommitNotAuthorized` above the reserved amount; `Internal` when a
/// chain of the holds does not add up to the reserved amount.
fn returns_of(
    holds: &[lease_hold::Model],
    reserved: u64,
    actual: u64,
) -> Result<Vec<u64>, TxError> {
    let Some(unused) = reserved.checked_sub(actual) else {
        return Err(TxError::Storage(StorageError::OverCommitNotAuthorized {
            reserved,
            actual,
        }));
    };
    let held: Vec<HeldEntry> = holds
        .iter()
        .map(|hold| HeldEntry {
            chain: u32::try_from(hold.chain).unwrap_or(0),
            position: u32::try_from(hold.position).unwrap_or(0),
            held: u64::try_from(hold.held_amount).unwrap_or(0),
        })
        .collect();
    unwind(&held, unused)
        .map_err(|error| TxError::Storage(StorageError::Internal(error.to_string())))
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
        actor: &Actor,
        scope: &AccessScope,
        mutation: &EvaluatedMutation<'_>,
        ttl: Duration,
    ) -> Result<TransitionOutcome<EvaluatedLease>, StorageError> {
        self.acquire_lease_impl(actor, scope, mutation, ttl).await
    }

    async fn commit_lease(
        &self,
        actor: &Actor,
        scope: &AccessScope,
        token: LeaseToken,
        actual_amount: Option<u64>,
        metric: Option<&MetricId>,
        idempotency: &PartialIdempotencyWrite,
        events: &[NotificationEvent],
    ) -> Result<TransitionOutcome<AppliedMutation>, StorageError> {
        self.settle_lease_impl(
            actor,
            scope,
            token,
            Settlement::Commit {
                actual: actual_amount,
                metric: metric.map(|metric| metric.as_str().to_owned()),
            },
            idempotency,
            events,
        )
        .await
    }

    async fn release_lease(
        &self,
        actor: &Actor,
        scope: &AccessScope,
        token: LeaseToken,
        idempotency: &PartialIdempotencyWrite,
        events: &[NotificationEvent],
    ) -> Result<TransitionOutcome<AppliedMutation>, StorageError> {
        self.settle_lease_impl(
            actor,
            scope,
            token,
            Settlement::Release,
            idempotency,
            events,
        )
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
