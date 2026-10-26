use crate::error::domain::DomainError;
use crate::{ActivityId, DefinitionError, DefinitionErrorKind};
use crate::{
    contracts::{ExecutionContract, ExecutionDefinition},
    runtime::ErasedActivity,
};
use parking_lot::RwLock;
use std::{collections::BTreeMap, sync::Arc};

/// Local contracts and handler bindings are updated atomically. The DB catalog controls admission.
#[derive(Default)]
pub struct Registry {
    state: RwLock<RegistryState>,
}
#[derive(Default)]
struct RegistryState {
    compensators: BTreeMap<String, BTreeMap<String, Arc<dyn crate::runtime::ErasedCompensation>>>,
    generations: BTreeMap<String, u64>,
    definitions: BTreeMap<String, Arc<ExecutionContract>>,
    handlers: BTreeMap<String, BTreeMap<String, Arc<dyn ErasedActivity>>>,
}
impl Registry {
    /// Checks that every bound definition has a handler for each activity.
    pub fn validate_bindings(&self) -> Result<(), DomainError> {
        let state = self.state.read();
        for (name, handlers) in &state.handlers {
            let definition = &state.definitions[name];
            if definition
                .activities
                .iter()
                .any(|a| !handlers.contains_key(&a.id.0))
            {
                return Err(DefinitionError::new(DefinitionErrorKind::IncompleteBindings).into());
            }
        }

        Ok(())
    }
    /// Returns `true` when handlers for `name` are bound at `generation`.
    pub fn available(&self, name: &str, generation: u64) -> bool {
        let state = self.state.read();
        state.generations.get(name) == Some(&generation) && state.handlers.contains_key(name)
    }
    /// Removes handler and compensator bindings for `name` bound at or before generation `through`.
    pub fn evict(&self, name: &str, through: u64) {
        let mut state = self.state.write();
        if state.generations.get(name).is_some_and(|g| *g <= through) {
            state.handlers.remove(name);
            state.compensators.remove(name);
            state.generations.remove(name);
        }
    }
    #[cfg(test)]
    pub fn get(&self, name: &str) -> Result<Arc<ExecutionContract>, DomainError> {
        self.state
            .read()
            .definitions
            .get(name)
            .cloned()
            .ok_or_else(|| DomainError::DefinitionNotFound(name.to_owned()))
    }
    /// Returns the contract for `name` when handlers are bound for all of its activities.
    pub fn executable_contract(&self, name: &str) -> Result<Arc<ExecutionContract>, DomainError> {
        let state = self.state.read();
        let contract = state
            .definitions
            .get(name)
            .ok_or_else(|| DomainError::DefinitionNotFound(name.to_owned()))?;
        let handlers = state
            .handlers
            .get(name)
            .ok_or_else(|| DomainError::DefinitionNotFound(name.to_owned()))?;
        if contract
            .activities
            .iter()
            .any(|a| !handlers.contains_key(&a.id.0))
        {
            return Err(DomainError::DefinitionNotFound(name.to_owned()));
        }
        Ok(contract.clone())
    }
    /// Returns the compensation handler bound for `step` of definition `name`.
    pub fn compensator(
        &self,
        name: &str,
        step: &ActivityId,
    ) -> Result<Arc<dyn crate::runtime::ErasedCompensation>, DomainError> {
        self.state
            .read()
            .compensators
            .get(name)
            .and_then(|c| c.get(&step.0))
            .cloned()
            .ok_or_else(|| DomainError::DefinitionNotFound(name.to_owned()))
    }
    /// Returns the activity handler bound for `activity` of definition `name`.
    pub fn handler(
        &self,
        name: &str,
        activity: &ActivityId,
    ) -> Result<Arc<dyn ErasedActivity>, DomainError> {
        self.state
            .read()
            .handlers
            .get(name)
            .and_then(|h| h.get(&activity.0))
            .cloned()
            .ok_or_else(|| DomainError::DefinitionNotFound(name.to_owned()))
    }
}
impl Registry {
    /// Records a contract without handlers; fails with `DefinitionConflict` when a different one is
    /// known.
    pub fn remember(&self, contract: ExecutionContract) -> Result<(), DomainError> {
        contract.validate()?;
        let mut state = self.state.write();
        if let Some(existing) = state.definitions.get(&contract.name) {
            return if existing.as_ref() == &contract {
                Ok(())
            } else {
                Err(DomainError::DefinitionConflict(contract.name))
            };
        }
        state
            .definitions
            .insert(contract.name.clone(), Arc::new(contract));
        Ok(())
    }
    #[cfg(test)]
    pub fn register_contract(&self, contract: ExecutionContract) -> Result<(), DomainError> {
        contract.validate()?;
        let mut state = self.state.write();
        if state.definitions.contains_key(&contract.name) {
            return Err(DomainError::DefinitionConflict(contract.name));
        }
        state
            .definitions
            .insert(contract.name.clone(), Arc::new(contract));
        Ok(())
    }
    #[cfg(test)]
    pub fn register(&self, definition: ExecutionDefinition) -> Result<(), DomainError> {
        self.bind(definition, 0)
    }
    /// Binds the handlers and compensators of `definition` at `generation`.
    /// Fails with `DefinitionConflict` when already bound or when the contract differs.
    pub fn bind(
        &self,
        definition: ExecutionDefinition,
        generation: u64,
    ) -> Result<(), DomainError> {
        // A built definition is already validated.
        let contract = definition.contract().clone();
        let mut state = self.state.write();
        if state.handlers.contains_key(&contract.name) {
            return Err(DomainError::DefinitionConflict(contract.name));
        }
        if let Some(existing) = state.definitions.get(&contract.name)
            && existing.as_ref() != &contract
        {
            return Err(DomainError::DefinitionConflict(contract.name));
        }
        let compensators = definition
            .activities()
            .iter()
            .filter_map(|a| {
                a.compensation
                    .as_ref()
                    .map(|c| (a.id.0.clone(), Arc::clone(&c.handler)))
            })
            .collect();
        state
            .compensators
            .insert(contract.name.clone(), compensators);
        let handlers = definition
            .into_parts()
            .activities
            .into_iter()
            .map(|a| (a.id.0, a.handler))
            .collect();
        state.generations.insert(contract.name.clone(), generation);
        state.handlers.insert(contract.name.clone(), handlers);
        state
            .definitions
            .insert(contract.name.clone(), Arc::new(contract));
        Ok(())
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "../../tests/unit/registry_tests.rs"]
mod tests;
