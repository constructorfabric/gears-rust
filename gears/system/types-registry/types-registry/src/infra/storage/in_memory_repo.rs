//! In-memory repository implementation using gts-rust.

use std::sync::atomic::{AtomicBool, Ordering};
use toolkit_gts::GTS_ID_URI_PREFIX;

use gts::{GtsConfig, GtsId, GtsIdPattern, GtsIdSegment, GtsOps};
use parking_lot::Mutex;
use uuid::Uuid;

use super::debug_diagnostics::{
    log_instance_validation_failure, log_registration_failure, log_schema_validation_failure,
};
use crate::domain::error::DomainError;
use crate::domain::model::{GtsEntity, ListQuery, SegmentMatchScope};
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
        let parsed =
            GtsId::try_new(gts_id).map_err(|e| DomainError::invalid_gts_id(e.to_string()))?;

        let segments: Vec<GtsIdSegment> = parsed.segments().to_vec();

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
    /// Strips the GTS URI prefix from `$id` fields for JSON Schema compatibility.
    fn extract_gts_id(&self, entity: &serde_json::Value) -> Option<String> {
        if let Some(obj) = entity.as_object() {
            for field in &self.config.entity_id_fields {
                if let Some(id) = obj.get(field).and_then(|v| v.as_str()) {
                    // Strip GTS URI prefix from $id field (JSON Schema URI format)
                    let cleaned_id = if field == "$id" {
                        id.strip_prefix(GTS_ID_URI_PREFIX).unwrap_or(id)
                    } else {
                        id
                    };
                    return Some(cleaned_id.to_owned());
                }
            }
        }
        None
    }

    /// Checks if an entity matches a pre-parsed wildcard plus the kind filter.
    ///
    /// The pattern is parsed once at the start of [`Self::list`] (so an
    /// invalid pattern fails the whole call rather than silently passing
    /// through every entity) and the resulting [`GtsIdPattern`] is threaded
    /// in here.
    fn matches_query(
        entity: &GtsEntity,
        wildcard: Option<&GtsIdPattern>,
        query: &ListQuery,
    ) -> bool {
        if let Some(wildcard) = wildcard {
            match GtsId::try_new(&entity.gts_id) {
                Ok(gts_id) => {
                    if !gts_id.matches_pattern(wildcard) {
                        return false;
                    }
                }
                // Stored entity has an unparseable GTS id — treat as no-match
                // for any filtered query (it shouldn't have been registered,
                // but better to hide than to crash on rendering).
                Err(_) => return false,
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
            && !segments_to_check.iter().any(|s| s.vendor() == *vendor)
        {
            return false;
        }

        if let Some(ref package) = query.package
            && !segments_to_check.iter().any(|s| s.package() == *package)
        {
            return false;
        }

        if let Some(ref namespace) = query.namespace
            && !segments_to_check
                .iter()
                .any(|s| s.namespace() == *namespace)
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

        GtsId::try_new(&gts_id).map_err(|e| DomainError::invalid_gts_id(e.to_string()))?;

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

        Err(DomainError::not_found_by_id(gts_id))
    }

    // TODO(#1630): replace linear scan with O(1) UUID lookup once gts-rust
    // exposes a UUID-keyed index on `GtsOps`. Today every lookup re-parses
    // each gts_id and recomputes UUID v5 (SHA-1) per entity.
    // https://github.com/constructorfabric/gears-rust/issues/1630
    fn get_by_uuid(&self, id: Uuid) -> Result<GtsEntity, DomainError> {
        let persistent = self.persistent.lock();
        for (gts_id, gts_entity) in persistent.store.items() {
            // UUIDs are deterministic v5 from gts_id; recompute and compare.
            if let Ok(parsed) = GtsId::try_new(gts_id)
                && parsed.to_uuid() == id
            {
                return Self::to_gts_entity(gts_id, &gts_entity.content);
            }
        }
        Err(DomainError::not_found_by_uuid(id))
    }

    fn list(&self, query: &ListQuery) -> Result<Vec<GtsEntity>, DomainError> {
        // Validate the pattern once up front. `None` means "no filter".
        // `Some("")` is a caller bug — it's distinct from `None` but can't
        // mean anything meaningful, and `GtsIdPattern::try_new("")` may not
        // surface a user-friendly diagnostic. An invalid wildcard (multiple
        // `*`s, mid-pattern `*`, segment-boundary violation — see GTS spec
        // section 10) is also a caller bug that previously slipped through
        // as "no filter applied"; surface both as `InvalidQuery` so they
        // can't hide.
        let wildcard = match query.pattern.as_deref() {
            Some("") => {
                return Err(DomainError::invalid_query(
                    "pattern is empty (use `None` to mean \"no filter\")",
                ));
            }
            Some(p) => Some(GtsIdPattern::try_new(p).map_err(|e| {
                DomainError::invalid_query(format!("invalid GTS wildcard pattern `{p}`: {e}"))
            })?),
            None => None,
        };

        let persistent = self.persistent.lock();
        let mut results = Vec::new();

        for (gts_id, gts_entity) in persistent.store.items() {
            if let Ok(entity) = Self::to_gts_entity(gts_id, &gts_entity.content)
                && Self::matches_query(&entity, wildcard.as_ref(), query)
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

        // Collect all GTS IDs from temporary, sorted lexicographically.
        //
        // Lexicographic order on GTS chain ids implies parent-before-child:
        // a parent type-schema id is a strict prefix of its derived schema's
        // id (parent ends with `~`, derived continues past it), and a
        // type-schema id is a strict prefix of any instance declared from
        // it. Walking ids in lex order therefore registers every parent
        // before any of its descendants, in a single pass — no separate
        // schemas-then-instances split needed.
        let sorted_ids: Vec<String> = {
            let temporary = self.temporary.lock();
            let mut ids: Vec<String> = temporary.store.items().map(|(id, _)| id.clone()).collect();
            ids.sort();
            ids
        };

        // Validate all entities in temporary storage
        {
            let mut temporary = self.temporary.lock();
            for gts_id in &sorted_ids {
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

        // Move to persistent in the same lex order (parents before children).
        {
            let mut temporary = self.temporary.lock();
            let mut persistent = self.persistent.lock();

            for gts_id in &sorted_ids {
                if let Some(entity) = temporary.store.get(gts_id) {
                    let content = entity.content.clone();
                    let result = persistent.add_entity(&content, true);
                    if !result.ok {
                        if gts_id.ends_with('~') {
                            log_schema_validation_failure(gts_id, &content, &result.error);
                        } else {
                            log_instance_validation_failure(
                                gts_id,
                                &content,
                                &result.error,
                                &mut persistent,
                            );
                        }
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
mod tests {
    use super::*;
    use serde_json::json;
    use toolkit_gts::{GTS_ID_PREFIX, gts_id, gts_uri};

    const JSON_SCHEMA_DRAFT_07: &str = "http://json-schema.org/draft-07/schema#";
    const USER_CREATED_ID: &str = gts_id!("acme.core.events.user_created.v1~");
    const ORDER_PLACED_ID: &str = gts_id!("globex.core.events.order_placed.v1~");
    const MISSING_ID: &str = gts_id!("fabrikam.pkg.ns.type.v1~");

    fn schema_uri(id: &str) -> String {
        gts_uri!(id)
    }

    fn default_config() -> GtsConfig {
        crate::config::TypesRegistryConfig::default().to_gts_config()
    }

    #[test]
    fn test_register_in_configuration_mode() {
        let repo = InMemoryGtsRepository::new(default_config());

        let entity = json!({
            "$id": schema_uri(USER_CREATED_ID),
            "$schema": JSON_SCHEMA_DRAFT_07,
            "type": "object",
            "properties": {
                "userId": { "type": "string" }
            }
        });

        let result = repo.register(&entity, false);
        assert!(result.is_ok());

        let registered = result.unwrap();
        assert_eq!(registered.gts_id, USER_CREATED_ID);
        assert!(registered.is_type());
    }

    #[test]
    fn test_register_duplicate_identical_succeeds() {
        let repo = InMemoryGtsRepository::new(default_config());

        let entity = json!({
            "$id": schema_uri(USER_CREATED_ID),
            "$schema": JSON_SCHEMA_DRAFT_07,
            "type": "object"
        });

        let result1 = repo.register(&entity, false);
        assert!(result1.is_ok());

        let result2 = repo.register(&entity, false);
        assert!(result2.is_ok(), "Idempotent registration should succeed");
    }

    #[test]
    fn test_register_duplicate_different_content_fails() {
        let repo = InMemoryGtsRepository::new(default_config());

        let entity1 = json!({
            "$id": schema_uri(USER_CREATED_ID),
            "$schema": JSON_SCHEMA_DRAFT_07,
            "type": "object"
        });

        let entity2 = json!({
            "$id": schema_uri(USER_CREATED_ID),
            "$schema": JSON_SCHEMA_DRAFT_07,
            "type": "object",
            "description": "Different content"
        });

        let result1 = repo.register(&entity1, false);
        assert!(result1.is_ok());

        let result2 = repo.register(&entity2, false);
        assert!(matches!(result2, Err(DomainError::AlreadyExists(_))));
    }

    #[test]
    fn test_register_invalid_gts_id_fails() {
        let repo = InMemoryGtsRepository::new(default_config());

        let entity = json!({
            "$id": "invalid-gts-id",
            "$schema": JSON_SCHEMA_DRAFT_07,
            "type": "object"
        });

        let result = repo.register(&entity, false);
        assert!(matches!(result, Err(DomainError::InvalidGtsId(_))));
    }

    #[test]
    fn test_register_missing_gts_id_fails() {
        let repo = InMemoryGtsRepository::new(default_config());

        let entity = json!({
            "$schema": JSON_SCHEMA_DRAFT_07,
            "type": "object"
        });

        let result = repo.register(&entity, false);
        assert!(matches!(result, Err(DomainError::InvalidGtsId(_))));
    }

    #[test]
    fn test_switch_to_ready() {
        let repo = InMemoryGtsRepository::new(default_config());

        let entity = json!({
            "$id": schema_uri(USER_CREATED_ID),
            "$schema": JSON_SCHEMA_DRAFT_07,
            "type": "object",
            "properties": {
                "userId": { "type": "string" }
            }
        });

        repo.register(&entity, false).unwrap();

        assert!(!repo.is_ready());

        let result = repo.switch_to_ready();
        assert!(result.is_ok());
        assert!(repo.is_ready());

        let get_result = repo.get(USER_CREATED_ID);
        assert!(get_result.is_ok());
    }

    #[test]
    fn test_list_default_returns_all() {
        let repo = InMemoryGtsRepository::new(default_config());

        let type1 = json!({
            "$id": schema_uri(USER_CREATED_ID),
            "$schema": JSON_SCHEMA_DRAFT_07,
            "type": "object"
        });
        let type2 = json!({
            "$id": schema_uri(ORDER_PLACED_ID),
            "$schema": JSON_SCHEMA_DRAFT_07,
            "type": "object"
        });

        repo.register(&type1, false).unwrap();
        repo.register(&type2, false).unwrap();
        repo.switch_to_ready().unwrap();

        let results = repo.list(&ListQuery::default()).unwrap();
        assert_eq!(results.len(), 2);
    }

    #[test]
    fn test_get_not_found() {
        let repo = InMemoryGtsRepository::new(default_config());
        repo.switch_to_ready().unwrap();

        let result = repo.get(MISSING_ID);
        assert!(matches!(result, Err(DomainError::NotFound { .. })));
    }

    #[test]
    fn test_register_in_ready_mode() {
        let repo = InMemoryGtsRepository::new(default_config());
        repo.switch_to_ready().unwrap();

        let entity = json!({
            "$id": schema_uri(USER_CREATED_ID),
            "$schema": JSON_SCHEMA_DRAFT_07,
            "type": "object"
        });

        let result = repo.register(&entity, true);
        assert!(result.is_ok());

        let get_result = repo.get(USER_CREATED_ID);
        assert!(get_result.is_ok());
    }

    #[test]
    fn test_register_duplicate_identical_in_ready_mode_succeeds() {
        let repo = InMemoryGtsRepository::new(default_config());
        repo.switch_to_ready().unwrap();

        let entity = json!({
            "$id": schema_uri(USER_CREATED_ID),
            "$schema": JSON_SCHEMA_DRAFT_07,
            "type": "object"
        });

        repo.register(&entity, true).unwrap();
        let result = repo.register(&entity, true);
        assert!(
            result.is_ok(),
            "Idempotent registration should succeed in ready mode"
        );
    }

    #[test]
    fn test_register_duplicate_different_content_in_ready_mode_fails() {
        let repo = InMemoryGtsRepository::new(default_config());
        repo.switch_to_ready().unwrap();

        let entity1 = json!({
            "$id": schema_uri(USER_CREATED_ID),
            "$schema": JSON_SCHEMA_DRAFT_07,
            "type": "object"
        });

        let entity2 = json!({
            "$id": schema_uri(USER_CREATED_ID),
            "$schema": JSON_SCHEMA_DRAFT_07,
            "type": "object",
            "description": "Different content"
        });

        repo.register(&entity1, true).unwrap();
        let result = repo.register(&entity2, true);
        assert!(matches!(result, Err(DomainError::AlreadyExists(_))));
    }

    #[test]
    fn test_exists() {
        let repo = InMemoryGtsRepository::new(default_config());

        let entity = json!({
            "$id": schema_uri(USER_CREATED_ID),
            "$schema": JSON_SCHEMA_DRAFT_07,
            "type": "object"
        });

        repo.register(&entity, false).unwrap();
        repo.switch_to_ready().unwrap();

        assert!(repo.exists(USER_CREATED_ID));
        assert!(!repo.exists(MISSING_ID));
    }

    #[test]
    fn test_list_with_is_type_filter() {
        let repo = InMemoryGtsRepository::new(default_config());

        let type_entity = json!({
            "$id": schema_uri(USER_CREATED_ID),
            "$schema": JSON_SCHEMA_DRAFT_07,
            "type": "object"
        });

        repo.register(&type_entity, false).unwrap();
        repo.switch_to_ready().unwrap();

        let query = ListQuery::default().with_is_type(true);
        let results = repo.list(&query).unwrap();
        assert_eq!(results.len(), 1);

        let query = ListQuery::default().with_is_type(false);
        let results = repo.list(&query).unwrap();
        assert_eq!(results.len(), 0);
    }

    #[test]
    fn test_list_with_pattern_filter() {
        let repo = InMemoryGtsRepository::new(default_config());

        let entity = json!({
            "$id": schema_uri(USER_CREATED_ID),
            "$schema": JSON_SCHEMA_DRAFT_07,
            "type": "object"
        });

        repo.register(&entity, false).unwrap();
        repo.switch_to_ready().unwrap();

        let query = ListQuery::default().with_pattern(format!("{GTS_ID_PREFIX}acme.*"));
        let results = repo.list(&query).unwrap();
        assert_eq!(results.len(), 1);

        let query = ListQuery::default().with_pattern(format!("{GTS_ID_PREFIX}contoso.*"));
        let results = repo.list(&query).unwrap();
        assert_eq!(results.len(), 0);
    }

    #[test]
    fn test_list_with_empty_pattern_returns_invalid_query() {
        // `Some("")` is not equivalent to `None`. Empty pattern is a caller
        // bug (often a default-constructed value or a malformed query
        // string) and must be rejected, not silently treated as "no filter".
        let repo = InMemoryGtsRepository::new(default_config());
        repo.switch_to_ready().unwrap();

        let query = ListQuery::default().with_pattern("");
        match repo.list(&query) {
            Err(DomainError::InvalidQuery(msg)) => {
                assert!(msg.contains("empty"), "msg should mention empty: {msg}");
            }
            Err(e) => panic!("expected InvalidQuery, got {e:?}"),
            Ok(items) => panic!(
                "expected InvalidQuery error; got {} items (silent fall-through)",
                items.len()
            ),
        }
    }

    #[test]
    // Test exists specifically to assert that an invalid wildcard pattern is
    // surfaced as `InvalidQuery`; the literal must stay in source.
    #[allow(unknown_lints, de0901_gts_string_pattern)]
    fn test_list_with_invalid_pattern_returns_invalid_query() {
        // GTS spec section 10: at most one trailing wildcard. Multiple `*` or
        // mid-pattern `*` must be surfaced as InvalidQuery, not silently
        // skipped (which would have made every entity match).
        let repo = InMemoryGtsRepository::new(default_config());
        repo.register(
            &json!({
                "$id": schema_uri(gts_id!("acme.core.events.x.v1~")),
                "$schema": JSON_SCHEMA_DRAFT_07,
                "type": "object"
            }),
            false,
        )
        .unwrap();
        repo.switch_to_ready().unwrap();

        // Multi-wildcard pattern → invalid per spec.
        let invalid_pattern = format!("{GTS_ID_PREFIX}*.*.rg.*");
        let query = ListQuery::default().with_pattern(invalid_pattern.clone());
        match repo.list(&query) {
            Err(DomainError::InvalidQuery(msg)) => {
                assert!(
                    msg.contains(&invalid_pattern),
                    "msg should cite the input: {msg}"
                );
            }
            Err(e) => panic!("expected InvalidQuery, got {e:?}"),
            Ok(items) => panic!(
                "expected InvalidQuery error; got {} items (silent fall-through)",
                items.len()
            ),
        }
    }

    #[test]
    fn test_register_with_description() {
        let repo = InMemoryGtsRepository::new(default_config());

        let entity = json!({
            "$id": schema_uri(USER_CREATED_ID),
            "$schema": JSON_SCHEMA_DRAFT_07,
            "type": "object",
            "description": "A user created event"
        });

        let result = repo.register(&entity, false).unwrap();
        assert_eq!(result.description, Some("A user created event".to_owned()));
    }

    #[test]
    fn test_register_instance() {
        let repo = InMemoryGtsRepository::new(default_config());

        let entity = json!({
            "id": gts_id!("acme.core.events.user_created.v1~acme.core.events.instance.v1"),
            "data": "value"
        });

        let result = repo.register(&entity, false).unwrap();
        assert!(result.is_instance());
    }

    #[test]
    fn test_extract_gts_id_with_gtsid_field() {
        let repo = InMemoryGtsRepository::new(default_config());

        let entity = json!({
            "gtsId": USER_CREATED_ID,
            "$schema": JSON_SCHEMA_DRAFT_07,
            "type": "object"
        });

        let result = repo.register(&entity, false);
        assert!(result.is_ok());
    }

    #[test]
    fn test_extract_gts_id_with_id_field() {
        let repo = InMemoryGtsRepository::new(default_config());

        let entity = json!({
            "id": USER_CREATED_ID,
            "$schema": JSON_SCHEMA_DRAFT_07,
            "type": "object"
        });

        let result = repo.register(&entity, false);
        assert!(result.is_ok());
    }
}
