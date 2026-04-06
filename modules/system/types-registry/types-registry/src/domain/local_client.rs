//! Local client implementing the `TypesRegistryApi` trait.

use std::sync::Arc;

use async_trait::async_trait;
use modkit_macros::domain_model;
use types_registry_sdk::{
    GtsEntity, ListQuery, RegisterResult, TypesRegistryClient, TypesRegistryError,
};

use crate::domain::service::TypesRegistryService;

/// Local client for the Types Registry module.
///
/// This client implements the `TypesRegistryApi` trait and delegates
/// to the domain service. It is registered in the `ClientHub` for
/// inter-module communication.
#[domain_model]
pub struct TypesRegistryLocalClient {
    service: Arc<TypesRegistryService>,
}

impl TypesRegistryLocalClient {
    /// Creates a new local client with the given service.
    #[must_use]
    pub fn new(service: Arc<TypesRegistryService>) -> Self {
        Self { service }
    }
}

#[async_trait]
impl TypesRegistryClient for TypesRegistryLocalClient {
    async fn register(
        &self,
        entities: Vec<serde_json::Value>,
    ) -> Result<Vec<RegisterResult>, TypesRegistryError> {
        Ok(self.service.register(entities))
    }

    async fn list(&self, query: ListQuery) -> Result<Vec<GtsEntity>, TypesRegistryError> {
        self.service.list(&query).map_err(TypesRegistryError::from)
    }

    async fn get(&self, gts_id: &str) -> Result<GtsEntity, TypesRegistryError> {
        self.service.get(gts_id).map_err(TypesRegistryError::from)
    }
}
#[cfg(test)]
#[path = "local_client_tests.rs"]
mod tests;
