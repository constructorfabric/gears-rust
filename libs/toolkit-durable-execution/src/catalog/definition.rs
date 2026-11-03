use crate::contracts::ExecutionContract;
use crate::error::domain::DomainError;
use crate::registration::{Registration, RegistrationState, UnregisterMode};

/// Persisted lifecycle is independent of run execution epochs and delivery fences.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct Definition {
    /// Contract registered under the definition name.
    pub contract: ExecutionContract,
    /// Binding generation, incremented when a released definition is activated again.
    pub generation: u64,
    /// Optimistic concurrency revision, bumped on every lifecycle change.
    pub revision: u64,
    /// Current lifecycle state of the registration.
    pub state: RegistrationState,
    /// Highest generation whose execution is revoked; `None` when nothing is revoked.
    pub revoked_through: Option<u64>,
}
impl Definition {
    /// Creates an active registration at generation and revision zero.
    pub fn new(contract: ExecutionContract) -> Self {
        Self {
            contract,
            generation: 0,
            revision: 0,
            state: RegistrationState::Active,
            revoked_through: None,
        }
    }
    /// Returns the public registration view.
    pub fn view(&self) -> Registration {
        Registration {
            name: self.contract.name.clone(),
            generation: self.generation,
            revision: self.revision,
            state: self.state,
        }
    }
    /// Returns `true` when `generation` is current and not revoked.
    pub fn permits(&self, generation: u64) -> bool {
        generation == self.generation && self.revoked_through.is_none_or(|last| generation > last)
    }
    /// Returns whether `generation` may execute forward work, or compensation when `compensation`
    /// is set.
    pub fn permits_execution(&self, generation: u64, compensation: bool) -> bool {
        if compensation {
            self.permits_compensation(generation)
        } else {
            self.permits(generation)
        }
    }
    /// Returns `true` when `generation` may run compensation, including while the definition is
    /// stopping.
    pub fn permits_compensation(&self, generation: u64) -> bool {
        self.permits(generation)
            || generation == self.generation && self.state == RegistrationState::Stopping
    }
    /// Returns the generation new handler bindings attach to, the next one after release.
    /// Fails with `InvalidState` while stopping a non-saga definition.
    pub fn binding_generation(&self) -> Result<u64, DomainError> {
        // Restart may restore exact saga bindings for reverse cleanup while Stopping.
        if self.state == RegistrationState::Stopping && !self.contract.saga {
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
    /// Retires the definition or starts stopping it and revokes the current generation, depending
    /// on `mode`.
    /// Fails with `ConcurrentUpdate` for a stale `revision`.
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
    /// Reactivates the definition, starting a new generation when it was released.
    /// Fails with `ConcurrentUpdate` for a stale `revision` and `InvalidState` while stopping.
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
    /// Marks a stopping definition released; fails with `InvalidState` otherwise.
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
