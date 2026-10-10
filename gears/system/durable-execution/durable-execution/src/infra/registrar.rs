//! Trusted management API. Every operation obtains a fresh service scope.
use crate::{
    domain::{error::DomainError, registry::Registry},
    infra::storage::JournalStore,
};
use async_trait::async_trait;
use durable_execution_sdk::WorkflowRegistry;
use durable_execution_sdk::contracts::{ExecutionContract, ExecutionDefinition};
use durable_execution_sdk::registration::{Registration, UnregisterOptions};
use std::sync::Arc;
use toolkit_canonical_errors::CanonicalError;
pub struct Registrar {
    pub store: JournalStore,
    pub registry: Arc<Registry>,
}
impl Registrar {
    async fn register_contract_inner(
        &self,
        contract: ExecutionContract,
    ) -> Result<Registration, DomainError> {
        let value = self.store.ensure_contract(contract.clone()).await?;
        self.registry.remember(contract)?;
        Ok(value.view())
    }
    async fn bind_definition(
        &self,
        definition: ExecutionDefinition,
    ) -> Result<Registration, DomainError> {
        let value = self.store.ensure_contract(definition.contract()).await?;
        if value.state != durable_execution_sdk::registration::RegistrationState::Stopping
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
    async fn read_registration(&self, name: &str) -> Result<Registration, DomainError> {
        Ok(self.store.definition(name).await?.view())
    }
    async fn unregister_inner(
        &self,
        name: &str,
        options: UnregisterOptions,
    ) -> Result<Registration, DomainError> {
        Ok(self.store.unregister(name, options).await?.view())
    }
    async fn activate_inner(
        &self,
        name: &str,
        expected_revision: u64,
    ) -> Result<Registration, DomainError> {
        Ok(self.store.activate(name, expected_revision).await?.view())
    }
}

/// SDK boundary: each method maps the domain failure to `CanonicalError` once.
#[async_trait]
impl WorkflowRegistry for Registrar {
    async fn register_contract(
        &self,
        contract: ExecutionContract,
    ) -> Result<Registration, CanonicalError> {
        self.register_contract_inner(contract)
            .await
            .map_err(CanonicalError::from)
    }
    async fn register(
        &self,
        definition: ExecutionDefinition,
    ) -> Result<Registration, CanonicalError> {
        self.bind_definition(definition)
            .await
            .map_err(CanonicalError::from)
    }
    async fn registration(&self, name: &str) -> Result<Registration, CanonicalError> {
        self.read_registration(name)
            .await
            .map_err(CanonicalError::from)
    }
    async fn unregister(
        &self,
        name: &str,
        options: UnregisterOptions,
    ) -> Result<Registration, CanonicalError> {
        self.unregister_inner(name, options)
            .await
            .map_err(CanonicalError::from)
    }
    async fn activate(
        &self,
        name: &str,
        expected_revision: u64,
    ) -> Result<Registration, CanonicalError> {
        self.activate_inner(name, expected_revision)
            .await
            .map_err(CanonicalError::from)
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "../../tests/unit/registrar_tests.rs"]
mod tests;
