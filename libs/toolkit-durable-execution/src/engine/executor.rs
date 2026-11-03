//! Executes one activity per delivery. The journal is authoritative; queue
//! redelivery and process shutdown cannot bypass checkpoint or owner fencing.
use crate::authorization::ServiceAction;
use crate::runtime::ActivityInput;
use crate::state::persisted::RunStatus;
use crate::{ActivityContext, ActivityError, RunId};
use crate::{
    catalog::registry::Registry,
    config::Config,
    error::domain::DomainError,
    state::journal::{Claim, Journal},
    store::{JournalStore, StoreError},
};
use chrono::{DateTime, Utc};
use std::{collections::BTreeMap, sync::Arc, time::Duration};
use tokio_util::sync::CancellationToken;

type ActivityTask = AbortOnDrop<Result<serde_json::Value, ActivityError>>;

struct ClaimedDelivery {
    journal: Journal,
    claim: Claim,
    definition: Arc<crate::contracts::ExecutionContract>,
}

/// Runs claimed activities against the journal with owner fencing and lease heartbeats.
pub struct Executor {
    /// Authoritative journal store.
    pub store: JournalStore,
    /// Bound workflow definitions and activity handlers.
    pub registry: Arc<Registry>,
    /// Gear configuration, including lease and shutdown timeouts.
    pub config: Config,
}
impl Executor {
    #[cfg(test)]
    #[cfg_attr(coverage_nightly, coverage(off))]
    pub async fn execute(
        &self,
        id: RunId,
        generation: i64,
        shutdown: CancellationToken,
    ) -> Result<(), StoreError> {
        let budget = super::shutdown::ShutdownBudget::new(Duration::from_secs(
            self.config.shutdown_timeout_secs,
        ));
        self.execute_target(id, generation, None, shutdown, &budget)
            .await
    }
    /// Claims and runs the delivered activity within the shutdown budget.
    /// A timed-out shutdown leaves any remaining claim to recovery.
    pub(crate) async fn execute_target(
        &self,
        id: RunId,
        generation: i64,
        activity: Option<&str>,
        shutdown: CancellationToken,
        budget: &super::shutdown::ShutdownBudget,
    ) -> Result<(), StoreError> {
        if let Ok(result) = budget
            .bound(
                &shutdown,
                Box::pin(self.execute_claim(id, generation, activity, &shutdown, budget)),
            )
            .await
        {
            result
        } else {
            tracing::warn!(run_id = %id.0, "durable execution shutdown timed out; recovery owns any remaining claim");
            Ok(())
        }
    }

    async fn execute_claim(
        &self,
        id: RunId,
        generation: i64,
        activity: Option<&str>,
        shutdown: &CancellationToken,
        budget: &super::shutdown::ShutdownBudget,
    ) -> Result<(), StoreError> {
        let Some(ClaimedDelivery {
            journal,
            claim,
            definition,
        }) = self
            .acquire_claim(id, generation, activity, shutdown)
            .await?
        else {
            return Ok(());
        };
        let mut lease_until = claim_lease(&journal, claim).ok_or(StoreError::Conflict)?;
        let mut confirmed_lease = lease_deadline(lease_until);
        if shutdown.is_cancelled() || !journal.owns(claim, Utc::now()) {
            return Ok(());
        }
        let (mut task, cancel, timeout) =
            self.spawn_activity(&journal, &definition, claim, shutdown)?;
        let timer = tokio::time::sleep(timeout);
        tokio::pin!(timer);
        let mut check = monitor_interval(self.config.heartbeat_secs);
        let mut last_heartbeat = tokio::time::Instant::now();
        let result = loop {
            // cancel-safe: dropping monitoring cancels reads or a fenced heartbeat
            // transaction. A heartbeat committed before cancellation can only
            // postpone recovery; it cannot checkpoint or release the handler.
            let heartbeat_due = last_heartbeat + Duration::from_secs(self.config.heartbeat_secs);
            let renew_by = confirmed_lease
                .checked_sub(Duration::from_secs(self.config.heartbeat_secs))
                .unwrap_or_else(tokio::time::Instant::now);
            let monitoring = tokio::time::timeout_at(renew_by, async {
                check.tick().await;
                self.monitor_once(id, claim, tokio::time::Instant::now() >= heartbeat_due)
                    .await
            });
            tokio::select! {
                biased;
                () = shutdown.cancelled() => {
                    budget.begin();
                    if !task.stop_until(&cancel, budget.cleanup_deadline(confirmed_lease)).await {
                        warn_retained_claim(&journal, claim, "shutdown", lease_until);
                        return Ok(());
                    }
                    return self.release(id, claim).await;
                }
                result = &mut task.0 => break match result {
                    Ok(result) => result,
                    Err(_) => Err(ActivityError::retryable("activity_panicked")),
                },
                () = &mut timer => {
                    if !task.stop_until(&cancel, budget.cleanup_deadline(confirmed_lease)).await {
                        warn_retained_claim(&journal, claim, "activity_timeout", lease_until);
                        return Ok(());
                    }
                    break Err(ActivityError::retryable("activity_timeout"));
                }
                outcome = monitoring => {
                    let outcome = if let Ok(result) = outcome {
                        result
                    } else {
                        warn_retained_claim(&journal, claim, "monitoring_budget_exceeded", lease_until);
                        Err(StoreError::AuthorizationUnavailable)
                    };
                    match outcome {
                        Err(error) => {
                            if !task.stop_until(&cancel, budget.cleanup_deadline(confirmed_lease)).await {
                                warn_retained_claim(&journal, claim, "monitoring_failed", lease_until);
                            }
                            return Err(error);
                        }
                        Ok(Monitor::Lost) => {
                            if !task.stop_until(&cancel, budget.cleanup_deadline(confirmed_lease)).await {
                                warn_retained_claim(&journal, claim, "ownership_lost", lease_until);
                            }
                            return Ok(());
                        }
                        Ok(Monitor::Revoked) => {
                            if !task.stop_until(&cancel, budget.cleanup_deadline(confirmed_lease)).await {
                                warn_retained_claim(&journal, claim, "definition_revoked", lease_until);
                                return Ok(());
                            }
                            return self.cancel_revoked(id, claim).await;
                        }
                        Ok(Monitor::Cancelled) => {
                            if !task.stop_until(&cancel, budget.cleanup_deadline(confirmed_lease)).await {
                                warn_retained_claim(&journal, claim, "user_cancelled", lease_until);
                                return Ok(());
                            }
                            break Err(ActivityError::Cancelled);
                        }
                        Ok(Monitor::Running { until, renewed }) => {
                            lease_until = until;
                            confirmed_lease = lease_deadline(until);
                            if renewed {
                                last_heartbeat = tokio::time::Instant::now();
                            }
                        }
                    }
                }
            }
        };
        // Reload before checkpoint: cancellation/heartbeat may have advanced the
        // revision. A matching fence is necessary but never sufficient without CAS.
        for _ in 0..8 {
            let scope = self.store.worker_scope(ServiceAction::Execute).await?;
            let Some(mut current) = self.store.get(&scope, id).await? else {
                return Ok(());
            };
            let registration = self
                .store
                .worker_definition(&current.run.definition)
                .await?;
            if !registration.permits_execution(
                current.registration_generation,
                current.execution.is_compensation(),
            ) {
                return self.cancel_revoked(id, claim).await;
            }
            let revision = current.revision;
            let transition = match &result {
                Ok(value) => current.complete(claim, value.clone(), Utc::now()),
                Err(error) => current.fail(claim, error, Utc::now(), jitter()),
            };
            if matches!(transition, Err(DomainError::LeaseLost)) {
                return Ok(());
            }
            transition.map_err(lost_race)?;
            let enqueue = current.run.next_attempt_at.is_some();
            match self
                .store
                .save(scope.clone(), revision, current, enqueue)
                .await
            {
                Err(StoreError::Conflict) => {}
                result => return result,
            }
        }
        Err(StoreError::Conflict)
    }
    // A CAS loser has performed no external work; reload and retry the same delivery.
    async fn acquire_claim(
        &self,
        id: RunId,
        generation: i64,
        activity: Option<&str>,
        shutdown: &CancellationToken,
    ) -> Result<Option<ClaimedDelivery>, StoreError> {
        for _ in 0..8 {
            let scope = self.store.worker_scope(ServiceAction::Execute).await?;
            let Some(mut journal) = self.store.get(&scope, id).await? else {
                // A run missing from the journal usually means two engines share a
                // queue name, or the service scope excludes its tenant; the owning
                // engine's recovery republishes the delivery.
                tracing::warn!(
                    run_id = %id.0,
                    generation,
                    "durable delivery for a run not visible to this engine; check queue names and service scope"
                );
                return Ok(None);
            };
            if journal.run.status.is_terminal() {
                return Ok(None);
            }
            let registration = self
                .store
                .worker_definition(&journal.run.definition)
                .await?;
            if !registration.permits_execution(
                journal.registration_generation,
                journal.execution.is_compensation(),
            ) {
                return Ok(None);
            }
            if !self
                .registry
                .available(&journal.run.definition, journal.registration_generation)
            {
                return Ok(None);
            }
            let Ok(definition) = self.registry.executable_contract(&journal.run.definition) else {
                return Ok(None);
            };
            let revision = journal.revision;
            let prior_generation = journal.delivery_generation;
            let claim = journal
                .claim_target(
                    &definition,
                    generation,
                    activity,
                    Utc::now(),
                    self.config.lease(),
                )
                .map_err(lost_race)?;
            let Some(claim) = claim else {
                if journal.run.status == RunStatus::Blocked
                    || journal.run.status == RunStatus::Failed
                    || journal.delivery_generation != prior_generation
                {
                    let enqueue = journal.run.next_attempt_at.is_some();
                    return ignore_race(self.store.save(scope, revision, journal, enqueue).await)
                        .map(|()| None);
                }
                return Ok(None);
            };
            if self
                .persist_claim(scope, revision, &journal, claim, shutdown)
                .await?
            {
                return Ok(Some(ClaimedDelivery {
                    journal,
                    claim,
                    definition,
                }));
            }
            if shutdown.is_cancelled() {
                return Ok(None);
            }
            tokio::task::yield_now().await;
        }
        Err(StoreError::Conflict)
    }
    fn spawn_activity(
        &self,
        journal: &Journal,
        definition: &crate::contracts::ExecutionContract,
        claim: Claim,
        shutdown: &CancellationToken,
    ) -> Result<(ActivityTask, CancellationToken, Duration), StoreError> {
        let instance = journal.execution.instances.get(claim.step).cloned();
        let template = instance.as_ref().map_or(claim.step, |i| i.template);
        let mut step = definition.activities[template].clone();
        let reverse = instance
            .as_ref()
            .is_some_and(|i| i.identity.phase == crate::ExecutionPhase::Compensation);
        if reverse {
            let policy = step
                .compensation
                .as_ref()
                .ok_or(StoreError::Invariant("compensation policy missing"))?;
            step.timeout = policy.timeout;
        }
        let cancel = shutdown.child_token();
        let context = ActivityContext::from(crate::activity::ActivityContextInit {
            instance: instance.as_ref().map_or_else(
                || crate::ActivityInstanceId {
                    step: step.id.clone(),
                    item_index: None,
                    phase: crate::ExecutionPhase::Forward,
                },
                |i| i.identity.clone(),
            ),
            execution_epoch: journal.run.execution_epoch,
            run_id: journal.run.id,
            activity_id: step.id.clone(),
            attempt: journal.run.activities[claim.step].attempts,
            idempotency_key: format!(
                "{}:{}",
                journal.run.id.0, journal.run.activities[claim.step].id.0
            ),
            owner: journal.run.owner.clone(),
            deadline: Utc::now()
                + chrono::Duration::from_std(step.timeout).map_err(|_| StoreError::OUT_OF_RANGE)?,
            cancellation: cancel.clone(),
        });
        let mut input = ActivityInput {
            selected_input: instance.as_ref().map(|i| i.input.clone()),
            run_input: journal.input.clone(),
            previous_results: journal
                .run
                .activities
                .iter()
                .filter_map(|a| a.result.clone().map(|r| (a.id.0.clone(), r)))
                .collect::<BTreeMap<_, _>>(),
        };
        input.previous_results = journal.execution.checkpoints();
        let compensator = if reverse {
            Some(
                self.registry
                    .compensator(&definition.name, &step.id)
                    .map_err(StoreError::Domain)?,
            )
        } else {
            None
        };
        let handler = self
            .registry
            .handler(&definition.name, &step.id)
            .map_err(|_| StoreError::Conflict)?;
        let task = AbortOnDrop(tokio::spawn(async move {
            if let Some(compensator) = compensator {
                let i = instance
                    .ok_or_else(|| ActivityError::permanent("compensation_instance_missing"))?;
                let receipt = i
                    .receipt
                    .ok_or_else(|| ActivityError::permanent("compensation_receipt_missing"))?;
                compensator
                    .execute(context, i.input, receipt)
                    .await
                    .map(|()| serde_json::Value::Null)
            } else {
                handler.execute(context, input).await
            }
        }));
        Ok((task, cancel, step.timeout))
    }
    async fn persist_claim(
        &self,
        scope: toolkit_security::AccessScope,
        revision: i64,
        journal: &Journal,
        claim: Claim,
        shutdown: &CancellationToken,
    ) -> Result<bool, StoreError> {
        let until = claim_lease(journal, claim).ok_or(StoreError::Conflict)?;
        // Authorization and persistence share the claim's lease. A cancelled
        // commit may have succeeded; fenced recovery owns that ambiguous outcome.
        let persistence = tokio::time::timeout_at(
            lease_deadline(until),
            self.store.save(
                scope,
                revision,
                journal.clone(),
                journal.run.next_attempt_at.is_some(),
            ),
        );
        tokio::select! {
            biased;
            () = shutdown.cancelled() => Ok(false),
            result = persistence => match result {
                Ok(Err(StoreError::Conflict)) => Ok(false),
                Ok(result) => {
                    result?;
                    Ok(!shutdown.is_cancelled() && journal.owns(claim, Utc::now()))
                }
                Err(_) => {
                    warn_retained_claim(journal, claim, "claim_persistence_timeout", until);
                    Err(StoreError::Conflict)
                }
            }
        }
    }
    // cancel-safe: reads have no effects; heartbeat persistence uses journal CAS.
    async fn monitor_once(
        &self,
        id: RunId,
        claim: Claim,
        renew: bool,
    ) -> Result<Monitor, StoreError> {
        let scope = self.store.worker_scope(ServiceAction::Execute).await?;
        let Some(current) = self.store.claim_probe(&scope, id, claim).await? else {
            return Ok(Monitor::Lost);
        };
        if !current.owns(claim, Utc::now()) {
            return Ok(Monitor::Lost);
        }
        let registration = self.store.worker_definition(&current.definition).await?;
        if !registration.permits_execution(
            current.registration_generation,
            current.phase == crate::ExecutionPhase::Compensation,
        ) {
            return Ok(Monitor::Revoked);
        }
        if current.cancellation_requested {
            return Ok(Monitor::Cancelled);
        }
        let until = if renew {
            let Some(until) = self.heartbeat(id, claim).await? else {
                return Ok(Monitor::Lost);
            };
            until
        } else {
            current.lease_until.ok_or(StoreError::Conflict)?
        };
        Ok(Monitor::Running {
            until,
            renewed: renew,
        })
    }
    async fn cancel_revoked(&self, id: RunId, claim: Claim) -> Result<(), StoreError> {
        for _ in 0..8 {
            let scope = self.store.worker_scope(ServiceAction::Execute).await?;
            let Some(mut j) = self.store.get(&scope, id).await? else {
                return Ok(());
            };
            let revision = j.revision;
            if j.execution.contract.saga {
                j.request_abort_at(
                    j.run.execution_epoch,
                    Utc::now(),
                    Some("definition_unregistered"),
                )
                .map_err(lost_race)?;
            } else {
                j.request_cancel_at(
                    j.run.execution_epoch,
                    Utc::now(),
                    Some("definition_unregistered"),
                )
                .map_err(lost_race)?;
            }
            if j.owns(claim, Utc::now()) {
                j.release(claim, Utc::now()).map_err(lost_race)?;
            }
            let enqueue = j.run.next_attempt_at.is_some();
            match self.store.save(scope, revision, j, enqueue).await {
                Err(StoreError::Conflict) => {}
                result => return result,
            }
        }
        Err(StoreError::Conflict)
    }
    /// Reconciles stopping definitions and evicts revoked generations from the registry.
    pub(crate) async fn reconcile_definitions(&self) -> Result<(), StoreError> {
        for definition in self.store.definitions().await? {
            let current = if definition.state == crate::registration::RegistrationState::Stopping {
                self.store.reconcile_definition(&definition).await?;
                self.store
                    .worker_definition(&definition.contract.name)
                    .await?
            } else {
                // The catalog scan already authorized this observation. Stable rows
                // need no per-definition AuthN/PDP exchange or database lookup.
                definition
            };
            if current.state != crate::registration::RegistrationState::Stopping
                && let Some(through) = current.revoked_through
            {
                self.registry.evict(&current.contract.name, through);
            }
        }
        Ok(())
    }
    async fn heartbeat(
        &self,
        id: RunId,
        claim: Claim,
    ) -> Result<Option<DateTime<Utc>>, StoreError> {
        let scope = self.store.worker_scope(ServiceAction::Heartbeat).await?;
        for _ in 0..8 {
            let Some(mut j) = self.store.get(&scope, id).await? else {
                return Ok(None);
            };
            if !j
                .heartbeat(claim, Utc::now(), self.config.lease())
                .map_err(lost_race)?
            {
                return Ok(None);
            }
            let until = claim_lease(&j, claim).ok_or(StoreError::Conflict)?;
            match self.store.save(scope.clone(), j.revision, j, false).await {
                Err(StoreError::Conflict) => {}
                result => return result.map(|()| Some(until)),
            }
        }
        Err(StoreError::Conflict)
    }
    async fn release(&self, id: RunId, claim: Claim) -> Result<(), StoreError> {
        let scope = self.store.worker_scope(ServiceAction::Execute).await?;
        for _ in 0..8 {
            let Some(mut j) = self.store.get(&scope, id).await? else {
                return Ok(());
            };
            if j.release(claim, Utc::now()).is_err() {
                return Ok(());
            }
            let enqueue = j.run.next_attempt_at.is_some();
            match self.store.save(scope.clone(), j.revision, j, enqueue).await {
                Err(StoreError::Conflict) => {}
                result => return result,
            }
        }
        Err(StoreError::Conflict)
    }
    /// Recovers runs whose activity claims have expired and promotes waiting successors.
    pub async fn recover(&self) -> Result<(), StoreError> {
        let now = Utc::now();
        let mut cursor = None;
        loop {
            let scope = self.store.worker_scope(ServiceAction::Recover).await?;
            let (journals, next) = self.store.expired_claims_page(&scope, now, cursor).await?;
            if journals.is_empty() && next.is_none() {
                break;
            }
            cursor = next;
            for mut journal in journals {
                let scope = self.store.worker_scope(ServiceAction::Recover).await?;
                let registration = match self.store.worker_definition(&journal.run.definition).await
                {
                    Ok(registration) => registration,
                    Err(StoreError::Domain(DomainError::DefinitionNotFound(_))) => {
                        tracing::warn!(run_id = %journal.run.id.0,
                            definition = %journal.run.definition,
                            "durable recovery deferred: definition not registered");
                        continue;
                    }
                    Err(error) => return Err(error),
                };
                if !registration.permits_execution(
                    journal.registration_generation,
                    journal.execution.is_compensation(),
                ) {
                    if journal.execution.contract.saga {
                        journal
                            .request_abort_at(
                                journal.run.execution_epoch,
                                now,
                                Some("definition_unregistered"),
                            )
                            .map_err(lost_race)?;
                    } else {
                        journal
                            .request_cancel_at(
                                journal.run.execution_epoch,
                                now,
                                Some("definition_unregistered"),
                            )
                            .map_err(lost_race)?;
                    }
                }
                if journal.recover(now).map_err(lost_race)? {
                    let enqueue = journal.run.next_attempt_at.is_some();
                    ignore_race(
                        self.store
                            .save(scope, journal.revision, journal, enqueue)
                            .await,
                    )?;
                }
            }
            if cursor.is_none() {
                break;
            }
        }
        self.store.promote_successors().await?;
        Ok(())
    }
}
fn monitor_interval(seconds: u64) -> tokio::time::Interval {
    let mut interval = tokio::time::interval(Duration::from_secs(seconds));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    interval
}

enum Monitor {
    Running { until: DateTime<Utc>, renewed: bool },
    Lost,
    Revoked,
    Cancelled,
}
fn claim_lease(journal: &Journal, claim: Claim) -> Option<DateTime<Utc>> {
    journal.execution.instances.get(claim.step)?.lease_until
}
fn lease_deadline(until: DateTime<Utc>) -> tokio::time::Instant {
    tokio::time::Instant::now() + (until - Utc::now()).to_std().unwrap_or_default()
}
fn warn_retained_claim(journal: &Journal, claim: Claim, reason: &str, lease_until: DateTime<Utc>) {
    tracing::warn!(
        run_id = %journal.run.id.0,
        activity_id = %journal.run.activities[claim.step].id.0,
        attempt = journal.run.activities[claim.step].attempts,
        fence = claim.fence,
        reason,
        retained_lease_until = %lease_until,
        "durable activity cleanup unconfirmed; retaining claim for recovery"
    );
}
/// Worker transitions race with commands and other workers; losing one is a
/// retryable `Conflict`. Other domain failures keep their cause.
fn lost_race(error: DomainError) -> StoreError {
    match error {
        DomainError::LeaseLost | DomainError::ConcurrentUpdate | DomainError::InvalidState(_) => {
            StoreError::Conflict
        }
        error => StoreError::Domain(error),
    }
}
fn ignore_race(result: Result<(), StoreError>) -> Result<(), StoreError> {
    match result {
        Err(StoreError::Conflict) => Ok(()),
        other => other,
    }
}
fn jitter() -> u16 {
    (uuid::Uuid::new_v4().as_u128() % 1001) as u16
}
struct AbortOnDrop<T>(tokio::task::JoinHandle<T>);
impl<T> AbortOnDrop<T> {
    /// A cooperative handler must return only after its external work has
    /// stopped. Aborting a future alone cannot confirm that a process stopped.
    #[cfg(test)]
    async fn stop(&mut self, cancel: &CancellationToken, seconds: u64) -> bool {
        self.stop_until(
            cancel,
            tokio::time::Instant::now() + Duration::from_secs(seconds),
        )
        .await
    }
    async fn stop_until(
        &mut self,
        cancel: &CancellationToken,
        deadline: tokio::time::Instant,
    ) -> bool {
        cancel.cancel();
        match tokio::time::timeout_at(deadline, &mut self.0).await {
            Ok(Ok(_)) => true,
            Ok(Err(_)) => false,
            Err(_) => {
                self.0.abort();
                false
            }
        }
    }
}
impl<T> Drop for AbortOnDrop<T> {
    fn drop(&mut self) {
        self.0.abort();
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "../../tests/unit/executor_tests.rs"]
mod tests;

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "../../tests/unit/executor_behavior_tests.rs"]
mod behavior_tests;

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "../../tests/unit/control_plane_tests.rs"]
mod control_plane_tests;

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "../../tests/unit/staged_runtime_tests.rs"]
mod staged_runtime_tests;
