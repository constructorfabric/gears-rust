//! In-memory repository implementation using gts-rust.

use std::sync::atomic::{AtomicBool, Ordering};

use gts::{GtsConfig, GtsID, GtsIdSegment, GtsOps, GtsWildcard};
use parking_lot::Mutex;
use types_registry_sdk::{GtsEntity, ListQuery, SegmentMatchScope};

use super::debug_diagnostics::{
    log_instance_validation_failure, log_registration_failure, log_schema_validation_failure,
};
use crate::domain::error::DomainError;
use crate::domain::repo::GtsRepository;

/// In-memory repository for GTS entities using gts-rust.
///
/// Implements two-phase storage:
/// - **Configuration phase**: Entities stored in `temporary` without validation
/// - **Ready phase**: Entities validated and stored in `persistent`
///
/// Note: Uses `Mutex` instead of `RwLock` because `GtsOps` contains a
/// `Box<dyn GtsReader>` which is not `Sync`.
pub struct InMemoryGtsRepository {
    /// Temporary storage during configuration phase.
    temporary: Mutex<GtsOps>,
    /// Persistent storage after ready commit.
    persistent: Mutex<GtsOps>,
    /// Flag indicating ready mode.
    is_ready: AtomicBool,
    /// GTS configuration.
    config: GtsConfig,
}

impl InMemoryGtsRepository {
    /// Creates a new in-memory repository with the given GTS configuration.
    #[must_use]
    pub fn new(config: GtsConfig) -> Self {
        Self {
            temporary: Mutex::new(GtsOps::new(None, None, 0)),
            persistent: Mutex::new(GtsOps::new(None, None, 0)),
            is_ready: AtomicBool::new(false),
            config,
        }
    }

    /// Converts a gts-rust entity result to our SDK `GtsEntity`.
    fn to_gts_entity(gts_id: &str, content: &serde_json::Value) -> Result<GtsEntity, DomainError> {
        let parsed = GtsID::new(gts_id).map_err(|e| DomainError::invalid_gts_id(e.to_string()))?;

        let segments: Vec<GtsIdSegment> = parsed.gts_id_segments.clone();

        let is_schema = gts_id.ends_with('~');

        let id = parsed.to_uuid();

        let description = content
            .get("description")
            .and_then(|v| v.as_str())
            .map(ToOwned::to_owned);

        Ok(GtsEntity::new(
            id,
            gts_id.to_owned(),
            segments,
            is_schema,
            content.clone(),
            description,
        ))
    }

    /// Extracts the GTS ID from an entity JSON value using configured fields.
    ///
    /// Strips the `gts://` URI prefix from `$id` fields for JSON Schema compatibility (gts-rust v0.7.0+).
    fn extract_gts_id(&self, entity: &serde_json::Value) -> Option<String> {
        if let Some(obj) = entity.as_object() {
            for field in &self.config.entity_id_fields {
                if let Some(id) = obj.get(field).and_then(|v| v.as_str()) {
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

    /// Checks if an entity matches the given query filters.
    fn matches_query(entity: &GtsEntity, query: &ListQuery) -> bool {
        if let Some(ref pattern) = query.pattern
            && let Ok(wildcard) = GtsWildcard::new(pattern)
        {
            if let Ok(gts_id) = GtsID::new(&entity.gts_id) {
                if !gts_id.wildcard_match(&wildcard) {
                    return false;
                }
            } else {
                return false;
            }
        }

        if let Some(is_type) = query.is_type
            && entity.is_type() != is_type
        {
            return false;
        }

        let segments_to_check: Vec<&GtsIdSegment> = match query.segment_scope {
            SegmentMatchScope::Primary => entity.segments.first().into_iter().collect(),
            SegmentMatchScope::Any => entity.segments.iter().collect(),
        };

        if let Some(ref vendor) = query.vendor
            && !segments_to_check.iter().any(|s| s.vendor == *vendor)
        {
            return false;
        }

        if let Some(ref package) = query.package
            && !segments_to_check.iter().any(|s| s.package == *package)
        {
            return false;
        }

        if let Some(ref namespace) = query.namespace
            && !segments_to_check.iter().any(|s| s.namespace == *namespace)
        {
            return false;
        }

        true
    }
}

impl GtsRepository for InMemoryGtsRepository {
    fn register(
        &self,
        entity: &serde_json::Value,
        validate: bool,
    ) -> Result<GtsEntity, DomainError> {
        let gts_id = self
            .extract_gts_id(entity)
            .ok_or_else(|| DomainError::invalid_gts_id("No GTS ID field found in entity"))?;

        GtsID::new(&gts_id).map_err(|e| DomainError::invalid_gts_id(e.to_string()))?;

        if self.is_ready.load(Ordering::SeqCst) {
            let mut persistent = self.persistent.lock();

            if let Some(existing) = persistent.store.get(&gts_id) {
                if existing.content == *entity {
                    return Self::to_gts_entity(&gts_id, entity);
                }
                return Err(DomainError::already_exists(&gts_id));
            }

            let result = persistent.add_entity(entity, validate);
            if !result.ok {
                // Debug logging for registration failure
                if gts_id.ends_with('~') {
                    log_schema_validation_failure(&gts_id, entity, &result.error);
                } else {
                    log_instance_validation_failure(
                        &gts_id,
                        entity,
                        &result.error,
                        &mut persistent,
                    );
                }
                return Err(DomainError::validation_failed(result.error));
            }

            Self::to_gts_entity(&gts_id, entity)
        } else {
            let mut temporary = self.temporary.lock();

            if let Some(existing) = temporary.store.get(&gts_id) {
                if existing.content == *entity {
                    return Self::to_gts_entity(&gts_id, entity);
                }
                return Err(DomainError::already_exists(&gts_id));
            }

            let result = temporary.add_entity(entity, false);
            if !result.ok {
                // Debug logging for registration failure (even in config phase)
                log_registration_failure(Some(&gts_id), entity, &result.error);
                return Err(DomainError::validation_failed(result.error));
            }

            Self::to_gts_entity(&gts_id, entity)
        }
    }

    fn get(&self, gts_id: &str) -> Result<GtsEntity, DomainError> {
        let mut persistent = self.persistent.lock();

        if let Some(entity) = persistent.store.get(gts_id) {
            return Self::to_gts_entity(gts_id, &entity.content);
        }

        Err(DomainError::not_found(gts_id))
    }

    fn list(&self, query: &ListQuery) -> Result<Vec<GtsEntity>, DomainError> {
        let persistent = self.persistent.lock();
        let mut results = Vec::new();

        for (gts_id, gts_entity) in persistent.store.items() {
            if let Ok(entity) = Self::to_gts_entity(gts_id, &gts_entity.content)
                && Self::matches_query(&entity, query)
            {
                results.push(entity);
            }
        }

        Ok(results)
    }

    fn exists(&self, gts_id: &str) -> bool {
        let mut persistent = self.persistent.lock();
        persistent.store.get(gts_id).is_some()
    }

    fn is_ready(&self) -> bool {
        self.is_ready.load(Ordering::SeqCst)
    }

    fn switch_to_ready(&self) -> Result<(), Vec<String>> {
        let mut errors = Vec::new();

        // Collect all GTS IDs, separating schemas (ending with ~) from instances
        let (schema_ids, instance_ids): (Vec<String>, Vec<String>) = {
            let temporary = self.temporary.lock();
            temporary
                .store
                .items()
                .map(|(id, _)| id.clone())
                .partition(|id| id.ends_with('~'))
        };

        // Validate all entities in temporary storage
        {
            let mut temporary = self.temporary.lock();
            for gts_id in schema_ids.iter().chain(instance_ids.iter()) {
                let result = temporary.validate_entity(gts_id);
                if !result.ok {
                    // Debug logging for validation failure
                    if let Some(entity) = temporary.store.get(gts_id) {
                        let content = entity.content.clone();
                        if gts_id.ends_with('~') {
                            log_schema_validation_failure(gts_id, &content, &result.error);
                        } else {
                            log_instance_validation_failure(
                                gts_id,
                                &content,
                                &result.error,
                                &mut temporary,
                            );
                        }
                    }
                    errors.push(format!("{gts_id}: {}", result.error));
                }
            }
        }

        if !errors.is_empty() {
            return Err(errors);
        }

        // Move to persistent: schemas first, then instances
        // This ensures schemas are available when validating instances
        {
            let mut temporary = self.temporary.lock();
            let mut persistent = self.persistent.lock();

            // Add schemas first (with validation)
            for gts_id in &schema_ids {
                if let Some(entity) = temporary.store.get(gts_id) {
                    let content = entity.content.clone();
                    let result = persistent.add_entity(&content, true);
                    if !result.ok {
                        // Debug logging for schema commit failure
                        log_schema_validation_failure(gts_id, &content, &result.error);
                        errors.push(format!("{gts_id}: {}", result.error));
                    }
                }
            }

            // Then add instances (with validation against already-added schemas)
            for gts_id in &instance_ids {
                if let Some(entity) = temporary.store.get(gts_id) {
                    let content = entity.content.clone();
                    let result = persistent.add_entity(&content, true);
                    if !result.ok {
                        // Debug logging for instance commit failure
                        log_instance_validation_failure(
                            gts_id,
                            &content,
                            &result.error,
                            &mut persistent,
                        );
                        errors.push(format!("{gts_id}: {}", result.error));
                    }
                }
            }
        }

        if !errors.is_empty() {
            return Err(errors);
        }

        self.is_ready.store(true, Ordering::SeqCst);

        Ok(())
    }
}
#[cfg(test)]
#[path = "in_memory_repo_tests.rs"]
mod tests;
