use crate::domain::error::DomainError;
use durable_execution_sdk::contracts::{ErasedActivity, ExecutionContract, ExecutionDefinition};
use durable_execution_sdk::{ActivityId, DefinitionError};
use parking_lot::RwLock;
use std::{collections::BTreeMap, sync::Arc};

/// Local contracts and handler bindings are updated atomically. The DB catalog controls admission.
#[derive(Default)]
#[toolkit_macros::domain_model]
pub struct Registry {
    state: RwLock<RegistryState>,
}
#[derive(Default)]
struct RegistryState {
    generations: BTreeMap<String, u64>,
    definitions: BTreeMap<String, Arc<ExecutionContract>>,
    handlers: BTreeMap<String, BTreeMap<String, Arc<dyn ErasedActivity>>>,
}
impl Registry {
    pub fn validate_bindings(&self) -> Result<(), DomainError> {
        let state = self.state.read();
        for (name, handlers) in &state.handlers {
            let definition = &state.definitions[name];
            if definition
                .activities
                .iter()
                .any(|a| !handlers.contains_key(&a.id.0))
            {
                return Err(DefinitionError::new("worker handlers are incomplete").into());
            }
        }

        Ok(())
    }
    pub fn available(&self, name: &str, generation: u64) -> bool {
        let state = self.state.read();
        state.generations.get(name) == Some(&generation) && state.handlers.contains_key(name)
    }
    pub fn evict(&self, name: &str, through: u64) {
        let mut state = self.state.write();
        if state.generations.get(name).is_some_and(|g| *g <= through) {
            state.handlers.remove(name);
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
    pub fn bind(
        &self,
        definition: ExecutionDefinition,
        generation: u64,
    ) -> Result<(), DomainError> {
        let contract = definition.contract();
        contract.validate()?;
        let mut state = self.state.write();
        if state.handlers.contains_key(&contract.name) {
            return Err(DomainError::DefinitionConflict(contract.name));
        }
        if let Some(existing) = state.definitions.get(&contract.name)
            && existing.as_ref() != &contract
        {
            return Err(DomainError::DefinitionConflict(contract.name));
        }
        let handlers = definition
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
