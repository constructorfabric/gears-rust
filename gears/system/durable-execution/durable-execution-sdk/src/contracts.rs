//! Serializable contracts shared by submitting hosts and activity workers.
pub use crate::activity::{
    ActivityDefinition, ActivityInput, ActivitySpec, ErasedActivity, ExecutionContract,
    ExecutionDefinition,
};
use crate::{ActivityError, DefinitionError};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

/// A persisted recipe, independent of Rust function addresses and type names.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "source", content = "value", rename_all = "snake_case")]
pub enum InputSource {
    RunInput,
    Checkpoint(String),
    Tuple(Vec<InputSource>),
    List(Vec<InputSource>),
}
impl InputSource {
    pub(crate) fn read(&self, input: &ActivityInput) -> Result<Value, ActivityError> {
        match self {
            Self::RunInput => Ok(input.run_input.clone()),
            Self::Checkpoint(id) => input
                .previous_results
                .get(id)
                .cloned()
                .ok_or_else(|| ActivityError::permanent("checkpoint_missing")),
            Self::Tuple(items) | Self::List(items) => items
                .iter()
                .map(|s| s.read(input))
                .collect::<Result<Vec<_>, _>>()
                .map(Value::Array),
        }
    }
    pub fn checkpoints(&self) -> Vec<&str> {
        match self {
            Self::RunInput => vec![],
            Self::Checkpoint(id) => vec![id],
            Self::Tuple(items) | Self::List(items) => {
                items.iter().flat_map(Self::checkpoints).collect()
            }
        }
    }
}
/// Persisted declaration of how step inputs and the workflow result are wired
/// to the run input and earlier checkpoints. It is part of the contract
/// identity (fingerprint), lets hosts without handlers locate the result and is
/// validated against stage order; the engine itself does not route data by it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DataFlowSpec {
    pub version: u8,
    pub inputs: BTreeMap<String, InputSource>,
    pub dependencies: BTreeMap<String, Vec<InputSource>>,
    pub output: InputSource,
}
impl DataFlowSpec {
    /// # Errors
    /// Rejects unsupported flow versions and dependencies outside earlier stages.
    pub fn validate(&self, contract: &ExecutionContract) -> Result<(), DefinitionError> {
        let invalid = || DefinitionError::new("invalid workflow input sources");
        if self.version != 1 || self.inputs.len() != contract.activities.len() {
            return Err(invalid());
        }
        let mut previous = BTreeSet::new();
        for stage in contract.stages()? {
            for id in &stage {
                let source = self.inputs.get(id).ok_or_else(invalid)?;
                for dependency in
                    std::iter::once(source).chain(self.dependencies.get(id).into_iter().flatten())
                {
                    if dependency
                        .checkpoints()
                        .iter()
                        .any(|id| !previous.contains(*id))
                    {
                        return Err(invalid());
                    }
                }
            }
            previous.extend(stage);
        }
        if self.dependencies.keys().any(|id| !previous.contains(id))
            || self
                .output
                .checkpoints()
                .iter()
                .any(|id| !previous.contains(*id))
        {
            return Err(invalid());
        }
        Ok(())
    }
}
