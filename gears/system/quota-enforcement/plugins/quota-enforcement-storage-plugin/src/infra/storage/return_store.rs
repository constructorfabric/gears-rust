//! Return and rollback: giving back a charge (ADR-0011), and the sweeper of
//! deleted charges.
//!
//! # Lock order
//!
//! A return or rollback reads its charge without a lock to learn the Quotas
//! of its entries, which never change. It then locks those Quota rows in
//! ascending order (rank 1), the stripes of the charge's scope and, for a
//! return, of its return key (rank 2, one sorted set), and reads the charge
//! again. A charge under the key that is not the one first read is a
//! replacement of a deleted charge, so the named charge is unknown. The
//! counter rows of the entries follow in ascending `quota_id` order (rank 5),
//! and whether a period is settled is read under its counter lock. Two
//! returns with one key meet on the return key's stripe: the second finds the
//! first one's record.
//!
//! The sweeper takes a charge's stripe `SKIP LOCKED` and deletes only a charge
//! still deleted by the rule, so it never removes rows a return is working on
//! or a charge that a return has since kept.

use std::sync::Arc;

use quota_enforcement_sdk::{
    AppliedMutation, AttributionDigest, ChargeReturn, Decision, HeldAllocationCharges, HeldEntry,
    IdempotencyScope, MetricId, MutationResult, NotificationEvent, NotificationEventKind, QuotaId,
    QuotaType, StorageError, TransitionOutcome, plan_of, unwind,
};
use serde_json::json;
use time::OffsetDateTime;
use toolkit_db::secure::DBRunner;
use toolkit_security::AccessScope;
use uuid::Uuid;

use super::config_cache::ConfigSnapshot;
use super::consumption_store::{
    AppliedEntry, Clock, SqlConsumptionStore, TxError, counters_of, credit_counter, expires_at,
    lift, quota_event, scope_key_of, unavailable,
};
use super::entity::{charge, charge_entry, charge_return};
use super::locking::{ContentionBudget, lock_scopes, with_budget};
use super::quota_mapping;
use super::repo::operation_log_repo::{self, Entry};
use super::repo::{RowWait, charge_repo, consumption_counter_repo as counter_repo};
use super::repo::{idempotency_repo as idem_repo, quota_repo};
use crate::domain::ports::Actor;
use crate::infra::outbox::{AttemptWakes, NotificationEnqueuer, settling};

/// What a give-back is: a return of an amount under its own key, or a
/// rollback of the remainder under the caller's attribution.
#[derive(Clone)]
enum GiveBack {
    Return(ChargeReturn),
    Rollback {
        charge: IdempotencyScope,
        authorized: AttributionDigest,
    },
}

impl GiveBack {
    fn charge(&self) -> &IdempotencyScope {
        match self {
            Self::Return(ret) => &ret.charge,
            Self::Rollback { charge, .. } => charge,
        }
    }

    fn unknown(&self) -> StorageError {
        StorageError::OperationNotFound {
            key: self.charge().key.clone(),
        }
    }
}

/// What the unlocked read of the charge found.
#[derive(Clone, Default)]
struct Discovered {
    charge_id: Option<Uuid>,
    metric: Option<String>,
    quotas: Vec<Uuid>,
}

/// Who a give-back acts as, and what it reads the time from and enqueues into.
struct Env<'a> {
    actor: &'a Actor,
    clock: &'a Clock,
    enqueuer: &'a Arc<dyn NotificationEnqueuer>,
    config: &'a ConfigSnapshot,
}

fn held_of(entries: &[charge_entry::Model]) -> Vec<HeldEntry> {
    entries
        .iter()
        .map(|entry| HeldEntry {
            chain: u32::try_from(entry.chain).unwrap_or(0),
            position: u32::try_from(entry.position).unwrap_or(0),
            held: u64::try_from(entry.charged - entry.returned).unwrap_or(0),
        })
        .collect()
}

/// What remains of a charge: the least any chain still holds.
fn remaining(held: &[HeldEntry]) -> u64 {
    let mut chains = std::collections::BTreeMap::<u32, u64>::new();
    for entry in held {
        *chains.entry(entry.chain).or_default() += entry.held;
    }
    chains.into_values().min().unwrap_or(0)
}

/// What a give-back applied: the plan of every reached entry, the entries
/// whose counters moved, and their Quotas.
#[derive(Default)]
struct Parts {
    plan: Vec<((u32, u32), quota_enforcement_sdk::QuotaDebitPlan)>,
    moved: Vec<AppliedEntry>,
    quotas: Vec<quota_enforcement_sdk::Quota>,
}

/// The charge the locked read found, if it is the one the unlocked read
/// found and the request may name it: a rollback's attribution digest must
/// be the charge's, and a return's metric type the charge's.
fn verified(
    what: &GiveBack,
    named: Option<charge::Model>,
    discovered: &Discovered,
) -> Result<charge::Model, StorageError> {
    // @cpt-begin:cpt-cf-quota-enforcement-flow-rollback:p1:inst-rlb-unknown-if
    // @cpt-begin:cpt-cf-quota-enforcement-flow-rollback:p1:inst-rlb-unknown
    let charge = named
        .filter(|charge| Some(charge.id) == discovered.charge_id)
        .ok_or_else(|| what.unknown())?;
    match what {
        GiveBack::Rollback { authorized, .. }
            if charge.attribution_digest != authorized.as_bytes() =>
        {
            Err(what.unknown())
        }
        // @cpt-end:cpt-cf-quota-enforcement-flow-rollback:p1:inst-rlb-unknown
        // @cpt-end:cpt-cf-quota-enforcement-flow-rollback:p1:inst-rlb-unknown-if
        GiveBack::Return(ret) if charge.metric != ret.metric.as_str() => {
            Err(StorageError::MetricTypeMismatch {
                requested: ret.metric.as_str().to_owned(),
                charged: charge.metric,
            })
        }
        _ => Ok(charge),
    }
}

fn no_op() -> TransitionOutcome<AppliedMutation> {
    TransitionOutcome::NoOp(AppliedMutation {
        decision: Decision::allowed_with_plan(Vec::new()),
        mutation: MutationResult::default(),
    })
}

// @cpt-flow:cpt-cf-quota-enforcement-flow-return:p1
// @cpt-flow:cpt-cf-quota-enforcement-flow-rollback:p1
// @cpt-dod:cpt-cf-quota-enforcement-dod-charge-ledger:p1
impl SqlConsumptionStore {
    pub(super) async fn apply_return_impl(
        &self,
        actor: &Actor,
        scope: &AccessScope,
        ret: &ChargeReturn,
        events: &[NotificationEvent],
    ) -> Result<TransitionOutcome<AppliedMutation>, StorageError> {
        self.give_back(
            "apply return",
            actor,
            scope,
            GiveBack::Return(ret.clone()),
            events,
        )
        .await
    }

    pub(super) async fn apply_rollback_impl(
        &self,
        actor: &Actor,
        scope: &AccessScope,
        target: &quota_enforcement_sdk::RollbackTarget,
        events: &[NotificationEvent],
    ) -> Result<TransitionOutcome<AppliedMutation>, StorageError> {
        self.give_back(
            "apply rollback",
            actor,
            scope,
            GiveBack::Rollback {
                charge: target.original.clone(),
                authorized: target.authorized,
            },
            events,
        )
        .await
    }

    async fn give_back(
        &self,
        operation: &'static str,
        actor: &Actor,
        scope: &AccessScope,
        what: GiveBack,
        events: &[NotificationEvent],
    ) -> Result<TransitionOutcome<AppliedMutation>, StorageError> {
        let discovered = self
            .discover(scope, &what)
            .await
            .map_err(|error| lift(operation, error))?;
        let config = self.config().await?;
        let budget = ContentionBudget::of(&config, discovered.metric.as_deref());
        with_budget(budget, || {
            let wakes = AttemptWakes::begin(&self.enqueuer);
            let enqueuer = wakes.enqueuer();
            let clock = Arc::clone(&self.clock);
            let actor = actor.clone();
            let scope = scope.clone();
            let events = events.to_vec();
            let what = what.clone();
            let discovered = discovered.clone();
            let config = Arc::clone(&config);
            settling(
                wakes,
                self.db.transaction_ref_mapped(move |tx| {
                    Box::pin(async move {
                        let env = Env {
                            actor: &actor,
                            clock: &clock,
                            enqueuer: &enqueuer,
                            config: &config,
                        };
                        Self::give_back_in_tx(tx, &scope, &what, &discovered, &events, &env).await
                    })
                }),
            )
        })
        .await
        .map_err(|error| lift(operation, error))
    }

    /// The live charge `what` names and the Quotas of its entries, read
    /// without a lock. A rollback under another attribution discovers nothing,
    /// as for an unknown charge, so it locks none of the charge's Quotas.
    async fn discover(&self, scope: &AccessScope, what: &GiveBack) -> Result<Discovered, TxError> {
        let conn = self.db.conn().map_err(TxError::Db)?;
        let now = (self.clock)();
        let Some(charge) =
            charge_repo::find(&conn, scope, &scope_key_of(what.charge()), now).await?
        else {
            return Ok(Discovered::default());
        };
        if let GiveBack::Rollback { authorized, .. } = what
            && charge.attribution_digest != authorized.as_bytes()
        {
            return Ok(Discovered::default());
        }
        let quotas = charge_repo::entries_of(&conn, scope, charge.id)
            .await?
            .into_iter()
            .map(|entry| entry.quota_id)
            .collect();
        Ok(Discovered {
            charge_id: Some(charge.id),
            metric: Some(charge.metric),
            quotas,
        })
    }

    async fn give_back_in_tx(
        tx: &impl DBRunner,
        scope: &AccessScope,
        what: &GiveBack,
        discovered: &Discovered,
        events: &[NotificationEvent],
        env: &Env<'_>,
    ) -> Result<TransitionOutcome<AppliedMutation>, TxError> {
        // @cpt-begin:cpt-cf-quota-enforcement-flow-return:p1:inst-rtn-lock
        // @cpt-begin:cpt-cf-quota-enforcement-flow-rollback:p1:inst-rlb-lookup
        Self::lock(tx, scope, what, discovered).await?;
        let now = (env.clock)();
        let named = charge_repo::find(tx, scope, &scope_key_of(what.charge()), now).await?;
        // @cpt-end:cpt-cf-quota-enforcement-flow-rollback:p1:inst-rlb-lookup
        // @cpt-end:cpt-cf-quota-enforcement-flow-return:p1:inst-rtn-lock
        if let GiveBack::Return(ret) = what
            && let Some(outcome) =
                Self::recorded_return(tx, scope, ret, named.as_ref(), now).await?
        {
            return Ok(outcome);
        }
        // @cpt-begin:cpt-cf-quota-enforcement-flow-return:p1:inst-rtn-reject-if
        // @cpt-begin:cpt-cf-quota-enforcement-flow-return:p1:inst-rtn-reject
        let charge = verified(what, named, discovered)?;
        let entries = charge_repo::entries_of(tx, scope, charge.id).await?;
        let held = held_of(&entries);
        let remaining = remaining(&held);
        let amount = match what {
            GiveBack::Return(ret) => ret.amount,
            // @cpt-begin:cpt-cf-quota-enforcement-flow-rollback:p1:inst-rlb-idem
            GiveBack::Rollback { .. } if remaining == 0 => return Ok(no_op()),
            // @cpt-end:cpt-cf-quota-enforcement-flow-rollback:p1:inst-rlb-idem
            GiveBack::Rollback { .. } => remaining,
        };
        let exceeds = || StorageError::ReturnExceedsCharge {
            requested: amount,
            remaining,
        };
        if amount > remaining {
            return Err(exceeds().into());
        }
        // @cpt-end:cpt-cf-quota-enforcement-flow-return:p1:inst-rtn-reject
        // @cpt-end:cpt-cf-quota-enforcement-flow-return:p1:inst-rtn-reject-if
        // @cpt-begin:cpt-cf-quota-enforcement-flow-return:p1:inst-rtn-unwind
        let taken = unwind(&held, amount).map_err(|_| exceeds())?;
        // @cpt-end:cpt-cf-quota-enforcement-flow-return:p1:inst-rtn-unwind
        // @cpt-begin:cpt-cf-quota-enforcement-flow-return:p1:inst-rtn-apply
        // @cpt-begin:cpt-cf-quota-enforcement-flow-rollback:p1:inst-rlb-apply
        let parts = Self::apply_parts(tx, scope, what, &charge, &entries, &taken, now).await?;
        // @cpt-begin:cpt-cf-quota-enforcement-flow-rollback:p1:inst-rlb-settled-if
        // @cpt-begin:cpt-cf-quota-enforcement-flow-rollback:p1:inst-rlb-settled
        let Some(first) = parts.quotas.first() else {
            return Err(StorageError::ReturnNotApplicable.into());
        };
        // @cpt-end:cpt-cf-quota-enforcement-flow-rollback:p1:inst-rlb-settled
        // @cpt-end:cpt-cf-quota-enforcement-flow-rollback:p1:inst-rlb-settled-if
        Self::record(tx, scope, what, &charge, &parts.moved, now, env).await?;
        // Emitted even inside the settlement window: ADR-0004 silences
        // adjusted and threshold events there, not this one.
        let event = quota_event(
            env.actor,
            first,
            NotificationEventKind::QuotaRollbackApplied,
            json!({
                "operation": what.charge().operation_type.as_str(),
                "idempotency_key": what.charge().key,
                "returned_amount": amount,
                "remaining_amount": remaining - amount,
                "affected_quotas": parts.moved.iter().map(|entry| QuotaId::new(entry.quota_id)).collect::<Vec<_>>(),
                "principal": env.actor.to_string(),
            }),
            now,
        );
        env.enqueuer
            .enqueue_all(tx, std::slice::from_ref(&event))
            .await?;
        env.enqueuer.enqueue_all(tx, events).await?;
        // @cpt-end:cpt-cf-quota-enforcement-flow-rollback:p1:inst-rlb-apply
        // @cpt-end:cpt-cf-quota-enforcement-flow-return:p1:inst-rtn-apply
        let mut event_ids = vec![event.event_id];
        event_ids.extend(events.iter().map(|e| e.event_id));
        Ok(TransitionOutcome::Applied(AppliedMutation {
            decision: Decision::allowed_with_plan(plan_of(parts.plan)),
            mutation: MutationResult {
                counters: counters_of(&parts.moved),
                threshold_crossings: Vec::new(),
                event_ids,
            },
        }))
    }

    /// Rank 1, the Quotas the charge reached, then rank 2, the stripes of the
    /// charge and of the return key.
    async fn lock(
        tx: &impl DBRunner,
        scope: &AccessScope,
        what: &GiveBack,
        discovered: &Discovered,
    ) -> Result<(), TxError> {
        let mut quotas = discovered.quotas.clone();
        quotas.sort_unstable();
        quotas.dedup();
        for quota_id in &quotas {
            quota_repo::find_by_id(tx, scope, *quota_id, Some(RowWait::Nowait)).await?;
        }
        let return_scope = match what {
            GiveBack::Return(ret) => Some(ret.scope()),
            GiveBack::Rollback { .. } => None,
        };
        let mut stripes = vec![what.charge()];
        stripes.extend(return_scope.as_ref());
        lock_scopes::<TxError>(tx, &stripes).await
    }

    /// Grow each reached entry's returned amount and lower the counters of
    /// the entries that take a part, in ascending `quota_id` order (rank 5).
    async fn apply_parts(
        tx: &impl DBRunner,
        scope: &AccessScope,
        what: &GiveBack,
        charge: &charge::Model,
        entries: &[charge_entry::Model],
        taken: &[u64],
        now: OffsetDateTime,
    ) -> Result<Parts, TxError> {
        let mut reached: Vec<(&charge_entry::Model, u64)> = entries
            .iter()
            .zip(taken.iter().copied())
            .filter(|(_, take)| *take > 0)
            .collect();
        reached.sort_by_key(|(entry, _)| entry.quota_id);
        let mut parts = Parts::default();
        for (entry, take) in reached {
            let part = AppliedEntry {
                quota_id: entry.quota_id,
                period_id: entry.period_id,
                amount: take,
                value: 0,
                chain: u32::try_from(entry.chain).unwrap_or(0),
                position: u32::try_from(entry.position).unwrap_or(0),
            };
            parts.plan.push(part.planned());
            charge_repo::add_returned(tx, scope, entry, i64::try_from(take).unwrap_or(i64::MAX))
                .await?;
            let row = quota_repo::find_by_id(tx, scope, entry.quota_id, Some(RowWait::Nowait))
                .await?
                .ok_or_else(|| what.unknown())?;
            let quota = quota_mapping::row_to_quota(row)?;
            if !Self::takes(tx, scope, &quota, entry, charge, now).await? {
                continue;
            }
            let value = credit_counter(
                tx,
                scope,
                entry.quota_id,
                entry.period_id,
                take,
                now,
                RowWait::Nowait,
            )
            .await?;
            parts.moved.push(AppliedEntry { value, ..part });
            parts.quotas.push(quota);
        }
        Ok(parts)
    }

    /// Keep the charge one replay window from now, refresh its `expires_at`,
    /// write the return record of a return, and log each counter that moved.
    async fn record(
        tx: &impl DBRunner,
        scope: &AccessScope,
        what: &GiveBack,
        charge: &charge::Model,
        moved: &[AppliedEntry],
        now: OffsetDateTime,
        env: &Env<'_>,
    ) -> Result<(), TxError> {
        let actor = env.actor;
        let tenant_id = what.charge().tenant_id;
        let keep = expires_at(env.config, tenant_id, &charge.metric, now);
        charge_repo::keep_until(tx, scope, charge.id, keep).await?;
        charge_repo::refresh_expiry(tx, scope, &[charge.id]).await?;
        let operation = match what {
            GiveBack::Return(ret) => {
                charge_repo::insert_return(
                    tx,
                    scope,
                    charge_return::Model {
                        tenant_id: tenant_id.as_uuid(),
                        return_key: ret.key.clone(),
                        charge_id: charge.id,
                        amount: i64::try_from(ret.amount).unwrap_or(i64::MAX),
                        created_at: now,
                    },
                )
                .await?;
                operation_log_repo::OP_RETURN
            }
            GiveBack::Rollback { .. } => operation_log_repo::OP_ROLLBACK,
        };
        for entry in moved {
            operation_log_repo::append(
                tx,
                scope,
                Entry {
                    tenant_id: tenant_id.as_uuid(),
                    quota_id: entry.quota_id,
                    operation,
                    actor,
                    record_version: 1,
                    detail: String::new(),
                    occurred_at: now,
                },
            )
            .await?;
        }
        Ok(())
    }

    /// The outcome a recorded return under `ret`'s key decides, if one lives:
    /// the first result for the same charge and amount, a payload mismatch
    /// otherwise. A repeat naming a metric type other than the charge's is
    /// refused as a first call would be. A record of a deleted charge went
    /// with it and is removed.
    async fn recorded_return(
        tx: &impl DBRunner,
        scope: &AccessScope,
        ret: &ChargeReturn,
        named: Option<&charge::Model>,
        now: OffsetDateTime,
    ) -> Result<Option<TransitionOutcome<AppliedMutation>>, TxError> {
        // @cpt-begin:cpt-cf-quota-enforcement-flow-return:p1:inst-rtn-idem
        let tenant_id = ret.charge.tenant_id.as_uuid();
        let Some(record) = charge_repo::find_return(tx, scope, tenant_id, &ret.key).await? else {
            return Ok(None);
        };
        let Some(recorded) = charge_repo::find_live_by_id(tx, scope, record.charge_id, now).await?
        else {
            charge_repo::delete_return(tx, scope, tenant_id, &ret.key).await?;
            return Ok(None);
        };
        let same = named.is_some_and(|charge| charge.id == record.charge_id)
            && u64::try_from(record.amount).ok() == Some(ret.amount);
        if !same {
            return Err(StorageError::IdempotencyPayloadMismatch.into());
        }
        if recorded.metric != ret.metric.as_str() {
            return Err(StorageError::MetricTypeMismatch {
                requested: ret.metric.as_str().to_owned(),
                charged: recorded.metric,
            }
            .into());
        }
        Ok(Some(no_op()))
        // @cpt-end:cpt-cf-quota-enforcement-flow-return:p1:inst-rtn-idem
    }

    /// Whether `entry` of `charge` can take a part of a return: an
    /// allocation entry always; a consumption entry inside the return window
    /// while its period, read under the counter lock, is not settled; a rate
    /// entry never. The counter's value does not decide it.
    async fn takes(
        tx: &impl DBRunner,
        scope: &AccessScope,
        quota: &quota_enforcement_sdk::Quota,
        entry: &charge_entry::Model,
        charge: &charge::Model,
        now: OffsetDateTime,
    ) -> Result<bool, TxError> {
        Ok(match quota.quota_type {
            QuotaType::Allocation => true,
            QuotaType::Consumption => {
                let Some(period_id) = entry.period_id else {
                    return Ok(false);
                };
                let period = counter_repo::find_by_period_id_for_update(
                    tx,
                    scope,
                    period_id,
                    RowWait::Nowait,
                )
                .await?;
                now < charge.return_window_end && period.is_some_and(|row| !row.is_settled)
            }
            QuotaType::Rate => false,
        })
    }

    /// Release the holds of deactivated Quotas, up to `batch_size` holding
    /// entries, then remove up to `batch_size` charges whose `expires_at` has
    /// passed, the released ones among them, in one transaction and one delete.
    /// Returns the number removed.
    pub(super) async fn reclaim_expired_charges_impl(
        &self,
        batch_size: u32,
        now: OffsetDateTime,
    ) -> Result<u64, StorageError> {
        const OPERATION: &str = "reclaim charges";
        // @cpt-begin:cpt-cf-quota-enforcement-algo-retention-sweep:p1:inst-ret-charge
        self.release_pending_holds(batch_size)
            .await
            .map_err(|error| lift(OPERATION, error))?;
        let removed = self
            .db
            .transaction_ref_mapped(move |tx| {
                Box::pin(async move {
                    let all = AccessScope::allow_all();
                    let doomed = charge_repo::select_expired(tx, &all, batch_size, now).await?;
                    let ids = Self::unguarded(tx, doomed).await?;
                    Ok::<_, TxError>(charge_repo::delete_if_deleted(tx, &all, &ids, now).await?)
                })
            })
            .await
            .map_err(|error| lift(OPERATION, error))?;
        // @cpt-end:cpt-cf-quota-enforcement-algo-retention-sweep:p1:inst-ret-charge
        Ok(removed)
    }

    /// Stop up to `budget` holding entries on deactivated Quotas from holding,
    /// one transaction per Quota under the stripes of its charges, skipping a
    /// charge whose stripe is taken; a Quota with no holding entry left leaves
    /// the list.
    async fn release_pending_holds(&self, budget: u32) -> Result<(), TxError> {
        let all = AccessScope::allow_all();
        let conn = self.db.conn().map_err(TxError::Db)?;
        let mut budget = budget;
        for quota_id in charge_repo::quotas_with_pending_holds(&conn, &all, budget).await? {
            let limit = budget;
            let seen = self
                .db
                .transaction_ref_mapped(move |tx| {
                    Box::pin(async move {
                        let all = AccessScope::allow_all();
                        let charges =
                            charge_repo::charges_held_on(tx, &all, quota_id, limit).await?;
                        let seen = charges.len();
                        let ids = Self::unguarded(tx, charges).await?;
                        charge_repo::release_holds(tx, &all, &ids, quota_id).await?;
                        Ok::<_, TxError>(seen)
                    })
                })
                .await?;
            budget = budget.saturating_sub(u32::try_from(seen).unwrap_or(u32::MAX));
            charge_repo::clear_pending_holds(&conn, &all, quota_id).await?;
            if budget == 0 {
                break;
            }
        }
        Ok(())
    }

    /// The ids of `charges` whose stripes this transaction took; a charge whose
    /// stripe a writer holds waits for the next pass.
    async fn unguarded(
        tx: &impl DBRunner,
        charges: Vec<charge_repo::ChargeScope>,
    ) -> Result<Vec<Uuid>, TxError> {
        let stripe = |(_, tenant_id, operation_type, idem_key): &charge_repo::ChargeScope| {
            idem_repo::stripe_of(&idem_repo::ScopeKey {
                tenant_id: *tenant_id,
                operation_type,
                idem_key,
            })
        };
        let stripes: Vec<i32> = charges.iter().map(stripe).collect();
        let locked = idem_repo::try_lock_stripes(tx, &stripes).await?;
        Ok(charges
            .iter()
            .filter(|charge| locked.contains(&stripe(charge)))
            .map(|charge| charge.0)
            .collect())
    }

    pub(super) async fn read_held_allocation_charge_stats_impl(
        &self,
        before: OffsetDateTime,
    ) -> Result<Vec<HeldAllocationCharges>, StorageError> {
        const OPERATION: &str = "read held allocation charges";
        let conn = self
            .db
            .conn()
            .map_err(|error| unavailable(OPERATION, "connection", &error))?;
        let rows = charge_repo::held_allocation_by_metric(&conn, &AccessScope::allow_all(), before)
            .await
            .map_err(|error| lift(OPERATION, error.into()))?;
        rows.into_iter()
            .map(|(metric, count, oldest_created_at)| {
                MetricId::parse(&metric)
                    .map(|metric| HeldAllocationCharges {
                        metric,
                        count,
                        oldest_created_at,
                    })
                    .map_err(|error| {
                        lift(
                            OPERATION,
                            TxError::Storage(StorageError::Internal(error.to_string())),
                        )
                    })
            })
            .collect()
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "return_store_tests.rs"]
mod tests;
