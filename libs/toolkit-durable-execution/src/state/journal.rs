//! Deterministic run transitions. The repository persists every transition with
//! compare-and-swap; neither delivery nor a stale worker can override a checkpoint.
use crate::contracts::ExecutionContract;
use crate::error::domain::DomainError;
use crate::state::persisted::{ActivityStatus, ExecutionSnapshot, Run, RunStatus};
use crate::{ActivityError, ExecutionOwner, RunId};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::time::Duration;

/// Persisted run aggregate: execution state, public run view and delivery bookkeeping.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Journal {
    /// Execution engine state for stages, instances and reversal.
    pub execution: super::execution::ExecutionState,
    /// Number of run events committed so far; used as the observation cursor.
    pub event_sequence: u64,
    /// Public run record projected from the execution state.
    pub run: Run,
    /// Input the run was started with.
    pub input: Value,
    /// Time the run reached a terminal status with no running activity; `None` otherwise.
    #[serde(default)]
    pub completed_at: Option<DateTime<Utc>>,
    /// Registration generation whose handler bindings may execute the run.
    #[serde(default)]
    pub registration_generation: u64,
    #[cfg(test)]
    #[serde(skip)]
    pub(crate) registration_contract: Option<ExecutionContract>,
    /// Fingerprint of the contract the run was started with.
    pub fingerprint: String,
    /// Optimistic concurrency revision checked by the repository on every save.
    pub revision: i64,
    /// Earliest lease expiry among running instances; `None` when nothing is running.
    pub lease_until: Option<DateTime<Utc>>,
    /// Delivery counter bumped whenever new work is scheduled; stale deliveries are ignored.
    pub delivery_generation: i64,
    /// `true` while a pause or stop is pending for in-flight work.
    pub cancellation_requested: bool,
}

/// Fenced claim on one activity instance of a run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Claim {
    /// Fence issued when the instance was claimed.
    pub fence: i64,
    /// Index of the claimed instance in `ExecutionState::instances`.
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
    /// Returns keys of routable activity instances, or none when no attempt is scheduled.
    pub fn delivery_activities(&self) -> Vec<String> {
        if self.run.next_attempt_at.is_none() {
            return vec![];
        }
        let e = &self.execution;
        e.routable()
            .into_iter()
            .map(|index| e.instances[index].identity.key())
            .collect()
    }
    /// Returns keys of activity instances to publish for the scheduled attempt time.
    pub fn publication_activities(&self) -> Vec<String> {
        let Some(at) = self.run.next_attempt_at else {
            return vec![];
        };
        let e = &self.execution;
        e.publication_candidates(at)
            .into_iter()
            .map(|index| e.instances[index].identity.key())
            .collect()
    }
    /// Returns the due time of the activity instance with key `activity`, if it exists.
    pub fn delivery_due(&self, activity: &str) -> Option<DateTime<Utc>> {
        self.execution
            .instances
            .iter()
            .find(|i| i.identity.key() == activity)
            .map(|i| i.due_at)
    }
    /// The selected activity is stable for a delivery generation even if the
    /// queue wakes late and other retry deadlines have elapsed meanwhile.
    #[cfg(test)]
    #[must_use]
    pub fn delivery_activity(&self) -> Option<String> {
        let at = self.run.next_attempt_at?;
        let e = &self.execution;
        e.ready(at)
            .first()
            .map(|i| e.instances[*i].state.id.0.clone())
    }
    /// Creates a queued run journal for `definition` and materializes its first stage.
    pub fn new(
        id: RunId,
        owner: ExecutionOwner,
        definition: &ExecutionContract,
        input: Value,
        now: DateTime<Utc>,
    ) -> Result<Self, DomainError> {
        let execution = super::execution::ExecutionState::new(definition.clone(), &input, now)?;
        let mut journal = Self {
            execution,
            event_sequence: 0,
            run: Run {
                id,
                definition: definition.name.clone(),
                owner,
                status: RunStatus::Queued,
                activities: Vec::new(),
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
            lease_until: None,
            delivery_generation: 0,
            cancellation_requested: false,
        };
        journal.sync_execution(now, false)?;
        Ok(journal)
    }
    #[cfg(test)]
    pub fn claim(
        &mut self,
        definition: &ExecutionContract,
        generation: i64,
        now: DateTime<Utc>,
        lease: Duration,
    ) -> Result<Option<Claim>, DomainError> {
        self.claim_target(definition, generation, None, now, lease)
    }
    /// Claims the ready instance, or the instance keyed `target`, for delivery `generation`.
    /// Returns `None` for a blocked run, a stale generation or nothing to claim; blocks the run on
    /// a fingerprint mismatch.
    pub fn claim_target(
        &mut self,
        definition: &ExecutionContract,
        generation: i64,
        target: Option<&str>,
        now: DateTime<Utc>,
        lease: Duration,
    ) -> Result<Option<Claim>, DomainError> {
        if self.run.status == RunStatus::Blocked || generation != self.delivery_generation {
            return Ok(None);
        }
        if self.fingerprint != definition.fingerprint()? {
            self.block("definition_mismatch", now);
            return Ok(None);
        }
        let e = &mut self.execution;
        let selected = target.map_or_else(
            || e.ready(now).first().copied(),
            |id| {
                e.instances
                    .iter()
                    .position(|i| i.identity.key() == id)
                    .filter(|index| e.can_claim(*index, now))
            },
        );
        let Some(step) = selected else {
            return Ok(None);
        };
        let fence = e.claim(step, now, lease)?;
        self.sync_execution(now, true)?;
        Ok(Some(Claim { fence, step }))
    }
    /// Returns `true` when `claim` still holds an unexpired lease at `now`.
    #[must_use]
    pub fn owns(&self, claim: Claim, now: DateTime<Utc>) -> bool {
        self.execution.owns(claim.step, claim.fence, now)
    }
    /// Extends the lease of `claim`; returns `false` when the claim is no longer owned.
    pub fn heartbeat(
        &mut self,
        claim: Claim,
        now: DateTime<Utc>,
        lease: Duration,
    ) -> Result<bool, DomainError> {
        let changed = self
            .execution
            .heartbeat(claim.step, claim.fence, now, lease)?;
        if changed {
            self.sync_execution(now, false)?;
        }
        Ok(changed)
    }
    /// Checkpoint + scheduling are persisted together by the journal repository.
    pub fn complete(
        &mut self,
        claim: Claim,
        result: Value,
        now: DateTime<Utc>,
    ) -> Result<(), DomainError> {
        self.execution
            .finish(claim.step, claim.fence, Ok(result), &self.input, now, 0)?;
        self.sync_execution(now, true)
    }
    /// Records a failed attempt for `claim`, scheduling a retry with `jitter` or stopping the run.
    pub fn fail(
        &mut self,
        claim: Claim,
        error: &ActivityError,
        now: DateTime<Utc>,
        jitter: u16,
    ) -> Result<(), DomainError> {
        self.execution.finish(
            claim.step,
            claim.fence,
            Err(error.clone()),
            &self.input,
            now,
            jitter,
        )?;
        self.sync_execution(now, true)
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
        if self.execution.is_compensation() {
            return Err(DomainError::InvalidState(
                "compensation cannot be cancelled",
            ));
        }
        if self.run.status.is_terminal() || self.cancellation_requested {
            return Ok(false);
        }
        self.execution.pause()?;
        self.run.stop_reason = reason
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|r| r.chars().take(240).collect());
        self.sync_execution(now, false)?;
        Ok(true)
    }
    /// Pause the current epoch without a reason.
    #[cfg(test)]
    pub fn request_cancel(&mut self, now: DateTime<Utc>) {
        let epoch = self.run.execution_epoch;
        self.request_cancel_at(epoch, now, None).ok();
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
        if self.fingerprint != definition.fingerprint()? {
            return Err(DomainError::DefinitionMismatch);
        }
        if self.run.execution_epoch
            == expected_epoch
                .checked_add(1)
                .ok_or(DomainError::Internal("epoch overflow"))?
            && self
                .run
                .previous_executions
                .last()
                .is_some_and(|s| s.epoch == expected_epoch)
        {
            return Ok(false);
        }
        // A rejected continuation must leave the journal untouched, so check the
        // delivery counter and take the snapshot before changing anything.
        self.delivery_generation
            .checked_add(1)
            .ok_or(DomainError::Internal("delivery generation overflow"))?;
        let snapshot = self.epoch_snapshot(self.run.status);
        let changed = self.execution.continue_run(
            expected_epoch,
            from == RunStatus::Cancelled,
            &self.input,
            now,
        )?;
        if changed {
            // The stop sentence belongs to the archived epoch, not the next one.
            self.run.stop_reason = None;
            self.run.previous_executions.push(snapshot);
            self.run.execution_epoch = expected_epoch + 1;
            self.sync_execution(now, true)?;
        }
        Ok(changed)
    }
    /// Snapshot of the current epoch, including its stop sentence.
    fn epoch_snapshot(&self, status: RunStatus) -> ExecutionSnapshot {
        ExecutionSnapshot {
            compensation_cause: self.execution.cause().cloned(),
            error_code: self.run.error_code.clone(),
            instances: self
                .execution
                .instances
                .iter()
                .map(|i| {
                    (
                        i.identity.clone(),
                        u32::try_from(i.stage).unwrap_or(u32::MAX),
                    )
                })
                .collect(),
            completed_at: self.completed_at,
            epoch: self.run.execution_epoch,
            status,
            ended_at: self.run.updated_at,
            activities: self.run.activities.clone(),
            stop_reason: self.run.stop_reason.clone(),
        }
    }
    /// Returns running instances with expired leases to pending; returns `true` when anything
    /// changed.
    pub fn recover(&mut self, now: DateTime<Utc>) -> Result<bool, DomainError> {
        let changed = self.execution.recover(&self.input, now)?;
        if changed {
            self.sync_execution(now, true)?;
        }
        Ok(changed)
    }
    /// Graceful shutdown leaves the run runnable, not user-cancelled.
    pub fn release(&mut self, claim: Claim, now: DateTime<Utc>) -> Result<(), DomainError> {
        self.execution
            .release(claim.step, claim.fence, now, &self.input)?;
        self.sync_execution(now, true)
    }
    /// Aborts the run for `expected_epoch`, storing a reason truncated to 240 characters.
    /// Returns `false` when already stopped or terminal; fails with `ConcurrentUpdate` for a stale
    /// epoch.
    pub fn request_abort_at(
        &mut self,
        expected_epoch: u64,
        now: DateTime<Utc>,
        reason: Option<&str>,
    ) -> Result<bool, DomainError> {
        if self.run.execution_epoch != expected_epoch {
            return Err(DomainError::ConcurrentUpdate);
        }
        let changed = self.execution.abort(
            reason.map(|s| s.chars().take(240).collect()),
            &self.input,
            now,
        )?;
        if changed {
            self.sync_execution(now, true)?;
        }
        Ok(changed)
    }
    fn sync_execution(&mut self, now: DateTime<Utc>, schedule: bool) -> Result<(), DomainError> {
        let e = &self.execution;
        self.run.activities = e.instances.iter().map(|i| i.state.clone()).collect();
        self.run.status = e.status(now);
        self.run.next_attempt_at = e.next_due();
        self.run.error_code = e
            .instances
            .iter()
            .rev()
            .find_map(|i| i.state.error_code.clone())
            .or_else(|| e.stage_error.clone());
        self.cancellation_requested = e.cancellation_requested();
        self.lease_until = e.instances.iter().filter_map(|i| i.lease_until).min();
        if schedule && self.run.next_attempt_at.is_some() {
            self.delivery_generation = self
                .delivery_generation
                .checked_add(1)
                .ok_or(DomainError::Internal("delivery generation overflow"))?;
        }
        self.touch(now);
        Ok(())
    }
    /// Marks the run blocked with error `code` and clears its schedule and lease.
    pub fn block(&mut self, code: &str, now: DateTime<Utc>) {
        self.run.status = RunStatus::Blocked;
        self.run.error_code = Some(ActivityError::permanent(code).code().to_owned());
        self.lease_until = None;
        self.run.next_attempt_at = None;
        self.touch(now);
    }
    fn touch(&mut self, now: DateTime<Utc>) {
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

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "../../tests/unit/journal_tests.rs"]
mod tests;
