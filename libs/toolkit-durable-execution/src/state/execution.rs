//! Persisted stage expansion and reverse execution. All mutations commit through journal CAS.
use super::persisted::{ActivityAttempt, ActivityRun, ActivityStatus, RunStatus};
use crate::contracts::{ExecutionContract, InputSource};
use crate::error::domain::DomainError;
use crate::{ActivityError, ActivityId, ActivityInstanceId, ExecutionPhase};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet},
    time::Duration,
};

/// Original failure or explicit termination, independent of compensation failures.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Cause {
    /// Forward step failure identified by its error code.
    Failure(String),
    /// Explicit abort with an optional caller-supplied reason.
    Abort(Option<String>),
}
#[derive(Clone, Debug, Serialize, Deserialize)]
enum Phase {
    Forward,
    Stopping(Cause),
    Reversing(Cause),
    ReverseFailed(Cause),
    Reversed(Cause),
}
/// A durable instance owns its exact selected input and an independent claim.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Instance {
    /// Step, branch index and phase that identify this instance.
    pub identity: ActivityInstanceId,
    /// Zero-based stage the instance belongs to.
    pub stage: usize,
    /// Index of the activity template in `contract.activities`.
    pub template: usize,
    /// Exact input selected for this instance when it was materialized.
    pub input: Value,
    /// Forward result handed to a compensation instance; `None` for forward instances.
    pub receipt: Option<Value>,
    /// Persisted status, attempts and checkpoint of this instance.
    pub state: ActivityRun,
    /// Claim fence, incremented on every claim; stale fences cannot finish or heartbeat.
    pub fence: i64,
    /// Expiry of the current claim; `None` when the instance is not running.
    pub lease_until: Option<DateTime<Utc>>,
    /// Earliest time the instance may be claimed, advanced by retry backoff.
    pub due_at: DateTime<Utc>,
}
/// Unified execution data for static stages, per-run expansion and compensation.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ExecutionState {
    /// Contract that defines the stages, templates and retry policies of the run.
    pub contract: ExecutionContract,
    /// Materialized forward and compensation instances in creation order.
    pub instances: Vec<Instance>,
    /// Error code that stopped stage expansion, such as `fanout_limit_exceeded`.
    pub stage_error: Option<String>,
    stages: Vec<Vec<String>>,
    materialized: BTreeSet<usize>,
    completed: BTreeSet<usize>,
    phase: Phase,
    paused: bool,
    epoch: u64,
}
impl ExecutionState {
    /// Creates the state for a new run and materializes the first stage from `input`.
    pub fn new(
        contract: ExecutionContract,
        input: &Value,
        now: DateTime<Utc>,
    ) -> Result<Self, DomainError> {
        let stages = contract.stages()?;
        let mut state = Self {
            contract,
            instances: vec![],
            stage_error: None,
            stages,
            materialized: BTreeSet::new(),
            completed: BTreeSet::new(),
            phase: Phase::Forward,
            paused: false,
            epoch: 0,
        };
        state.advance(input, now)?;
        Ok(state)
    }
    /// Returns a copy without inputs, results, receipts and attempt history for the metadata
    /// header.
    pub fn control_metadata(&self) -> Self {
        Self {
            contract: self.contract.clone(),
            instances: self
                .instances
                .iter()
                .map(|i| Instance {
                    identity: i.identity.clone(),
                    stage: i.stage,
                    template: i.template,
                    input: Value::Null,
                    receipt: None,
                    state: ActivityRun {
                        id: i.state.id.clone(),
                        status: i.state.status,
                        attempts: i.state.attempts,
                        attempt_history: vec![],
                        budget_attempts: i.state.budget_attempts,
                        result: None,
                        error_code: i.state.error_code.clone(),
                        started_at: i.state.started_at,
                        finished_at: i.state.finished_at,
                    },
                    fence: i.fence,
                    lease_until: i.lease_until,
                    due_at: i.due_at,
                })
                .collect(),
            stage_error: self.stage_error.clone(),
            stages: self.stages.clone(),
            materialized: self.materialized.clone(),
            completed: self.completed.clone(),
            phase: self.phase.clone(),
            paused: self.paused,
            epoch: self.epoch,
        }
    }
    /// Returns `true` once the run has entered saga reversal.
    pub fn is_compensation(&self) -> bool {
        matches!(
            self.phase,
            Phase::Reversing(_) | Phase::ReverseFailed(_) | Phase::Reversed(_)
        )
    }
    /// Returns the failure or abort that stopped forward execution, or `None` while forward.
    pub fn cause(&self) -> Option<&Cause> {
        match &self.phase {
            Phase::Forward => None,
            Phase::Stopping(c)
            | Phase::Reversing(c)
            | Phase::ReverseFailed(c)
            | Phase::Reversed(c) => Some(c),
        }
    }
    /// Candidate delivery identities do not imply that a concurrency slot is available.
    pub fn routable(&self) -> Vec<usize> {
        if self.paused {
            return vec![];
        }
        match self.phase {
            Phase::Forward if self.stage_error.is_none() => {
                let Some(stage) =
                    (0..self.stages.len()).find(|stage| !self.completed.contains(stage))
                else {
                    return vec![];
                };
                self.instances
                    .iter()
                    .enumerate()
                    .filter(|(_, i)| {
                        i.stage == stage
                            && i.identity.phase == ExecutionPhase::Forward
                            && matches!(
                                i.state.status,
                                ActivityStatus::Pending | ActivityStatus::RetryWait
                            )
                    })
                    .map(|(index, _)| index)
                    .collect()
            }
            Phase::Reversing(_) => self
                .instances
                .iter()
                .enumerate()
                .find(|(_, i)| {
                    i.identity.phase == ExecutionPhase::Compensation
                        && i.state.status != ActivityStatus::Succeeded
                })
                .filter(|(_, i)| {
                    matches!(
                        i.state.status,
                        ActivityStatus::Pending | ActivityStatus::RetryWait
                    )
                })
                .map(|(index, _)| vec![index])
                .unwrap_or_default(),
            _ => vec![],
        }
    }
    fn available_slots(&self, index: usize) -> usize {
        let instance = &self.instances[index];
        if instance.identity.phase == ExecutionPhase::Compensation {
            return 1;
        }
        let limit = self.contract.activities[instance.template]
            .fanout
            .as_ref()
            .map_or(128, |options| options.concurrency);
        let running = self
            .instances
            .iter()
            .filter(|i| i.stage == instance.stage && i.state.status == ActivityStatus::Running)
            .count();
        limit.saturating_sub(running)
    }
    /// Returns routable instances that are due at `now`, limited by free concurrency slots.
    pub fn ready(&self, now: DateTime<Utc>) -> Vec<usize> {
        let candidates = self.routable();
        let slots = candidates
            .first()
            .map_or(0, |index| self.available_slots(*index));
        candidates
            .into_iter()
            .filter(|index| self.instances[*index].due_at <= now)
            .take(slots)
            .collect()
    }
    /// Returns `true` when the instance at `index` is routable, due and has a free slot.
    pub fn can_claim(&self, index: usize, now: DateTime<Utc>) -> bool {
        self.routable().contains(&index)
            && self.instances[index].due_at <= now
            && self.available_slots(index) > 0
    }
    /// Returns instances to publish for delivery: all candidates of a fresh stage, otherwise those
    /// ready at `at`.
    pub fn publication_candidates(&self, at: DateTime<Utc>) -> Vec<usize> {
        let candidates = self.routable();
        let fresh = candidates.first().is_some_and(|index| {
            let stage = self.instances[*index].stage;
            self.instances
                .iter()
                .filter(|i| i.stage == stage && i.identity.phase == ExecutionPhase::Forward)
                .all(|i| i.state.attempts == 0)
        });
        if fresh { candidates } else { self.ready(at) }
    }
    /// Returns the earliest due time of a pending or retrying instance that can become ready, if
    /// any.
    pub fn next_due(&self) -> Option<DateTime<Utc>> {
        self.instances
            .iter()
            .filter(|i| {
                matches!(
                    i.state.status,
                    ActivityStatus::Pending | ActivityStatus::RetryWait
                )
            })
            .map(|i| i.due_at)
            .filter(|at| !self.ready(*at).is_empty())
            .min()
    }
    /// Returns `true` when `fence` holds an unexpired claim on the running instance at `index`.
    pub fn owns(&self, index: usize, fence: i64, now: DateTime<Utc>) -> bool {
        self.instances.get(index).is_some_and(|i| {
            i.fence == fence
                && i.state.status == ActivityStatus::Running
                && i.lease_until.is_some_and(|t| t > now)
        })
    }
    /// Claims the instance at `index` for `lease`, recording a new attempt, and returns its new
    /// fence.
    /// Fails with `InvalidState` when the instance is not eligible.
    pub fn claim(
        &mut self,
        index: usize,
        now: DateTime<Utc>,
        lease: Duration,
    ) -> Result<i64, DomainError> {
        if !self.can_claim(index, now) {
            return Err(DomainError::InvalidState("instance is not eligible"));
        }
        let i = &mut self.instances[index];
        i.fence = i
            .fence
            .checked_add(1)
            .ok_or(DomainError::Internal("fence overflow"))?;
        i.lease_until = Some(deadline(now, lease)?);
        i.state.status = ActivityStatus::Running;
        i.state.attempts = i
            .state
            .attempts
            .checked_add(1)
            .ok_or(DomainError::Internal("attempt overflow"))?;
        i.state.budget_attempts = i
            .state
            .budget_attempts
            .checked_add(1)
            .ok_or(DomainError::Internal("attempt overflow"))?;
        i.state.started_at = Some(now);
        i.state.finished_at = None;
        i.state.error_code = None;
        i.state.attempt_history.push(ActivityAttempt {
            number: i.state.attempts,
            epoch: self.epoch,
            fence: u64::try_from(i.fence)
                .map_err(|_| DomainError::Internal("fence out of range"))?,
            started_at: now,
            finished_at: None,
            status: ActivityStatus::Running,
            error_code: None,
        });
        Ok(i.fence)
    }
    /// Extends the claim lease; returns `false` when `fence` no longer owns the instance.
    pub fn heartbeat(
        &mut self,
        index: usize,
        fence: i64,
        now: DateTime<Utc>,
        lease: Duration,
    ) -> Result<bool, DomainError> {
        if !self.owns(index, fence, now) {
            return Ok(false);
        }
        self.instances[index].lease_until = Some(deadline(now, lease)?);
        Ok(true)
    }
    /// Records the outcome of a claimed attempt, schedules a retry or failure, and advances the
    /// run.
    /// Fails with `LeaseLost` when `fence` no longer owns the instance.
    pub fn finish(
        &mut self,
        index: usize,
        fence: i64,
        result: Result<Value, ActivityError>,
        root: &Value,
        now: DateTime<Utc>,
        jitter: u16,
    ) -> Result<(), DomainError> {
        if !self.owns(index, fence, now) {
            return Err(DomainError::LeaseLost);
        }
        let template = &self.contract.activities[self.instances[index].template];
        let policy = if self.instances[index].identity.phase == ExecutionPhase::Compensation {
            &template
                .compensation
                .as_ref()
                .ok_or(DomainError::Internal("compensator policy missing"))?
                .retry
        } else {
            &template.retry
        };
        let i = &mut self.instances[index];
        match result {
            Ok(value) => {
                i.state.result = Some(value);
                finish(i, ActivityStatus::Succeeded, None, now);
            }
            Err(error) => {
                let code = error.code().to_owned();
                if self.paused || matches!(self.phase, Phase::Stopping(_)) {
                    finish(i, ActivityStatus::Cancelled, Some(code), now);
                } else if let ActivityError::Retryable { retry_after, .. } = &error
                    && i.state.budget_attempts < policy.max_attempts
                {
                    i.due_at = deadline(
                        now,
                        policy.delay(i.state.budget_attempts, *retry_after, jitter),
                    )?;
                    finish(i, ActivityStatus::RetryWait, Some(code), now);
                } else {
                    finish(i, ActivityStatus::Failed, Some(code.clone()), now);
                    if let Phase::Reversing(cause) = &self.phase {
                        self.phase = Phase::ReverseFailed(cause.clone());
                    } else if self.contract.saga {
                        self.phase = Phase::Stopping(Cause::Failure(code));
                    }
                }
            }
        }
        self.advance(root, now)
    }
    /// Pauses forward execution; fails with `InvalidState` once stopping or compensating.
    pub fn pause(&mut self) -> Result<(), DomainError> {
        if self.is_compensation() || matches!(self.phase, Phase::Stopping(_)) {
            return Err(DomainError::InvalidState(
                "compensation cannot be paused by a business command",
            ));
        }
        self.paused = true;
        Ok(())
    }
    /// Stops the run with an abort cause and starts reversal; returns `false` when already stopped
    /// or terminal.
    pub fn abort(
        &mut self,
        reason: Option<String>,
        root: &Value,
        now: DateTime<Utc>,
    ) -> Result<bool, DomainError> {
        if self.cause().is_some() || self.status(now).is_terminal() && !self.paused {
            return Ok(false);
        }
        self.paused = false;
        self.phase = Phase::Stopping(Cause::Abort(reason));
        self.advance(root, now)?;
        Ok(true)
    }
    /// Starts the next epoch by resetting failed or cancelled work in the current phase.
    /// Fails with `ConcurrentUpdate` for a stale `expected_epoch` and `InvalidState` while running
    /// or from a final phase.
    pub fn continue_run(
        &mut self,
        expected_epoch: u64,
        resume: bool,
        root: &Value,
        now: DateTime<Utc>,
    ) -> Result<bool, DomainError> {
        if self.epoch != expected_epoch {
            return Err(DomainError::ConcurrentUpdate);
        }
        if self
            .instances
            .iter()
            .any(|i| i.state.status == ActivityStatus::Running)
        {
            return Err(DomainError::InvalidState("run is still executing"));
        }
        let reverse = matches!(self.phase, Phase::ReverseFailed(_));
        if reverse && resume
            || !reverse
                && (resume != self.paused || !resume && self.status(now) != RunStatus::Failed)
            || matches!(self.phase, Phase::Reversed(_))
        {
            return Err(DomainError::InvalidState(
                "run cannot continue from its current phase",
            ));
        }
        if self.stage_error.is_some() && !reverse {
            return Err(DomainError::InvalidState(
                "fan-out limits require a new run",
            ));
        }
        self.epoch = self
            .epoch
            .checked_add(1)
            .ok_or(DomainError::Internal("epoch overflow"))?;
        if let Phase::ReverseFailed(c) = &self.phase {
            self.phase = Phase::Reversing(c.clone());
        }
        self.paused = false;
        for i in &mut self.instances {
            if (i.identity.phase == ExecutionPhase::Compensation) == reverse
                && matches!(
                    i.state.status,
                    ActivityStatus::Failed | ActivityStatus::Cancelled
                )
            {
                i.state.status = ActivityStatus::Pending;
                i.state.budget_attempts = 0;
                i.state.error_code = None;
                i.due_at = now;
            }
        }
        self.advance(root, now)?;
        Ok(true)
    }
    /// Returns a claimed instance to pending without failing it; fails with `LeaseLost` for a stale
    /// fence.
    pub fn release(
        &mut self,
        index: usize,
        fence: i64,
        now: DateTime<Utc>,
        root: &Value,
    ) -> Result<(), DomainError> {
        if !self.owns(index, fence, now) {
            return Err(DomainError::LeaseLost);
        }
        let i = &mut self.instances[index];
        finish(i, ActivityStatus::Pending, Some("interrupted".into()), now);
        i.due_at = now;
        self.advance(root, now)
    }
    /// Returns running instances with expired leases to pending; returns `true` when anything
    /// changed.
    pub fn recover(&mut self, root: &Value, now: DateTime<Utc>) -> Result<bool, DomainError> {
        let mut changed = false;
        for i in &mut self.instances {
            if i.state.status == ActivityStatus::Running && i.lease_until.is_none_or(|t| t <= now) {
                finish(
                    i,
                    ActivityStatus::Pending,
                    Some("lease_expired".into()),
                    now,
                );
                i.due_at = now;
                changed = true;
            }
        }
        if changed {
            self.advance(root, now)?;
        }
        Ok(changed)
    }
    /// Returns `true` while a pause or stop is pending for in-flight work.
    pub fn cancellation_requested(&self) -> bool {
        self.paused || matches!(self.phase, Phase::Stopping(_))
    }
    /// Derives the run status from the phase, pause flag and instance states at `now`.
    pub fn status(&self, now: DateTime<Utc>) -> RunStatus {
        match &self.phase {
            Phase::Reversing(_) => RunStatus::Compensating,
            Phase::ReverseFailed(_) => RunStatus::CompensationFailed,
            Phase::Reversed(_) => RunStatus::Compensated,
            Phase::Stopping(_) => RunStatus::Cancelling,
            Phase::Forward => {
                let running = self
                    .instances
                    .iter()
                    .any(|i| i.state.status == ActivityStatus::Running);
                // A confirmed final checkpoint wins over a pause that raced with it.
                if self.completed.len() == self.stages.len() {
                    return RunStatus::Succeeded;
                }
                if self.paused {
                    return if running {
                        RunStatus::Cancelling
                    } else {
                        RunStatus::Cancelled
                    };
                }
                if running {
                    RunStatus::Running
                } else if let Some(at) = self.next_due() {
                    if at > now {
                        RunStatus::RetryWait
                    } else {
                        RunStatus::Queued
                    }
                } else {
                    RunStatus::Failed
                }
            }
        }
    }
    /// Returns successful forward results by step id; fan-out steps appear only once their stage
    /// completes.
    pub fn checkpoints(&self) -> BTreeMap<String, Value> {
        let mut result = BTreeMap::new();
        for (stage, ids) in self.stages.iter().enumerate() {
            for id in ids {
                let values: Vec<_> = self
                    .instances
                    .iter()
                    .filter(|i| {
                        i.stage == stage
                            && i.identity.phase == ExecutionPhase::Forward
                            && i.identity.step.0 == *id
                            && i.state.status == ActivityStatus::Succeeded
                    })
                    .filter_map(|i| i.state.result.clone())
                    .collect();
                let fanout = self
                    .contract
                    .activities
                    .iter()
                    .find(|a| a.id.0 == *id)
                    .is_some_and(|a| a.fanout.is_some());
                if fanout {
                    if self.completed.contains(&stage) {
                        result.insert(id.clone(), Value::Array(values));
                    }
                } else if let Some(value) = values.into_iter().next() {
                    result.insert(id.clone(), value);
                }
            }
        }
        result
    }
    /// Resolves an input source against the run input and current checkpoints.
    pub fn read(&self, source: &InputSource, root: &Value) -> Result<Value, DomainError> {
        if let InputSource::FanOutBranch { step, index } = source {
            return self
                .instances
                .iter()
                .find(|i| {
                    i.identity.phase == ExecutionPhase::Forward
                        && i.identity.step.0 == *step
                        && i.identity.item_index == Some(*index)
                        && i.state.status == ActivityStatus::Succeeded
                })
                .and_then(|i| i.state.result.clone())
                .ok_or(DomainError::InvalidState("fan-out checkpoint is not ready"));
        }
        read(source, root, &self.checkpoints())
    }
    fn reject_expansion(
        &mut self,
        stage: usize,
        root: &Value,
        now: DateTime<Utc>,
    ) -> Result<(), DomainError> {
        self.stage_error = Some("fanout_limit_exceeded".into());
        self.materialized.insert(stage);
        if self.contract.saga {
            self.phase = Phase::Stopping(Cause::Failure("fanout_limit_exceeded".into()));
            self.advance(root, now)
        } else {
            Ok(())
        }
    }
    fn advance(&mut self, root: &Value, now: DateTime<Utc>) -> Result<(), DomainError> {
        // A lost lease consumes its attempt budget just like a returned failure.
        // Exhaustion must be persisted before any subsequent handler can run.
        for i in &mut self.instances {
            let template = &self.contract.activities[i.template];
            let policy = if i.identity.phase == ExecutionPhase::Compensation {
                &template
                    .compensation
                    .as_ref()
                    .ok_or(DomainError::Internal("compensator policy missing"))?
                    .retry
            } else {
                &template.retry
            };
            if i.state.status == ActivityStatus::Pending
                && i.state.budget_attempts >= policy.max_attempts
            {
                finish(
                    i,
                    ActivityStatus::Failed,
                    Some("attempts_exhausted".into()),
                    now,
                );
                if let Phase::Reversing(cause) = &self.phase {
                    self.phase = Phase::ReverseFailed(cause.clone());
                } else if self.contract.saga && matches!(self.phase, Phase::Forward) {
                    self.phase = Phase::Stopping(Cause::Failure("attempts_exhausted".into()));
                }
            }
        }
        if let Phase::Stopping(cause) = &self.phase {
            if self
                .instances
                .iter()
                .any(|i| i.state.status == ActivityStatus::Running)
            {
                return Ok(());
            }
            let cause = cause.clone();
            let reversals: Vec<_> = self
                .instances
                .iter()
                .rev()
                .filter(|i| {
                    i.identity.phase == ExecutionPhase::Forward
                        && i.state.status == ActivityStatus::Succeeded
                        && self.contract.activities[i.template].compensation.is_some()
                })
                .cloned()
                .collect();
            for i in reversals {
                let identity = ActivityInstanceId {
                    phase: ExecutionPhase::Compensation,
                    ..i.identity.clone()
                };
                self.instances.push(instance(
                    identity,
                    i.stage,
                    i.template,
                    i.input,
                    i.state.result,
                    now,
                ));
            }
            self.phase = Phase::Reversing(cause);
        }
        if let Phase::Reversing(cause) = &self.phase {
            if self
                .instances
                .iter()
                .filter(|i| i.identity.phase == ExecutionPhase::Compensation)
                .all(|i| i.state.status == ActivityStatus::Succeeded)
            {
                self.phase = Phase::Reversed(cause.clone());
            }
            return Ok(());
        }
        if !matches!(self.phase, Phase::Forward) || self.stage_error.is_some() {
            return Ok(());
        }
        for stage in 0..self.stages.len() {
            if self.completed.contains(&stage) {
                continue;
            }
            if !self.materialized.contains(&stage) {
                // A pause still records completed stages but starts no new one.
                if self.paused {
                    break;
                }
                let mut pending = Vec::new();
                for id in &self.stages[stage] {
                    let template = self
                        .contract
                        .activities
                        .iter()
                        .position(|a| a.id.0 == *id)
                        .ok_or(DomainError::Internal("stage template missing"))?;
                    let spec = &self.contract.activities[template];
                    let source = self
                        .contract
                        .flow
                        .inputs
                        .get(id)
                        .unwrap_or(&InputSource::RunInput);
                    let input = read(source, root, &self.checkpoints())?;
                    if let Some(options) = &spec.fanout {
                        let items = input
                            .as_array()
                            .ok_or(DomainError::InvalidState("fan-out input must be a list"))?;
                        if items.len() > options.max_items
                            || self
                                .instances
                                .iter()
                                .filter(|i| i.identity.phase == ExecutionPhase::Forward)
                                .count()
                                + pending.len()
                                + items.len()
                                > 4096
                        {
                            return self.reject_expansion(stage, root, now);
                        }
                        for (index, item) in items.iter().enumerate() {
                            pending.push(instance(
                                ActivityInstanceId {
                                    step: spec.id.clone(),
                                    item_index: Some(u32::try_from(index).map_err(|_| {
                                        DomainError::Internal("branch index overflow")
                                    })?),
                                    phase: ExecutionPhase::Forward,
                                },
                                stage,
                                template,
                                item.clone(),
                                None,
                                now,
                            ));
                        }
                    } else {
                        if self
                            .instances
                            .iter()
                            .filter(|i| i.identity.phase == ExecutionPhase::Forward)
                            .count()
                            + pending.len()
                            >= 4096
                        {
                            return self.reject_expansion(stage, root, now);
                        }
                        pending.push(instance(
                            ActivityInstanceId {
                                step: spec.id.clone(),
                                item_index: None,
                                phase: ExecutionPhase::Forward,
                            },
                            stage,
                            template,
                            input,
                            None,
                            now,
                        ));
                    }
                }
                self.instances.extend(pending);
                self.materialized.insert(stage);
            }
            if self
                .instances
                .iter()
                .filter(|i| i.stage == stage && i.identity.phase == ExecutionPhase::Forward)
                .all(|i| i.state.status == ActivityStatus::Succeeded)
            {
                self.completed.insert(stage);
            } else {
                break;
            }
        }
        Ok(())
    }
}
fn instance(
    identity: ActivityInstanceId,
    stage: usize,
    template: usize,
    input: Value,
    receipt: Option<Value>,
    now: DateTime<Utc>,
) -> Instance {
    let id = ActivityId(identity.key());
    Instance {
        identity,
        stage,
        template,
        input,
        receipt,
        state: ActivityRun {
            id,
            status: ActivityStatus::Pending,
            attempts: 0,
            attempt_history: vec![],
            budget_attempts: 0,
            result: None,
            error_code: None,
            started_at: None,
            finished_at: None,
        },
        fence: 0,
        lease_until: None,
        due_at: now,
    }
}
fn finish(i: &mut Instance, status: ActivityStatus, error: Option<String>, now: DateTime<Utc>) {
    i.state.status = status;
    i.state.error_code.clone_from(&error);
    i.state.finished_at = Some(now);
    i.lease_until = None;
    if let Some(a) = i.state.attempt_history.last_mut() {
        a.status = status;
        a.finished_at = Some(now);
        a.error_code = error;
    }
}
fn deadline(now: DateTime<Utc>, duration: Duration) -> Result<DateTime<Utc>, DomainError> {
    chrono::Duration::from_std(duration)
        .ok()
        .and_then(|d| now.checked_add_signed(d))
        .ok_or(DomainError::Internal("duration out of range"))
}
fn read(
    source: &InputSource,
    root: &Value,
    values: &BTreeMap<String, Value>,
) -> Result<Value, DomainError> {
    match source {
        InputSource::RunInput => Ok(root.clone()),
        InputSource::Checkpoint(id) | InputSource::FanOutResults(id) => values
            .get(id)
            .cloned()
            .ok_or(DomainError::InvalidState("checkpoint is not ready")),
        InputSource::FanOutBranch { step, index } => values
            .get(step)
            .and_then(Value::as_array)
            .and_then(|v| usize::try_from(*index).ok().and_then(|i| v.get(i)))
            .cloned()
            .ok_or(DomainError::InvalidState("branch is not ready")),
        InputSource::Tuple(items) | InputSource::List(items) => items
            .iter()
            .map(|s| read(s, root, values))
            .collect::<Result<Vec<_>, _>>()
            .map(Value::Array),
    }
}

#[cfg(test)]
#[path = "../../tests/unit/stage_state_tests.rs"]
mod tests;
