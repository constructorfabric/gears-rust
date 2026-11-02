//! Serializable contracts shared by submitting hosts and activity workers.
//!
//! [`ExecutionContract`] is the persisted, handler-free workflow identity. Build it
//! with [`crate::workflow::WorkflowBuilder`], which also binds the process-local
//! handlers, or register a contract alone with
//! [`crate::WorkflowRegistry::register_contract`].
use crate::authoring::{CompensationPolicy, FanOutOptions};
use crate::runtime::{ActivityInput, ErasedActivity, ErasedCompensationBinding};
use crate::{ActivityError, ActivityId, DefinitionError, DefinitionErrorKind, RetryPolicy};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet, HashSet},
    sync::Arc,
    time::Duration,
};
use toolkit_security::crypto::hash::Sha256;

/// Largest accepted activity or compensation timeout.
pub const MAX_TIMEOUT: Duration = Duration::from_hours(24);

/// Explicit serialized scheduling shape. Fan-out cardinality belongs to a run, not this contract.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum StageSpec {
    /// One ordinary activity.
    Sequential {
        /// Activity template ID declared in the contract.
        step: ActivityId,
    },
    /// A fixed ordered set of independent activities.
    Parallel {
        /// Activity template IDs in declaration order.
        steps: Vec<ActivityId>,
    },
    /// One activity template instantiated for each item of a run-specific list.
    FanOut {
        /// Activity template ID declared in the contract.
        step: ActivityId,
    },
}

/// A persisted recipe, independent of Rust function addresses and type names.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "source", content = "value", rename_all = "snake_case")]
#[non_exhaustive]
pub enum InputSource {
    /// The original workflow input.
    RunInput,
    /// One confirmed sequential or static branch checkpoint.
    Checkpoint(String),
    /// A heterogeneous tuple in declaration order.
    Tuple(Vec<InputSource>),
    /// A homogeneous checkpoint list in declaration order.
    List(Vec<InputSource>),
    /// Ordered results of a per-run expansion stage.
    FanOutResults(String),
    /// Result of one fan-out branch with a stable input index.
    FanOutBranch {
        /// Activity template ID declared in the contract.
        step: String,
        /// Stable zero-based input index within a fan-out stage.
        index: u32,
    },
}
impl InputSource {
    /// Materialize this selector from the run input and confirmed checkpoints.
    pub(crate) fn read(&self, input: &ActivityInput) -> Result<Value, ActivityError> {
        let missing = || ActivityError::permanent("checkpoint_missing");
        match self {
            Self::RunInput => Ok(input.run_input.clone()),
            Self::Checkpoint(id) | Self::FanOutResults(id) => {
                input.previous_results.get(id).cloned().ok_or_else(missing)
            }
            Self::FanOutBranch { step, index } => input
                .previous_results
                .get(step)
                .and_then(Value::as_array)
                .and_then(|items| items.get(usize::try_from(*index).ok()?))
                .cloned()
                .ok_or_else(missing),
            Self::Tuple(items) | Self::List(items) => items
                .iter()
                .map(|source| source.read(input))
                .collect::<Result<Vec<_>, _>>()
                .map(Value::Array),
        }
    }
    /// Template IDs referenced by this selector.
    #[must_use]
    pub fn checkpoints(&self) -> Vec<&str> {
        match self {
            Self::FanOutResults(id)
            | Self::FanOutBranch { step: id, .. }
            | Self::Checkpoint(id) => {
                vec![id]
            }
            Self::RunInput => vec![],
            Self::Tuple(items) | Self::List(items) => {
                items.iter().flat_map(Self::checkpoints).collect()
            }
        }
    }
}

/// Persisted declaration of how step inputs and the workflow result are wired
/// to the run input and earlier checkpoints. It is part of the contract identity,
/// lets hosts without handlers locate the result and is validated against stage order.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DataFlowSpec {
    /// Version of this serialized data-flow layout.
    pub version: u8,
    /// Selected input source for every activity template.
    pub inputs: BTreeMap<String, InputSource>,
    /// Earlier checkpoint sources explicitly declared by each activity.
    pub dependencies: BTreeMap<String, Vec<InputSource>>,
    /// Selector for the successful workflow result.
    pub output: InputSource,
}
impl DataFlowSpec {
    /// Flow of a low-level definition whose activities do not consume checkpoints:
    /// every activity reads the run input and the result is the last activity's checkpoint.
    #[must_use]
    pub fn independent<'a>(activities: impl IntoIterator<Item = &'a str>) -> Self {
        let ids: Vec<String> = activities.into_iter().map(str::to_owned).collect();
        Self {
            version: 1,
            inputs: ids
                .iter()
                .map(|id| (id.clone(), InputSource::RunInput))
                .collect(),
            dependencies: ids.iter().map(|id| (id.clone(), Vec::new())).collect(),
            output: ids.last().map_or(InputSource::RunInput, |id| {
                InputSource::Checkpoint(id.clone())
            }),
        }
    }
    /// # Errors
    /// [`DefinitionErrorKind::InvalidDataFlow`] for unsupported versions, missing
    /// inputs, or sources outside earlier stages.
    pub fn validate(&self, contract: &ExecutionContract) -> Result<(), DefinitionError> {
        let invalid =
            |step: &str| DefinitionError::new(DefinitionErrorKind::InvalidDataFlow).for_step(step);
        if self.version != 1 || self.inputs.len() != contract.activities.len() {
            return Err(DefinitionError::new(DefinitionErrorKind::InvalidDataFlow));
        }
        let mut previous = BTreeSet::new();
        for stage in contract.stages()? {
            for id in &stage {
                let source = self.inputs.get(id).ok_or_else(|| invalid(id))?;
                let reads_later_stage = std::iter::once(source)
                    .chain(self.dependencies.get(id).into_iter().flatten())
                    .any(|dependency| {
                        dependency
                            .checkpoints()
                            .iter()
                            .any(|c| !previous.contains(*c))
                    });
                if reads_later_stage {
                    return Err(invalid(id));
                }
            }
            previous.extend(stage);
        }
        if let Some(id) = self.dependencies.keys().find(|id| !previous.contains(*id)) {
            return Err(invalid(id));
        }
        if self
            .output
            .checkpoints()
            .iter()
            .any(|id| !previous.contains(*id))
        {
            return Err(DefinitionError::new(DefinitionErrorKind::InvalidDataFlow));
        }
        Ok(())
    }
}

/// What a result read returns: the workflow result, which requires successful
/// forward completion, or one confirmed checkpoint. Seen by [`crate::testing`]
/// fakes of the run service.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum ResultSelector {
    /// The workflow's final result.
    Workflow,
    /// One confirmed checkpoint.
    Checkpoint(InputSource),
}

/// Serializable policies of one activity template.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActivitySpec {
    /// Independent reverse execution policy.
    pub compensation: Option<CompensationPolicy>,
    /// Runtime list expansion policy.
    pub fanout: Option<FanOutOptions>,
    /// Stable template identifier.
    pub id: ActivityId,
    /// Positive invocation timeout, at most [`MAX_TIMEOUT`].
    pub timeout: Duration,
    /// Automatic retry budget and delay policy.
    pub retry: RetryPolicy,
}

/// Serializable versioned workflow identity. Safe to register on API processes
/// without constructing worker dependencies or handlers.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionContract {
    /// Automatically compensate terminal forward failures.
    pub saga: bool,
    /// Versioned workflow registration name.
    pub name: String,
    /// Ordered activity templates.
    pub activities: Vec<ActivitySpec>,
    /// Ordered sequential, static parallel or runtime fan-out stages.
    pub stages: Vec<StageSpec>,
    /// Input, dependency and final-result selectors.
    pub flow: DataFlowSpec,
}
impl ExecutionContract {
    /// Maximum number of activity templates in one contract.
    pub const MAX_ACTIVITIES: usize = 128;

    /// Validate a versioned name such as `orders.reserve.v1`.
    ///
    /// # Errors
    /// [`DefinitionErrorKind::InvalidName`] for an oversized or malformed name.
    pub fn validate_name(name: &str) -> Result<(), DefinitionError> {
        let charset = !name.is_empty()
            && name.len() <= 160
            && name
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b".-_".contains(&b));
        let versioned = name.rsplit_once(".v").is_some_and(|(_, v)| {
            !v.is_empty()
                && v.bytes().all(|b| b.is_ascii_digit())
                && v.parse::<u64>().is_ok_and(|n| n > 0)
        });
        if charset && versioned {
            Ok(())
        } else {
            Err(DefinitionError::new(DefinitionErrorKind::InvalidName))
        }
    }

    /// # Errors
    /// Returns the first invalid name, activity, policy, stage or data-flow rule.
    pub fn validate(&self) -> Result<(), DefinitionError> {
        Self::validate_name(&self.name)?;
        if self.activities.is_empty() || self.activities.len() > Self::MAX_ACTIVITIES {
            return Err(DefinitionError::new(DefinitionErrorKind::ActivityCount));
        }
        let mut ids = HashSet::new();
        for activity in &self.activities {
            activity.validate()?;
            if !ids.insert(&activity.id) {
                return Err(
                    DefinitionError::new(DefinitionErrorKind::DuplicateActivityId)
                        .for_step(&activity.id.0),
                );
            }
        }
        self.stages()?;
        self.flow.validate(self)?;
        Ok(())
    }

    /// SHA-256 of the persisted contract, independent of handler addresses.
    /// Changes to handler semantics must use a new definition version.
    ///
    /// # Errors
    /// Returns the validation error of an invalid contract.
    pub fn fingerprint(&self) -> Result<String, DefinitionError> {
        self.validate()?;
        let bytes = serde_json::to_vec(&("durable-contract-v3", self))
            .map_err(|_| DefinitionError::new(DefinitionErrorKind::Encoding))?;
        Ok(hex::encode(Sha256::digest(&bytes).as_ref()))
    }

    /// Activity IDs grouped by stage, in execution order.
    ///
    /// # Errors
    /// [`DefinitionErrorKind::InvalidStages`] unless stages list every activity once,
    /// in declaration order, with matching fan-out policies.
    pub fn stages(&self) -> Result<Vec<Vec<String>>, DefinitionError> {
        let invalid = || DefinitionError::new(DefinitionErrorKind::InvalidStages);
        let mut stages = Vec::with_capacity(self.stages.len());
        for stage in &self.stages {
            let ids = match stage {
                StageSpec::Sequential { step } | StageSpec::FanOut { step } => vec![step.0.clone()],
                StageSpec::Parallel { steps } if steps.len() >= 2 => {
                    steps.iter().map(|s| s.0.clone()).collect()
                }
                StageSpec::Parallel { .. } => {
                    return Err(DefinitionError::new(
                        DefinitionErrorKind::InvalidParallelStage,
                    ));
                }
            };
            for id in &ids {
                let spec = self
                    .activities
                    .iter()
                    .find(|a| a.id.0 == *id)
                    .ok_or_else(|| invalid().for_step(id))?;
                if spec.fanout.is_some() != matches!(stage, StageSpec::FanOut { .. }) {
                    return Err(invalid().for_step(id));
                }
            }
            stages.push(ids);
        }
        if stages.is_empty()
            || stages
                .iter()
                .flatten()
                .ne(self.activities.iter().map(|a| &a.id.0))
        {
            return Err(invalid());
        }
        Ok(stages)
    }
}
impl ActivitySpec {
    fn validate(&self) -> Result<(), DefinitionError> {
        let step = |kind| DefinitionError::new(kind).for_step(&self.id.0);
        let id = &self.id.0;
        if id.is_empty()
            || id.len() > 80
            || !id
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"-_".contains(&b))
        {
            return Err(step(DefinitionErrorKind::InvalidActivityId));
        }
        if self.timeout.is_zero() || self.timeout > MAX_TIMEOUT {
            return Err(step(DefinitionErrorKind::InvalidTimeout));
        }
        self.retry.validate().map_err(|e| e.for_step(id))?;
        if let Some(fanout) = &self.fanout {
            fanout.validate().map_err(|e| e.for_step(id))?;
        }
        if let Some(compensation) = &self.compensation
            && (compensation.timeout.is_zero()
                || compensation.timeout > MAX_TIMEOUT
                || compensation.retry.validate().is_err())
        {
            return Err(step(DefinitionErrorKind::InvalidCompensationPolicy));
        }
        Ok(())
    }
}

/// One activity of a low-level definition: a process-local handler and its
/// serializable policies.
///
/// This is input to [`DefinitionParts::build`]; a built [`ExecutionDefinition`]
/// exposes its activities read-only, after validation.
#[derive(Clone)]
pub(crate) struct ActivityDefinition {
    /// Optional reverse handler and its independent policy.
    pub compensation: Option<ErasedCompensationBinding>,
    /// Runtime list expansion policy.
    pub fanout: Option<FanOutOptions>,
    /// Stable template identifier.
    pub id: ActivityId,
    /// Positive invocation timeout, at most [`MAX_TIMEOUT`].
    pub timeout: Duration,
    /// Automatic retry budget and delay policy.
    pub retry: RetryPolicy,
    /// Process-local handler, excluded from serialization and `Debug`.
    pub handler: Arc<dyn ErasedActivity>,
}
impl ActivityDefinition {
    fn spec(&self) -> ActivitySpec {
        ActivitySpec {
            compensation: self.compensation.as_ref().map(|c| c.policy.clone()),
            fanout: self.fanout.clone(),
            id: self.id.clone(),
            timeout: self.timeout,
            retry: self.retry.clone(),
        }
    }
}
impl std::fmt::Debug for ActivityDefinition {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ActivityDefinition")
            .field("id", &self.id)
            .field("timeout", &self.timeout)
            .field("retry", &self.retry)
            .field("compensation", &self.compensation)
            .field("fanout", &self.fanout)
            .finish_non_exhaustive()
    }
}

/// Unvalidated parts of a low-level [`ExecutionDefinition`].
///
/// Typed code uses [`crate::workflow::WorkflowBuilder`] instead.
#[derive(Clone, Debug)]
pub(crate) struct DefinitionParts {
    /// Automatically compensate terminal forward failures.
    pub saga: bool,
    /// Versioned workflow registration name.
    pub name: String,
    /// Ordered activity templates.
    pub activities: Vec<ActivityDefinition>,
    /// Contiguous groups of step IDs executed in parallel. Unlisted steps remain
    /// sequential. Arbitrary dependencies and nested groups are unsupported.
    pub parallel_groups: Vec<Vec<ActivityId>>,
    /// Input, dependency and final-result selectors.
    pub flow: DataFlowSpec,
}
impl DefinitionParts {
    /// Validate the parts and derive the serializable contract and its fingerprint.
    ///
    /// # Errors
    /// Returns the first invalid grouping, name, activity, policy or data-flow rule.
    pub fn build(self) -> Result<ExecutionDefinition, DefinitionError> {
        let stages = self.stages()?;
        let contract = ExecutionContract {
            saga: self.saga,
            name: self.name.clone(),
            activities: self
                .activities
                .iter()
                .map(ActivityDefinition::spec)
                .collect(),
            stages: self.stage_specs(stages),
            flow: self.flow.clone(),
        };
        let fingerprint = contract.fingerprint()?;
        Ok(ExecutionDefinition {
            parts: self,
            contract,
            fingerprint,
        })
    }
    fn stage_specs(&self, stages: Vec<Vec<String>>) -> Vec<StageSpec> {
        stages
            .into_iter()
            .map(|ids| match <[String; 1]>::try_from(ids) {
                Ok([id])
                    if self
                        .activities
                        .iter()
                        .any(|a| a.id.0 == id && a.fanout.is_some()) =>
                {
                    StageSpec::FanOut {
                        step: ActivityId(id),
                    }
                }
                Ok([id]) => StageSpec::Sequential {
                    step: ActivityId(id),
                },
                Err(ids) => StageSpec::Parallel {
                    steps: ids.into_iter().map(ActivityId).collect(),
                },
            })
            .collect()
    }
    /// Activity IDs grouped by stage, in execution order.
    fn stages(&self) -> Result<Vec<Vec<String>>, DefinitionError> {
        let invalid = || DefinitionError::new(DefinitionErrorKind::InvalidStages);
        let mut groups = BTreeMap::new();
        let mut seen = HashSet::new();
        for group in &self.parallel_groups {
            let [first, _, ..] = group.as_slice() else {
                return Err(DefinitionError::new(
                    DefinitionErrorKind::InvalidParallelStage,
                ));
            };
            let start = self
                .activities
                .iter()
                .position(|a| a.id == *first)
                .ok_or_else(|| invalid().for_step(&first.0))?;
            for (offset, id) in group.iter().enumerate() {
                if self
                    .activities
                    .get(start + offset)
                    .is_none_or(|a| a.id != *id)
                    || !seen.insert(&id.0)
                {
                    return Err(invalid().for_step(&id.0));
                }
            }
            groups.insert(start, group.len());
        }
        let mut stages = Vec::new();
        let mut i = 0;
        while i < self.activities.len() {
            let count = groups.get(&i).copied().unwrap_or(1);
            stages.push(
                self.activities[i..i + count]
                    .iter()
                    .map(|a| a.id.0.clone())
                    .collect(),
            );
            i += count;
        }
        Ok(stages)
    }
}
impl TryFrom<DefinitionParts> for ExecutionDefinition {
    type Error = DefinitionError;
    fn try_from(parts: DefinitionParts) -> Result<Self, Self::Error> {
        parts.build()
    }
}

/// Validated versioned workflow definition with process-local handlers.
///
/// Internal form produced by [`crate::workflow::WorkflowBuilder`] and registered by
/// [`crate::WorkflowRegistry::register_workflow`].
#[derive(Clone)]
pub(crate) struct ExecutionDefinition {
    parts: DefinitionParts,
    contract: ExecutionContract,
    fingerprint: String,
}
impl ExecutionDefinition {
    /// Versioned workflow registration name.
    #[cfg(any(test, feature = "test-util"))]
    #[must_use]
    pub fn name(&self) -> &str {
        &self.parts.name
    }
    /// Whether terminal forward failures are compensated.
    #[cfg(any(test, feature = "test-util"))]
    #[must_use]
    pub fn saga(&self) -> bool {
        self.parts.saga
    }
    /// Ordered activity templates with their handlers.
    #[must_use]
    pub fn activities(&self) -> &[ActivityDefinition] {
        &self.parts.activities
    }
    /// Input, dependency and final-result selectors.
    #[cfg(any(test, feature = "test-util"))]
    #[must_use]
    pub fn flow(&self) -> &DataFlowSpec {
        &self.parts.flow
    }
    /// Serializable contract without handlers.
    #[must_use]
    pub fn contract(&self) -> &ExecutionContract {
        &self.contract
    }
    /// Contract fingerprint, computed once at build.
    #[must_use]
    pub fn fingerprint(&self) -> &str {
        &self.fingerprint
    }
    /// Activity IDs grouped by stage, in execution order.
    #[cfg(any(test, feature = "test-util"))]
    #[must_use]
    pub fn stages(&self) -> Vec<Vec<String>> {
        self.contract
            .stages
            .iter()
            .map(|stage| match stage {
                StageSpec::Sequential { step } | StageSpec::FanOut { step } => {
                    vec![step.0.clone()]
                }
                StageSpec::Parallel { steps } => steps.iter().map(|s| s.0.clone()).collect(),
            })
            .collect()
    }
    /// The editable parts, for building a changed definition.
    #[must_use]
    pub fn into_parts(self) -> DefinitionParts {
        self.parts
    }
}
impl std::fmt::Debug for ExecutionDefinition {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExecutionDefinition")
            .field("name", &self.parts.name)
            .field("saga", &self.parts.saga)
            .field("activities", &self.parts.activities)
            .field("parallel_groups", &self.parts.parallel_groups)
            .field("fingerprint", &self.fingerprint)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "../tests/unit/activity_tests.rs"]
mod tests;
