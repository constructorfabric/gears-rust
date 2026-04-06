//! Domain service for the Types Registry module.

use std::sync::Arc;

use modkit_macros::domain_model;
use types_registry_sdk::{GtsEntity, ListQuery, RegisterResult};

use super::error::DomainError;
use super::repo::GtsRepository;
use crate::config::TypesRegistryConfig;

/// Domain service for GTS entity operations.
///
/// This service orchestrates business logic and delegates storage
/// operations to the repository.
#[domain_model]
pub struct TypesRegistryService {
    repo: Arc<dyn GtsRepository>,
    config: TypesRegistryConfig,
}

impl TypesRegistryService {
    /// Creates a new `TypesRegistryService` with the given repository and config.
    #[must_use]
    pub fn new(repo: Arc<dyn GtsRepository>, config: TypesRegistryConfig) -> Self {
        Self { repo, config }
    }

    /// Registers GTS entities in batch.
    ///
    /// Validation is controlled by the ready state:
    /// - Configuration phase (not ready): No validation (for internal/system types)
    /// - Ready phase: Full validation
    ///
    /// Returns a `RegisterResult` for each input entity, preserving order.
    #[must_use]
    pub fn register(&self, entities: Vec<serde_json::Value>) -> Vec<RegisterResult> {
        let validate = self.repo.is_ready();
        self.register_internal(entities, validate)
    }

    /// Registers GTS entities in batch with forced validation.
    ///
    /// This method always validates entities regardless of ready state.
    /// Used by REST API to ensure all externally registered entities are validated.
    ///
    /// Returns a `RegisterResult` for each input entity, preserving order.
    #[must_use]
    pub fn register_validated(&self, entities: Vec<serde_json::Value>) -> Vec<RegisterResult> {
        self.register_internal(entities, true)
    }

    /// Internal registration method with explicit validation control.
    fn register_internal(
        &self,
        entities: Vec<serde_json::Value>,
        validate: bool,
    ) -> Vec<RegisterResult> {
        let mut results = Vec::with_capacity(entities.len());

        for entity in entities {
            let gts_id = self.extract_gts_id(&entity);
            let result = match self.repo.register(&entity, validate) {
                Ok(registered) => RegisterResult::Ok(registered),
                Err(e) => RegisterResult::Err {
                    gts_id,
                    error: e.into(),
                },
            };
            results.push(result);
        }

        results
    }

    /// Retrieves a single GTS entity by its identifier.
    pub fn get(&self, gts_id: &str) -> Result<GtsEntity, DomainError> {
        self.repo.get(gts_id)
    }

    /// Lists GTS entities matching the given query.
    pub fn list(&self, query: &ListQuery) -> Result<Vec<GtsEntity>, DomainError> {
        self.repo.list(query)
    }

    /// Switches the registry from configuration mode to ready mode.
    ///
    /// This validates all entities in temporary storage and moves them
    /// to persistent storage if validation succeeds.
    ///
    /// # Errors
    ///
    /// Returns `ReadyCommitFailed` with typed `ValidationError` structs
    /// containing the GTS ID and error message for each failing entity.
    pub fn switch_to_ready(&self) -> Result<(), DomainError> {
        use crate::domain::error::ValidationError;
        self.repo.switch_to_ready().map_err(|errors| {
            let typed_errors: Vec<ValidationError> = errors
                .into_iter()
                .map(|s| ValidationError::from_string(&s))
                .collect();
            DomainError::ReadyCommitFailed(typed_errors)
        })
    }

    /// Returns whether the registry is in ready mode.
    #[must_use]
    pub fn is_ready(&self) -> bool {
        self.repo.is_ready()
    }

    /// Extracts the GTS ID from an entity JSON value.
    ///
    /// Strips the `gts://` URI prefix from `$id` fields for JSON Schema compatibility (gts-rust v0.6.0+).
    fn extract_gts_id(&self, entity: &serde_json::Value) -> Option<String> {
        if let Some(obj) = entity.as_object() {
            for field in &self.config.entity_id_fields {
                if let Some(id) = obj.get(field.as_str()).and_then(|v| v.as_str()) {
                    // Strip gts:// prefix from $id field (JSON Schema URI format)
                    let cleaned_id = if field == "$id" {
                        id.strip_prefix("gts://").unwrap_or(id)
                    } else {
                        id
                    };
                    return Some(cleaned_id.to_owned());
                }
            }
        }
        None
    }
}
#[cfg(test)]
#[path = "service_tests.rs"]
mod tests;
