//! In-memory execution of a [`Workflow`] for unit tests of consumer gears.
//!
//! Enabled by the `test-util` feature. [`TestRuntime`] runs handlers in-process,
//! without storage, leases, queues, authorization or retry delays. It follows the
//! runtime's rules:
//! - stages run in order, and each stage is materialized before any of its handlers runs;
//! - fan-out lists are limited by `max_items`, and a run by 4096 forward instances;
//! - static branches and fan-out items run one at a time, in index order;
//! - a timed-out handler is cancelled and given the same timeout again to clean up;
//!   a panic becomes the retryable code `activity_panicked`;
//! - saga compensation runs in reverse stage and reverse branch order.
//!
//! Use PostgreSQL integration tests for crash recovery, concurrency and control commands.
//!
//! ```
//! use toolkit_durable_execution::{prelude::*, testing::{TestOutcome, TestRuntime}};
//! use std::time::Duration;
//! # tokio_test_block_on(async {
//! let workflow = WorkflowBuilder::<u32>::new("docs.double.v1")
//!     .then(Step::new("double", Duration::from_secs(5), |_, n: u32| async move { Ok(n * 2) }))
//!     .build()?;
//! let run = TestRuntime::new().run(&workflow, 21).await?;
//! assert_eq!(run.outcome(), &TestOutcome::Succeeded(42));
//! assert_eq!(run.attempts("double"), 1);
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! # }).unwrap();
//! # fn tokio_test_block_on<F: std::future::Future>(f: F) -> F::Output {
//! #     tokio::runtime::Builder::new_current_thread().enable_time().build().unwrap().block_on(f)
//! # }
//! ```
use crate::activity::ActivityContextInit;
pub use crate::client::{InspectService, RunService};
use crate::contracts::{ActivityDefinition, DataFlowSpec, InputSource};
use crate::runtime::ActivityInput;
use crate::workflow::{CheckpointReference, Payload, Workflow};
use crate::{
    ActivityContext, ActivityError, ActivityId, ActivityInstanceId, DefinitionError,
    DefinitionErrorKind, ExecutionOwner, ExecutionPhase, RetryPolicy, RunId,
};
use chrono::Utc;
use serde_json::Value;
use std::{collections::BTreeMap, future::Future, time::Duration};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

/// Largest number of forward activity instances one run may materialize.
pub const MAX_FORWARD_INSTANCES: usize = 4096;

/// Final state of an in-memory run. `Debug` omits the workflow result.
#[derive(Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum TestOutcome<O> {
    /// Every stage succeeded; carries the workflow result.
    Succeeded(O),
    /// Forward execution failed and no compensation ran.
    Failed {
        /// Instance key of the failed step.
        step: String,
        /// Safe failure code.
        code: String,
    },
    /// A saga failed and every declared compensator succeeded.
    Compensated {
        /// Instance key of the failed forward step.
        step: String,
        /// Safe failure code of the forward step.
        code: String,
    },
    /// A saga failed and one compensator exhausted its retry budget.
    CompensationFailed {
        /// Instance key of the failed forward step.
        step: String,
        /// Safe failure code of the forward step that started compensation.
        code: String,
        /// Instance key of the failed compensator.
        compensator: String,
        /// Safe failure code of the compensator.
        compensator_code: String,
    },
}
impl<O> std::fmt::Debug for TestOutcome<O> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Succeeded(_) => f.write_str("Succeeded(..)"),
            Self::Failed { step, code } => f
                .debug_struct("Failed")
                .field("step", step)
                .field("code", code)
                .finish(),
            Self::Compensated { step, code } => f
                .debug_struct("Compensated")
                .field("step", step)
                .field("code", code)
                .finish(),
            Self::CompensationFailed {
                step,
                code,
                compensator,
                compensator_code,
            } => f
                .debug_struct("CompensationFailed")
                .field("step", step)
                .field("code", code)
                .field("compensator", compensator)
                .field("compensator_code", compensator_code)
                .finish(),
        }
    }
}

/// Why [`TestRun::checkpoint`] could not read a checkpoint.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum CheckpointReadError {
    /// The reference was taken from a builder that was never built.
    #[error("checkpoint reference belongs to a workflow that was not built")]
    NotFinalized,
    /// The reference belongs to a different workflow contract than this run.
    #[error("checkpoint reference belongs to a different workflow contract")]
    OtherWorkflow,
    /// The stored value does not decode as the reference type.
    #[error("checkpoint value does not decode as the reference type")]
    Undecodable,
}

/// Result of [`TestRuntime::run`]. `Debug` shows identity and counts, never payloads.
pub struct TestRun<O> {
    run_id: RunId,
    workflow: String,
    fingerprint: String,
    outcome: TestOutcome<O>,
    run_input: Value,
    checkpoints: BTreeMap<String, Value>,
    branches: BTreeMap<(String, u32), Value>,
    attempts: BTreeMap<String, u32>,
    compensated: Vec<String>,
}
impl<O> TestRun<O> {
    /// Generated run identifier, also embedded in idempotency keys.
    #[must_use]
    pub fn run_id(&self) -> RunId {
        self.run_id
    }
    /// Final state.
    #[must_use]
    pub fn outcome(&self) -> &TestOutcome<O> {
        &self.outcome
    }
    /// The workflow result, or the final state when the run did not succeed.
    ///
    /// # Errors
    /// Returns the outcome of an unsuccessful run.
    pub fn into_output(self) -> Result<O, TestOutcome<O>> {
        match self.outcome {
            TestOutcome::Succeeded(output) => Ok(output),
            other => Err(other),
        }
    }
    /// A confirmed checkpoint decoded as its declared type; `Ok(None)` when absent.
    ///
    /// A fan-out branch is readable as soon as it succeeds, even if a sibling failed;
    /// the whole fan-out list only after every branch succeeded.
    ///
    /// # Errors
    /// [`CheckpointReadError`] for a reference of another or unbuilt workflow, or a
    /// value that does not decode.
    pub fn checkpoint<R: CheckpointReference>(
        &self,
        reference: &R,
    ) -> Result<Option<R::Output>, CheckpointReadError> {
        let step = reference.as_step_ref();
        let fingerprint = step
            .fingerprint()
            .map_err(|_| CheckpointReadError::NotFinalized)?;
        if step.workflow_name() != self.workflow || fingerprint != self.fingerprint {
            return Err(CheckpointReadError::OtherWorkflow);
        }
        let value = match step.source() {
            InputSource::FanOutBranch { step, index } => {
                self.branches.get(&(step.clone(), *index)).cloned()
            }
            source => source.read(&self.input()).ok(),
        };
        value
            .map(|v| serde_json::from_value(v).map_err(|_| CheckpointReadError::Undecodable))
            .transpose()
    }
    /// Invocations of a forward instance, by key: the step ID, or `f/<step>/<index>`
    /// for a fan-out item. Compensators use `c/<step>/<index|single>`.
    #[must_use]
    pub fn attempts(&self, instance: &str) -> u32 {
        self.attempts.get(instance).copied().unwrap_or_default()
    }
    /// Instance keys of successful compensators, in execution order.
    #[must_use]
    pub fn compensated(&self) -> &[String] {
        &self.compensated
    }
    fn input(&self) -> ActivityInput {
        ActivityInput {
            selected_input: None,
            run_input: self.run_input.clone(),
            previous_results: self.checkpoints.clone(),
        }
    }
}
impl<O> std::fmt::Debug for TestRun<O> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TestRun")
            .field("run_id", &self.run_id)
            .field("workflow", &self.workflow)
            .field("outcome", &self.outcome)
            .field("checkpoints", &self.checkpoints.len())
            .field("attempts", &self.attempts)
            .field("compensated", &self.compensated)
            .finish_non_exhaustive()
    }
}

/// Executes workflows in memory. See the [module documentation](self).
#[derive(Clone, Debug)]
pub struct TestRuntime {
    owner: ExecutionOwner,
}
impl Default for TestRuntime {
    fn default() -> Self {
        Self::new()
    }
}
impl TestRuntime {
    /// A runtime whose runs belong to a nil tenant and subject.
    #[must_use]
    pub fn new() -> Self {
        Self {
            owner: ExecutionOwner {
                tenant_id: Uuid::nil(),
                subject_id: Uuid::nil(),
            },
        }
    }
    /// Submit runs as `owner`.
    #[must_use]
    pub fn owner(mut self, owner: ExecutionOwner) -> Self {
        self.owner = owner;
        self
    }

    /// Execute one run of `workflow` to completion.
    ///
    /// # Errors
    /// [`DefinitionErrorKind::Encoding`] when the input or the final result cannot
    /// be converted.
    pub async fn run<I: Payload, O: Payload>(
        &self,
        workflow: &Workflow<I, O>,
        input: I,
    ) -> Result<TestRun<O>, DefinitionError> {
        let encoding = || DefinitionError::new(DefinitionErrorKind::Encoding);
        let definition = &workflow.definition;
        let mut run = Execution {
            runtime: self,
            run_id: RunId::from(Uuid::new_v4()),
            run_input: serde_json::to_value(input).map_err(|_| encoding())?,
            checkpoints: BTreeMap::new(),
            branches: BTreeMap::new(),
            attempts: BTreeMap::new(),
            confirmed: Vec::new(),
            forward_instances: 0,
        };
        let mut failure = None;
        for stage in definition.stages() {
            let activities: Vec<&ActivityDefinition> = stage
                .iter()
                .filter_map(|id| definition.activities().iter().find(|a| a.id.0 == *id))
                .collect();
            if let Err(failed) = run
                .stage(&activities, definition.flow(), definition.saga())
                .await
            {
                failure = Some(failed);
                break;
            }
        }
        let mut compensated = Vec::new();
        let outcome = match failure {
            None => {
                let output = definition
                    .flow()
                    .output
                    .read(&run.input(None))
                    .map_err(|_| encoding())?;
                TestOutcome::Succeeded(serde_json::from_value(output).map_err(|_| encoding())?)
            }
            Some((step, code)) if definition.saga() => {
                match run.compensate(&mut compensated).await {
                    Ok(()) => TestOutcome::Compensated { step, code },
                    Err((compensator, compensator_code)) => TestOutcome::CompensationFailed {
                        step,
                        code,
                        compensator,
                        compensator_code,
                    },
                }
            }
            Some((step, code)) => TestOutcome::Failed { step, code },
        };
        Ok(TestRun {
            run_id: run.run_id,
            workflow: definition.name().to_owned(),
            fingerprint: definition.fingerprint().to_owned(),
            outcome,
            run_input: run.run_input,
            checkpoints: run.checkpoints,
            branches: run.branches,
            attempts: run.attempts,
            compensated,
        })
    }
}

/// One materialized forward instance of a stage.
struct Planned<'a> {
    activity: &'a ActivityDefinition,
    item_index: Option<u32>,
    input: Value,
}
struct Confirmed<'a> {
    activity: &'a ActivityDefinition,
    item_index: Option<u32>,
    input: Value,
    output: Value,
}
struct Execution<'a> {
    runtime: &'a TestRuntime,
    run_id: RunId,
    run_input: Value,
    checkpoints: BTreeMap<String, Value>,
    branches: BTreeMap<(String, u32), Value>,
    attempts: BTreeMap<String, u32>,
    confirmed: Vec<Confirmed<'a>>,
    forward_instances: usize,
}
/// Failed instance key and safe code.
type Failure = (String, String);
impl<'a> Execution<'a> {
    fn input(&self, selected: Option<Value>) -> ActivityInput {
        ActivityInput {
            selected_input: selected,
            run_input: self.run_input.clone(),
            previous_results: self.checkpoints.clone(),
        }
    }

    /// Materialize every instance of the stage, as the runtime does, before running any.
    fn plan(
        &self,
        activities: &[&'a ActivityDefinition],
        flow: &DataFlowSpec,
    ) -> Result<Vec<Planned<'a>>, Failure> {
        let mut planned = Vec::new();
        for activity in activities {
            let id = &activity.id.0;
            let reject = |code: &str| (id.clone(), code.to_owned());
            let source = flow.inputs.get(id).unwrap_or(&InputSource::RunInput);
            let input = source
                .read(&self.input(None))
                .map_err(|error| reject(error.code()))?;
            if let Some(options) = &activity.fanout {
                let Value::Array(items) = input else {
                    return Err(reject("fanout_input_not_list"));
                };
                if items.len() > options.max_items
                    || self.forward_instances + planned.len() + items.len() > MAX_FORWARD_INSTANCES
                {
                    return Err(reject("fanout_limit_exceeded"));
                }
                for (index, item) in (0u32..).zip(items) {
                    planned.push(Planned {
                        activity,
                        item_index: Some(index),
                        input: item,
                    });
                }
            } else {
                if self.forward_instances + planned.len() >= MAX_FORWARD_INSTANCES {
                    return Err(reject("fanout_limit_exceeded"));
                }
                planned.push(Planned {
                    activity,
                    item_index: None,
                    input,
                });
            }
        }
        Ok(planned)
    }

    async fn stage(
        &mut self,
        activities: &[&'a ActivityDefinition],
        flow: &DataFlowSpec,
        saga: bool,
    ) -> Result<(), Failure> {
        let planned = self.plan(activities, flow)?;
        self.forward_instances += planned.len();
        let mut failure = None;
        let mut outputs: BTreeMap<String, Vec<Value>> = BTreeMap::new();
        for instance in planned {
            match self
                .invoke(
                    instance.activity,
                    instance.item_index,
                    instance.input.clone(),
                )
                .await
            {
                Ok(output) => {
                    // Each confirmed instance is readable at once, as in the runtime;
                    // a fan-out list only after the whole stage succeeds.
                    let id = instance.activity.id.0.clone();
                    match instance.item_index {
                        Some(index) => {
                            self.branches.insert((id.clone(), index), output.clone());
                            outputs.entry(id).or_default().push(output.clone());
                        }
                        None => {
                            self.checkpoints.insert(id, output.clone());
                        }
                    }
                    self.confirmed.push(Confirmed {
                        activity: instance.activity,
                        item_index: instance.item_index,
                        input: instance.input,
                        output,
                    });
                }
                Err(failed) => {
                    failure.get_or_insert(failed);
                    if saga {
                        break;
                    }
                }
            }
        }
        if let Some(failed) = failure {
            return Err(failed);
        }
        for activity in activities.iter().filter(|a| a.fanout.is_some()) {
            let id = &activity.id.0;
            let values = outputs.remove(id).unwrap_or_default();
            self.checkpoints.insert(id.clone(), Value::Array(values));
        }
        Ok(())
    }

    async fn invoke(
        &mut self,
        activity: &ActivityDefinition,
        item_index: Option<u32>,
        input: Value,
    ) -> Result<Value, Failure> {
        let instance = ActivityInstanceId {
            step: activity.id.clone(),
            item_index,
            phase: ExecutionPhase::Forward,
        };
        let key = instance.key();
        retry(&activity.retry, |attempt| {
            self.attempts.insert(key.clone(), attempt);
            let token = CancellationToken::new();
            let context = self.context(&instance, attempt, activity.timeout, &token);
            let handler = activity.handler.clone();
            let input = self.input(Some(input.clone()));
            supervise(
                async move { handler.execute(context, input).await },
                token,
                activity.timeout,
            )
        })
        .await
        .map_err(|code| (key.clone(), code))
    }

    async fn compensate(&mut self, compensated: &mut Vec<String>) -> Result<(), Failure> {
        let confirmed = std::mem::take(&mut self.confirmed);
        for done in confirmed.iter().rev() {
            let Some(binding) = &done.activity.compensation else {
                continue;
            };
            let instance = ActivityInstanceId {
                step: done.activity.id.clone(),
                item_index: done.item_index,
                phase: ExecutionPhase::Compensation,
            };
            let key = instance.key();
            retry(&binding.policy.retry, |attempt| {
                self.attempts.insert(key.clone(), attempt);
                let token = CancellationToken::new();
                let context = self.context(&instance, attempt, binding.policy.timeout, &token);
                let handler = binding.handler.clone();
                let (input, output) = (done.input.clone(), done.output.clone());
                supervise(
                    async move {
                        handler
                            .execute(context, input, output)
                            .await
                            .map(|()| Value::Null)
                    },
                    token,
                    binding.policy.timeout,
                )
            })
            .await
            .map_err(|code| (key.clone(), code))?;
            compensated.push(key);
        }
        Ok(())
    }

    fn context(
        &self,
        instance: &ActivityInstanceId,
        attempt: u32,
        timeout: Duration,
        token: &CancellationToken,
    ) -> ActivityContext {
        let key = instance.key();
        ActivityContext::from(ActivityContextInit {
            instance: instance.clone(),
            run_id: self.run_id,
            activity_id: ActivityId(key.clone()),
            attempt,
            execution_epoch: 0,
            idempotency_key: format!("{}:{key}", self.run_id),
            owner: self.runtime.owner.clone(),
            deadline: Utc::now()
                + chrono::Duration::from_std(timeout).unwrap_or(chrono::Duration::MAX),
            cancellation: token.clone(),
        })
    }
}

/// A spawned invocation that cannot outlive its supervisor: dropping the guard, for
/// example when the caller times out `TestRuntime::run`, signals cancellation and
/// aborts the task, like the runtime executor's abort-on-drop task.
struct Invocation {
    task: tokio::task::JoinHandle<Result<Value, ActivityError>>,
    token: CancellationToken,
}
impl Drop for Invocation {
    fn drop(&mut self) {
        self.token.cancel();
        self.task.abort();
    }
}

/// Run one invocation in its own task, as the runtime executor does: a panic becomes
/// `activity_panicked`; on timeout the token is cancelled and the handler gets the
/// same timeout again to confirm cleanup before it is aborted.
async fn supervise<F>(
    invocation: F,
    token: CancellationToken,
    timeout: Duration,
) -> Result<Value, ActivityError>
where
    F: Future<Output = Result<Value, ActivityError>> + Send + 'static,
{
    let mut guard = Invocation {
        task: tokio::spawn(invocation),
        token,
    };
    if let Ok(joined) = tokio::time::timeout(timeout, &mut guard.task).await {
        return joined.unwrap_or_else(|_| Err(ActivityError::retryable("activity_panicked")));
    }
    guard.token.cancel();
    // Cleanup time is bounded; dropping the guard aborts a handler that ignores it.
    let _cleanup = tokio::time::timeout(timeout, &mut guard.task).await;
    Err(ActivityError::retryable("activity_timeout"))
}

/// Invoke without delays until success, a permanent failure, cancellation or an
/// exhausted budget, yielding between attempts. Returns the safe failure code.
async fn retry<F, Fut>(policy: &RetryPolicy, mut attempt: F) -> Result<Value, String>
where
    F: FnMut(u32) -> Fut,
    Fut: Future<Output = Result<Value, ActivityError>>,
{
    let mut number = 0;
    loop {
        number += 1;
        let error = match attempt(number).await {
            Ok(output) => return Ok(output),
            Err(error) => error,
        };
        if !error.is_retryable() || number >= policy.max_attempts {
            return Err(error.code().to_owned());
        }
        tokio::task::yield_now().await;
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "../tests/unit/testing_tests.rs"]
mod tests;
