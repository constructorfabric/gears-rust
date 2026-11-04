//! Trusted management API. Every operation obtains a fresh service scope.
use crate::Error;
use crate::contracts::{ExecutionContract, ExecutionDefinition};
use crate::registration::{Registration, UnregisterOptions};
use crate::{catalog::registry::Registry, error::domain::DomainError, store::JournalStore};
use std::sync::Arc;
/// Workflow registry that persists definitions and binds them to local handlers.
pub struct Registrar {
    /// Journal store holding the definition catalog.
    pub store: JournalStore,
    /// In-process registry of bound definitions.
    pub registry: Arc<Registry>,
}
impl Registrar {
    /// Records `contract` in the catalog and remembers it locally, without handlers.
    pub(crate) async fn register_contract(
        &self,
        contract: ExecutionContract,
    ) -> Result<Registration, Error> {
        let value = self.store.ensure_contract(contract.clone()).await?;
        self.registry.remember(contract)?;
        Ok(value.view())
    }
    /// Records the contract in the catalog and binds the definition's handlers in
    /// this process.
    pub(crate) async fn register(
        &self,
        definition: ExecutionDefinition,
    ) -> Result<Registration, Error> {
        let value = self
            .store
            .ensure_contract(definition.contract().clone())
            .await?;
        if value.state != crate::registration::RegistrationState::Stopping
            && let Some(through) = value.revoked_through
        {
            self.registry.evict(&value.contract.name, through);
        }
        self.registry.bind(
            definition,
            value
                .binding_generation()
                .map_err(DomainError::for_definition)?,
        )?;
        // Binding succeeded. Delivery authorization may recover independently;
        // do not return a misleading failed registration after installing handlers.
        if self
            .store
            .recover_unclaimed_deliveries_for_definition(
                chrono::Utc::now(),
                chrono::Utc::now(),
                Some(&value.contract.name),
            )
            .await
            .is_err()
        {
            tracing::debug!("late binding delivery refresh deferred");
        }
        Ok(value.view())
    }
    /// Current catalog entry of `name`.
    pub(crate) async fn registration(&self, name: &str) -> Result<Registration, Error> {
        Ok(self.store.definition(name).await?.view())
    }
    /// Retires admission of `name`, or drains its work before releasing bindings.
    pub(crate) async fn unregister(
        &self,
        name: &str,
        options: UnregisterOptions,
    ) -> Result<Registration, Error> {
        Ok(self.store.unregister(name, options).await?.view())
    }
    /// Reactivates `name` at `expected_revision`.
    pub(crate) async fn activate(
        &self,
        name: &str,
        expected_revision: u64,
    ) -> Result<Registration, Error> {
        Ok(self.store.activate(name, expected_revision).await?.view())
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "../../tests/unit/registrar_tests.rs"]
mod tests;
