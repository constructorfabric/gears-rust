use crate::domain::error::DomainError;
use durable_execution_sdk::contracts::ExecutionContract;
use durable_execution_sdk::registration::{Registration, RegistrationState, UnregisterMode};
use toolkit_macros::domain_model;

/// Persisted lifecycle is independent of run execution epochs and delivery fences.
#[domain_model]
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct Definition {
    pub contract: ExecutionContract,
    pub generation: u64,
    pub revision: u64,
    pub state: RegistrationState,
    pub revoked_through: Option<u64>,
}
impl Definition {
    pub fn new(contract: ExecutionContract) -> Self {
        Self {
            contract,
            generation: 0,
            revision: 0,
            state: RegistrationState::Active,
            revoked_through: None,
        }
    }
    pub fn view(&self) -> Registration {
        Registration {
            name: self.contract.name.clone(),
            generation: self.generation,
            revision: self.revision,
            state: self.state,
        }
    }
    pub fn permits(&self, generation: u64) -> bool {
        generation == self.generation && self.revoked_through.is_none_or(|last| generation > last)
    }
    pub fn binding_generation(&self) -> Result<u64, DomainError> {
        if self.state == RegistrationState::Stopping {
            return Err(DomainError::InvalidState("definition is stopping"));
        }
        if self.state == RegistrationState::Released {
            self.generation
                .checked_add(1)
                .ok_or(DomainError::Internal("generation overflow"))
        } else {
            Ok(self.generation)
        }
    }
    pub fn unregister(&mut self, revision: u64, mode: UnregisterMode) -> Result<(), DomainError> {
        if self.revision != revision {
            return Err(DomainError::ConcurrentUpdate);
        }
        match mode {
            UnregisterMode::Retain => match self.state {
                RegistrationState::Active | RegistrationState::Retired => {
                    self.state = RegistrationState::Retired;
                }
                _ => {
                    return Err(DomainError::InvalidState(
                        "definition cannot be unregistered",
                    ));
                }
            },
            UnregisterMode::CancelAndRelease => {
                if self.state != RegistrationState::Released {
                    self.state = RegistrationState::Stopping;
                    self.revoked_through = Some(self.generation);
                }
            }
        }
        self.bump()
    }
    pub fn activate(&mut self, revision: u64) -> Result<(), DomainError> {
        if self.revision != revision {
            return Err(DomainError::ConcurrentUpdate);
        }
        if self.state == RegistrationState::Stopping {
            return Err(DomainError::InvalidState("definition is stopping"));
        }
        if self.state == RegistrationState::Released {
            self.generation = self
                .generation
                .checked_add(1)
                .ok_or(DomainError::Internal("generation overflow"))?;
        }
        self.state = RegistrationState::Active;
        self.bump()
    }
    pub fn release(&mut self) -> Result<(), DomainError> {
        if self.state != RegistrationState::Stopping {
            return Err(DomainError::InvalidState("definition is not stopping"));
        }
        self.state = RegistrationState::Released;
        self.bump()
    }
    fn bump(&mut self) -> Result<(), DomainError> {
        self.revision = self
            .revision
            .checked_add(1)
            .ok_or(DomainError::Internal("revision overflow"))?;
        Ok(())
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "../../tests/unit/registration_tests.rs"]
mod tests;
