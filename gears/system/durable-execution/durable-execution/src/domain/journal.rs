//! Deterministic run transitions. The repository persists every transition with
//! compare-and-swap; neither delivery nor a stale worker can override a checkpoint.
use crate::domain::error::DomainError;
use crate::domain::persisted::{ActivityRun, ActivityStatus, ExecutionSnapshot, Run, RunStatus};
use chrono::{DateTime, Utc};
use durable_execution_sdk::contracts::ExecutionContract;
use durable_execution_sdk::{ActivityError, ExecutionOwner, RunId};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::time::Duration;
use toolkit_macros::domain_model;

#[domain_model]
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Journal {
    pub run: Run,
    pub input: Value,
    #[serde(default)]
    pub completed_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub registration_generation: u64,
    #[cfg(test)]
    #[serde(skip)]
    pub(crate) registration_contract: Option<ExecutionContract>,
    pub fingerprint: String,
    pub revision: i64,
    pub fence: i64,
    pub lease_until: Option<DateTime<Utc>>,
    pub delivery_generation: i64,
    pub cancellation_requested: bool,
    #[serde(default)]
    pub parallel: Option<crate::domain::parallel::StageJournal>,
}

#[domain_model]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Claim {
    pub fence: i64,
    pub step: usize,
}

/// Shared fencing predicate for hydrated journals and lightweight worker probes.
pub fn owns_activity(
    expected_fence: i64,
    current_fence: i64,
    status: ActivityStatus,
    lease_until: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
) -> bool {
    expected_fence == current_fence
        && status == ActivityStatus::Running
        && lease_until.is_some_and(|until| until > now)
}

impl Journal {
    /// The selected activity is stable for a delivery generation even if the
    /// queue wakes late and other retry deadlines have elapsed meanwhile.
    #[must_use]
    pub fn delivery_activity(&self) -> Option<String> {
        let at = self.run.next_attempt_at?;
        if self.cancellation_requested
            || self.run.status.is_terminal()
            || self.run.status == RunStatus::Blocked
        {
            return None;
        }
        if let Some(j) = &self.parallel {
            j.ready(at).first().map(|i| j.steps[*i].id.clone())
        } else {
            self.run
                .activities
                .iter()
                .find(|a| a.status != ActivityStatus::Succeeded)
                .map(|a| a.id.0.clone())
        }
    }
    pub fn new(
        id: RunId,
        owner: ExecutionOwner,
        definition: &ExecutionContract,
        input: Value,
        now: DateTime<Utc>,
    ) -> Result<Self, DomainError> {
        Ok(Self {
            run: Run {
                id,
                definition: definition.name.clone(),
                owner,
                status: RunStatus::Queued,
                activities: definition
                    .activities
                    .iter()
                    .map(|step| ActivityRun {
                        id: step.id.clone(),
                        status: ActivityStatus::Pending,
                        attempts: 0,
                        attempt_history: Vec::new(),
                        budget_attempts: 0,
                        result: None,
                        error_code: None,
                        started_at: None,
                        finished_at: None,
                    })
                    .collect(),
                created_at: now,
                updated_at: now,
                next_attempt_at: Some(now),
                error_code: None,
                execution_epoch: 0,
                stop_reason: None,
                previous_executions: Vec::new(),
            },
            input,
            completed_at: None,
            registration_generation: 0,
            #[cfg(test)]
            registration_contract: Some(definition.clone()),
            fingerprint: definition.fingerprint()?,
            revision: 0,
            fence: 0,
            lease_until: None,
            delivery_generation: 0,
            cancellation_requested: false,
            parallel: if definition.parallel_groups.is_empty() {
                None
            } else {
                Some(crate::domain::parallel::StageJournal::new(
                    definition.stages()?,
                    now,
                )?)
            },
        })
    }
    pub fn claim(
        &mut self,
        definition: &ExecutionContract,
        generation: i64,
        now: DateTime<Utc>,
        lease: Duration,
    ) -> Result<Option<Claim>, DomainError> {
        if self.parallel.is_some() {
            return self.claim_parallel(definition, generation, now, lease);
        }
        if self.run.status.is_terminal()
            || self.cancellation_requested
            || self.run.status == RunStatus::Blocked
            || generation != self.delivery_generation
            || self.run.next_attempt_at.is_none() && self.run.status == RunStatus::Queued
            || self.lease_until.is_some_and(|until| until > now)
            || self.run.next_attempt_at.is_some_and(|next| next > now)
        {
            return Ok(None);
        }
        if self.fingerprint != definition.fingerprint()? {
            self.block("definition_mismatch", now);
            return Ok(None);
        }
        let Some(step) = self
            .run
            .activities
            .iter()
            .position(|a| a.status != ActivityStatus::Succeeded)
        else {
            return Err(DomainError::InvalidState("run has no unfinished activity"));
        };
        if self.run.activities[step].budget_attempts
            >= definition.activities[step].retry.max_attempts
        {
            self.run.status = RunStatus::Failed;
            self.run.error_code = Some("attempts_exhausted".into());
            self.run.activities[step].status = ActivityStatus::Failed;
            self.run.activities[step].error_code = self.run.error_code.clone();
            self.run.activities[step].finished_at = Some(now);
            self.lease_until = None;
            self.touch(now);
            return Ok(None);
        }
        self.fence = self
            .fence
            .checked_add(1)
            .ok_or(DomainError::Internal("fence overflow"))?;
        self.lease_until = Some(add_duration(now, lease)?);
        self.run.status = RunStatus::Running;
        self.run.next_attempt_at = None;
        let activity = &mut self.run.activities[step];
        activity.status = ActivityStatus::Running;
        activity.attempts += 1;
        activity.budget_attempts += 1;
        activity.started_at = Some(now);
        activity.finished_at = None;
        activity.error_code = None;
        self.run.error_code = None;
        self.touch(now);
        Ok(Some(Claim {
            fence: self.fence,
            step,
        }))
    }
    #[must_use]
    pub fn owns(&self, claim: Claim, now: DateTime<Utc>) -> bool {
        if let Some(j) = &self.parallel {
            return u64::try_from(claim.fence).is_ok_and(|fence| {
                j.owns(
                    crate::domain::parallel::Claim {
                        step: claim.step,
                        fence,
                    },
                    now,
                )
            });
        }
        matches!(self.run.status, RunStatus::Running | RunStatus::Cancelling)
            && self.run.activities.get(claim.step).is_some_and(|activity| {
                owns_activity(
                    claim.fence,
                    self.fence,
                    activity.status,
                    self.lease_until,
                    now,
                )
            })
    }
    pub fn heartbeat(
        &mut self,
        claim: Claim,
        now: DateTime<Utc>,
        lease: Duration,
    ) -> Result<bool, DomainError> {
        if let Some(j) = &mut self.parallel {
            let claim = parallel_claim(claim)?;
            if !j.owns(claim, now) {
                return Ok(false);
            }
            j.heartbeat(claim, now, lease)?;
            self.sync_parallel(now, false)?;
            return Ok(true);
        }
        if !self.owns(claim, now) {
            return Ok(false);
        }
        self.lease_until = Some(add_duration(now, lease)?);
        self.touch(now);
        Ok(true)
    }
    /// Checkpoint + scheduling are persisted together by the journal repository.
    pub fn complete(
        &mut self,
        claim: Claim,
        result: Value,
        now: DateTime<Utc>,
    ) -> Result<(), DomainError> {
        if let Some(j) = &mut self.parallel {
            j.complete(parallel_claim(claim)?, result, now)?;
            return self.sync_parallel(now, true);
        }
        if !self.owns(claim, now) {
            return Err(DomainError::LeaseLost);
        }
        // A successful return confirms the effect even if cancellation raced.
        // Checkpoint it before stopping unfinished work, as parallel stages do.
        let activity = &mut self.run.activities[claim.step];
        activity.result = Some(result);
        activity.status = ActivityStatus::Succeeded;
        activity.finished_at = Some(now);
        self.lease_until = None;
        if self
            .run
            .activities
            .iter()
            .all(|a| a.status == ActivityStatus::Succeeded)
        {
            self.run.status = RunStatus::Succeeded;
            self.run.next_attempt_at = None;
        } else if self.cancellation_requested {
            self.finish_cancel(now);
            return Ok(());
        } else {
            self.schedule(now)?;
        }
        self.touch(now);
        Ok(())
    }
    pub fn fail(
        &mut self,
        claim: Claim,
        error: &ActivityError,
        definition: &ExecutionContract,
        now: DateTime<Utc>,
        jitter: u16,
    ) -> Result<(), DomainError> {
        if let Some(j) = &mut self.parallel {
            let policy = &definition.activities[claim.step].retry;
            let attempts = j.steps[claim.step].budget_attempts;
            let next = match error {
                ActivityError::Retryable { retry_after, .. } if attempts < policy.max_attempts => {
                    Some(add_duration(
                        now,
                        policy.delay(attempts, *retry_after, jitter),
                    )?)
                }
                _ => None,
            };
            j.fail(parallel_claim(claim)?, &error.code(), next, now)?;
            return self.sync_parallel(now, true);
        }
        if !self.owns(claim, now) {
            return Err(DomainError::LeaseLost);
        }
        if self.cancellation_requested {
            self.finish_cancel(now);
            return Ok(());
        }
        let policy = &definition.activities[claim.step].retry;
        let code = error.code();
        let attempts = self.run.activities[claim.step].budget_attempts;
        let next = match error {
            ActivityError::Retryable { retry_after, .. } if attempts < policy.max_attempts => Some(
                add_duration(now, policy.delay(attempts, *retry_after, jitter))?,
            ),
            _ => None,
        };
        let activity = &mut self.run.activities[claim.step];
        activity.error_code = Some(code.clone());
        activity.finished_at = Some(now);
        self.run.error_code = Some(code);
        self.lease_until = None;
        if let Some(next) = next {
            activity.status = ActivityStatus::RetryWait;
            self.schedule(next)?;
            self.run.status = RunStatus::RetryWait;
        } else {
            // Handler cancellation is not a user cancellation. It is terminal
            // unless the executor handles shutdown by releasing the claim.
            activity.status = ActivityStatus::Failed;
            self.run.status = RunStatus::Failed;
            self.run.next_attempt_at = None;
        }
        self.touch(now);
        Ok(())
    }
    /// Epoch check and mutation belong to the same journal CAS transaction.
    ///
    /// A reason is stored only with the stop that this call newly records, and
    /// only for `expected_epoch`. A terminal run, a duplicate delivery and a
    /// stale epoch leave the stored reason untouched.
    pub fn request_cancel_at(
        &mut self,
        expected_epoch: u64,
        now: DateTime<Utc>,
        reason: Option<&str>,
    ) -> Result<bool, DomainError> {
        if self.run.execution_epoch != expected_epoch {
            return Err(DomainError::ConcurrentUpdate);
        }
        if self.run.status.is_terminal() || self.cancellation_requested {
            return Ok(false);
        }
        if let Some(reason) = reason.map(str::trim).filter(|s| !s.is_empty()) {
            self.run.stop_reason = Some(reason.chars().take(240).collect());
        }
        self.request_cancel(now);
        Ok(true)
    }
    pub fn request_cancel(&mut self, now: DateTime<Utc>) {
        if self.run.status.is_terminal() {
            return;
        }
        if let Some(j) = &mut self.parallel {
            j.stop();
            self.cancellation_requested = true;
            // Stopping does not create deliveries or advance a generation.
            project_parallel(&mut self.run, &mut self.lease_until, j, now);
            self.touch(now);
            return;
        }
        self.cancellation_requested = true;
        if self.lease_until.is_some_and(|until| until > now) {
            self.run.status = RunStatus::Cancelling;
            self.touch(now);
        } else {
            self.finish_cancel(now);
        }
    }
    /// Restart only unfinished work, retaining successful checkpoints and a
    /// snapshot of the preceding execution. The repository commits this change
    /// together with its new delivery and protects it with revision CAS.
    pub fn continue_execution(
        &mut self,
        expected_epoch: u64,
        from: RunStatus,
        definition: &ExecutionContract,
        now: DateTime<Utc>,
    ) -> Result<bool, DomainError> {
        if self.parallel.is_some() {
            if !matches!(from, RunStatus::Failed | RunStatus::Cancelled) {
                return Err(DomainError::InvalidState(
                    "only failed or cancelled runs can continue",
                ));
            }
            if self.fingerprint != definition.fingerprint()? {
                return Err(DomainError::DefinitionMismatch);
            }
            let duplicate = self.run.execution_epoch
                == expected_epoch
                    .checked_add(1)
                    .ok_or(DomainError::Internal("epoch overflow"))?
                && self
                    .run
                    .previous_executions
                    .last()
                    .is_some_and(|s| s.epoch == expected_epoch && s.status == from);
            if duplicate {
                return Ok(false);
            }
            if self.run.status != from {
                return Err(DomainError::InvalidState("run status changed"));
            }
            if self.run.execution_epoch != expected_epoch {
                return Err(DomainError::ConcurrentUpdate);
            }
            // Resuming unfinished stages always publishes a new delivery. Check
            // its counter before changing either the stage state or the archive.
            self.delivery_generation
                .checked_add(1)
                .ok_or(DomainError::Internal("delivery generation overflow"))?;
            let j = self
                .parallel
                .as_mut()
                .ok_or(DomainError::Internal("parallel journal missing"))?;
            if j.epoch != expected_epoch {
                return Err(DomainError::ConcurrentUpdate);
            }
            j.continue_execution(expected_epoch, from == RunStatus::Cancelled, now)?;
            let next_epoch = j.epoch;
            // The projected run still holds the old epoch until sync_parallel.
            let snapshot = self.archive_epoch(from);
            self.run.previous_executions.push(snapshot);
            self.run.execution_epoch = next_epoch;
            self.cancellation_requested = false;
            self.sync_parallel(now, true)?;
            return Ok(true);
        }
        if !matches!(from, RunStatus::Failed | RunStatus::Cancelled) {
            return Err(DomainError::InvalidState(
                "only failed or cancelled runs can continue",
            ));
        }
        if self.run.execution_epoch
            == expected_epoch
                .checked_add(1)
                .ok_or(DomainError::Internal("epoch overflow"))?
            && self
                .run
                .previous_executions
                .last()
                .is_some_and(|s| s.epoch == expected_epoch && s.status == from)
        {
            return Ok(false);
        }
        if self.run.execution_epoch != expected_epoch {
            return Err(DomainError::ConcurrentUpdate);
        }
        if self.run.status != from
            || self.lease_until.is_some_and(|until| until > now)
            || self
                .run
                .activities
                .iter()
                .any(|step| step.status == ActivityStatus::Running)
        {
            return Err(DomainError::InvalidState("run is still executing"));
        }
        if self.fingerprint != definition.fingerprint()? {
            return Err(DomainError::DefinitionMismatch);
        }
        self.delivery_generation
            .checked_add(1)
            .ok_or(DomainError::Internal("delivery generation overflow"))?;
        let archived = self.archive_epoch(self.run.status);
        self.run.previous_executions.push(archived);
        self.run.execution_epoch += 1;
        for step in &mut self.run.activities {
            if step.status != ActivityStatus::Succeeded {
                step.status = ActivityStatus::Pending;
                step.budget_attempts = 0;
                step.result = None;
                step.error_code = None;
                step.started_at = None;
                step.finished_at = None;
            }
        }
        self.cancellation_requested = false;
        self.lease_until = None;
        self.run.error_code = None;
        self.schedule(now)?;
        self.touch(now);
        Ok(true)
    }
    /// Move the current epoch's stop sentence onto the archived snapshot and
    /// clear it so the next epoch does not inherit the sentence.
    fn archive_epoch(&mut self, status: RunStatus) -> ExecutionSnapshot {
        let snapshot = ExecutionSnapshot {
            completed_at: self.completed_at,
            epoch: self.run.execution_epoch,
            status,
            ended_at: self.run.updated_at,
            activities: self.run.activities.clone(),
            stop_reason: self.run.stop_reason.clone(),
        };
        self.run.stop_reason = None;
        snapshot
    }
    fn finish_cancel(&mut self, now: DateTime<Utc>) {
        self.run.status = RunStatus::Cancelled;
        self.lease_until = None;
        self.run.next_attempt_at = None;
        for activity in &mut self.run.activities {
            if activity.status != ActivityStatus::Succeeded {
                activity.status = ActivityStatus::Cancelled;
                activity.finished_at = Some(now);
            }
        }
        self.touch(now);
    }
    pub fn recover(&mut self, now: DateTime<Utc>) -> Result<bool, DomainError> {
        if let Some(j) = &mut self.parallel {
            if self.run.status.is_terminal() || self.run.status == RunStatus::Blocked {
                return Ok(false);
            }
            let expired = j.steps.iter().any(|s| {
                s.status == ActivityStatus::Running && s.lease_until.is_none_or(|t| t <= now)
            });
            if !expired {
                return Ok(false);
            }
            j.recover(now);
            self.sync_parallel(now, true)?;
            return Ok(true);
        }
        if self.run.status.is_terminal()
            || self.run.status == RunStatus::Blocked
            || self.lease_until.is_some_and(|until| until > now)
        {
            return Ok(false);
        }
        if self.cancellation_requested {
            self.finish_cancel(now);
            return Ok(true);
        }
        if self.run.status != RunStatus::Running {
            return Ok(false);
        }
        // Preserve attempts and successful checkpoints. The uncertain activity
        // must be repeated with the same effect-idempotency key.
        for activity in &mut self.run.activities {
            if activity.status == ActivityStatus::Running {
                activity.status = ActivityStatus::Pending;
            }
        }
        self.lease_until = None;
        self.schedule(now)?;
        self.touch(now);
        Ok(true)
    }
    /// Graceful shutdown leaves the run runnable, not user-cancelled.
    pub fn release(&mut self, claim: Claim, now: DateTime<Utc>) -> Result<(), DomainError> {
        if !self.owns(claim, now) {
            return Err(DomainError::LeaseLost);
        }
        if let Some(j) = &mut self.parallel {
            j.steps[claim.step].lease_until = Some(now);
            j.recover(now);
            return self.sync_parallel(now, true);
        }
        self.lease_until = Some(now);
        self.recover(now)?;
        Ok(())
    }
    pub fn block(&mut self, code: &str, now: DateTime<Utc>) {
        self.run.status = RunStatus::Blocked;
        self.run.error_code = Some(ActivityError::permanent(code).code());
        self.lease_until = None;
        self.run.next_attempt_at = None;
        self.touch(now);
    }
    fn schedule(&mut self, at: DateTime<Utc>) -> Result<(), DomainError> {
        self.delivery_generation = self
            .delivery_generation
            .checked_add(1)
            .ok_or(DomainError::Internal("delivery generation overflow"))?;
        self.run.status = RunStatus::Queued;
        self.run.next_attempt_at = Some(at);
        Ok(())
    }
    fn touch(&mut self, now: DateTime<Utc>) {
        if self.parallel.is_none() {
            for activity in &mut self.run.activities {
                let Some(started_at) = activity.started_at else {
                    continue;
                };
                if activity.attempts == 0 {
                    continue;
                }
                if activity.status == ActivityStatus::Running
                    && activity
                        .attempt_history
                        .last()
                        .is_none_or(|a| a.number < activity.attempts)
                {
                    activity
                        .attempt_history
                        .push(crate::domain::persisted::ActivityAttempt {
                            number: activity.attempts,
                            epoch: self.run.execution_epoch,
                            fence: u64::try_from(self.fence).unwrap_or_default(),
                            started_at,
                            finished_at: None,
                            status: activity.status,
                            error_code: None,
                        });
                }
                if let Some(last) = activity
                    .attempt_history
                    .last_mut()
                    .filter(|a| a.finished_at.is_none())
                {
                    last.status = activity.status;
                    last.error_code.clone_from(&activity.error_code);
                    last.finished_at = activity.finished_at;
                    if activity.status == ActivityStatus::Pending {
                        last.finished_at = Some(now);
                        last.error_code = Some("lease_expired".into());
                    }
                }
            }
        }
        if self.run.status.is_terminal()
            && !self
                .run
                .activities
                .iter()
                .any(|a| a.status == ActivityStatus::Running)
        {
            self.completed_at.get_or_insert(now);
        } else {
            self.completed_at = None;
        }
        self.run.updated_at = now;
    }
}
fn parallel_claim(claim: Claim) -> Result<crate::domain::parallel::Claim, DomainError> {
    Ok(crate::domain::parallel::Claim {
        step: claim.step,
        fence: u64::try_from(claim.fence)
            .map_err(|_| DomainError::Internal("fence out of range"))?,
    })
}

impl Journal {
    fn claim_parallel(
        &mut self,
        definition: &ExecutionContract,
        generation: i64,
        now: DateTime<Utc>,
        lease: Duration,
    ) -> Result<Option<Claim>, DomainError> {
        if self.run.status.is_terminal()
            || self.run.status == RunStatus::Blocked
            || self.cancellation_requested
            || generation != self.delivery_generation
        {
            return Ok(None);
        }
        if self.fingerprint != definition.fingerprint()? {
            // Never revoke a live sibling's ownership due to a different worker's
            // incompatible binary. It may still finish using the correct definition.
            if self.parallel.as_ref().is_some_and(|j| !j.settled()) {
                return Ok(None);
            }
            self.block("definition_mismatch", now);
            return Ok(None);
        }
        let Some(at) = self.run.next_attempt_at.filter(|at| *at <= now) else {
            return Ok(None);
        };
        let j = self
            .parallel
            .as_mut()
            .ok_or(DomainError::Internal("parallel journal missing"))?;
        let Some(step) = j.ready(at).first().copied() else {
            return Ok(None);
        };
        if j.steps[step].budget_attempts >= definition.activities[step].retry.max_attempts {
            j.steps[step].status = ActivityStatus::Failed;
            j.steps[step].error = Some("attempts_exhausted".into());
            self.sync_parallel(now, true)?;
            return Ok(None);
        }
        let claim = j.claim(step, now, lease)?;
        let fence =
            i64::try_from(claim.fence).map_err(|_| DomainError::Internal("fence out of range"))?;
        // Atomically schedule the next eligible sibling with this claim.
        self.sync_parallel(now, true)?;
        Ok(Some(Claim { fence, step }))
    }

    fn sync_parallel(&mut self, now: DateTime<Utc>, schedule: bool) -> Result<(), DomainError> {
        let j = self
            .parallel
            .as_ref()
            .ok_or(DomainError::Internal("parallel journal missing"))?;
        project_parallel(&mut self.run, &mut self.lease_until, j, now);
        if schedule && self.run.next_attempt_at.is_some() {
            self.delivery_generation = self
                .delivery_generation
                .checked_add(1)
                .ok_or(DomainError::Internal("delivery generation overflow"))?;
        }
        self.touch(now);
        Ok(())
    }
}
fn project_parallel(
    run: &mut Run,
    lease_until: &mut Option<DateTime<Utc>>,
    j: &crate::domain::parallel::StageJournal,
    now: DateTime<Utc>,
) {
    for (out, step) in run.activities.iter_mut().zip(&j.steps) {
        out.status = step.status;
        out.attempts = step.attempts;
        out.attempt_history = step
            .history
            .iter()
            .map(|a| crate::domain::persisted::ActivityAttempt {
                number: a.number,
                epoch: a.epoch,
                fence: a.fence,
                started_at: a.started_at,
                finished_at: a.finished_at,
                status: a.status,
                error_code: a.error.clone(),
            })
            .collect();
        out.budget_attempts = step.budget_attempts;
        out.result.clone_from(&step.result);
        out.error_code.clone_from(&step.error);
        let current_attempt = step.history.iter().rev().find(|a| a.epoch == j.epoch);
        out.started_at = current_attempt.map(|a| a.started_at);
        out.finished_at = current_attempt.and_then(|a| a.finished_at);
    }
    *lease_until = j.steps.iter().filter_map(|s| s.lease_until).min();
    let stage = j
        .steps
        .iter()
        .find(|s| s.status != ActivityStatus::Succeeded)
        .map(|s| s.stage);
    run.next_attempt_at = if j.stop_requested {
        None
    } else {
        j.steps
            .iter()
            .filter(|s| {
                Some(s.stage) == stage
                    && matches!(
                        s.status,
                        ActivityStatus::Pending | ActivityStatus::RetryWait
                    )
            })
            .map(|s| s.due_at)
            .min()
    };
    run.error_code = j
        .steps
        .iter()
        .find(|s| matches!(s.status, ActivityStatus::Failed | ActivityStatus::RetryWait))
        .and_then(|s| s.error.clone());
    run.status = if !j.settled() {
        if j.stop_requested {
            RunStatus::Cancelling
        } else {
            RunStatus::Running
        }
    } else if j.succeeded() {
        RunStatus::Succeeded
    } else if j.stop_requested {
        RunStatus::Cancelled
    } else if let Some(at) = run.next_attempt_at {
        if at > now {
            RunStatus::RetryWait
        } else {
            RunStatus::Queued
        }
    } else {
        RunStatus::Failed
    };
}

fn add_duration(now: DateTime<Utc>, duration: Duration) -> Result<DateTime<Utc>, DomainError> {
    chrono::Duration::from_std(duration)
        .ok()
        .and_then(|d| now.checked_add_signed(d))
        .ok_or(DomainError::Internal("duration out of range"))
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "../../tests/unit/journal_tests.rs"]
mod tests;
