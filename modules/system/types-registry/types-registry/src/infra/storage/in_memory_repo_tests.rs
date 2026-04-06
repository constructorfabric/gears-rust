use super::*;
use serde_json::json;

const JSON_SCHEMA_DRAFT_07: &str = "http://json-schema.org/draft-07/schema#";

fn default_config() -> GtsConfig {
    crate::config::TypesRegistryConfig::default().to_gts_config()
}

#[test]
fn test_register_in_configuration_mode() {
    let repo = InMemoryGtsRepository::new(default_config());

    let entity = json!({
        "$id": "gts://gts.acme.core.events.user_created.v1~",
        "$schema": JSON_SCHEMA_DRAFT_07,
        "type": "object",
        "properties": {
            "userId": { "type": "string" }
        }
    });

    let result = repo.register(&entity, false);
    assert!(result.is_ok());

    let registered = result.unwrap();
    assert_eq!(registered.gts_id, "gts.acme.core.events.user_created.v1~");
    assert!(registered.is_type());
}

#[test]
fn test_register_duplicate_identical_succeeds() {
    let repo = InMemoryGtsRepository::new(default_config());

    let entity = json!({
        "$id": "gts://gts.acme.core.events.user_created.v1~",
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
        "$id": "gts://gts.acme.core.events.user_created.v1~",
        "$schema": JSON_SCHEMA_DRAFT_07,
        "type": "object"
    });

    let entity2 = json!({
        "$id": "gts://gts.acme.core.events.user_created.v1~",
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
        "$id": "gts://gts.acme.core.events.user_created.v1~",
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

    let get_result = repo.get("gts.acme.core.events.user_created.v1~");
    assert!(get_result.is_ok());
}

#[test]
fn test_list_with_filters() {
    let repo = InMemoryGtsRepository::new(default_config());

    let type1 = json!({
        "$id": "gts://gts.acme.core.events.user_created.v1~",
        "$schema": JSON_SCHEMA_DRAFT_07,
        "type": "object"
    });
    let type2 = json!({
        "$id": "gts://gts.globex.core.events.order_placed.v1~",
        "$schema": JSON_SCHEMA_DRAFT_07,
        "type": "object"
    });

    repo.register(&type1, false).unwrap();
    repo.register(&type2, false).unwrap();
    repo.switch_to_ready().unwrap();

    let query = ListQuery::default().with_vendor("acme");
    let results = repo.list(&query).unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].vendor(), Some("acme"));

    let query = ListQuery::default();
    let results = repo.list(&query).unwrap();
    assert_eq!(results.len(), 2);
}

#[test]
fn test_get_not_found() {
    let repo = InMemoryGtsRepository::new(default_config());
    repo.switch_to_ready().unwrap();

    let result = repo.get("gts.unknown.pkg.ns.type.v1~");
    assert!(matches!(result, Err(DomainError::NotFound(_))));
}

#[test]
fn test_register_in_ready_mode() {
    let repo = InMemoryGtsRepository::new(default_config());
    repo.switch_to_ready().unwrap();

    let entity = json!({
        "$id": "gts://gts.acme.core.events.user_created.v1~",
        "$schema": JSON_SCHEMA_DRAFT_07,
        "type": "object"
    });

    let result = repo.register(&entity, true);
    assert!(result.is_ok());

    let get_result = repo.get("gts.acme.core.events.user_created.v1~");
    assert!(get_result.is_ok());
}

#[test]
fn test_register_duplicate_identical_in_ready_mode_succeeds() {
    let repo = InMemoryGtsRepository::new(default_config());
    repo.switch_to_ready().unwrap();

    let entity = json!({
        "$id": "gts://gts.acme.core.events.user_created.v1~",
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
        "$id": "gts://gts.acme.core.events.user_created.v1~",
        "$schema": JSON_SCHEMA_DRAFT_07,
        "type": "object"
    });

    let entity2 = json!({
        "$id": "gts://gts.acme.core.events.user_created.v1~",
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
        "$id": "gts://gts.acme.core.events.user_created.v1~",
        "$schema": JSON_SCHEMA_DRAFT_07,
        "type": "object"
    });

    repo.register(&entity, false).unwrap();
    repo.switch_to_ready().unwrap();

    assert!(repo.exists("gts.acme.core.events.user_created.v1~"));
    assert!(!repo.exists("gts.unknown.pkg.ns.type.v1~"));
}

#[test]
fn test_list_with_is_type_filter() {
    let repo = InMemoryGtsRepository::new(default_config());

    let type_entity = json!({
        "$id": "gts://gts.acme.core.events.user_created.v1~",
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
fn test_list_with_package_filter() {
    let repo = InMemoryGtsRepository::new(default_config());

    let entity = json!({
        "$id": "gts://gts.acme.core.events.user_created.v1~",
        "$schema": JSON_SCHEMA_DRAFT_07,
        "type": "object"
    });

    repo.register(&entity, false).unwrap();
    repo.switch_to_ready().unwrap();

    let query = ListQuery::default().with_package("core");
    let results = repo.list(&query).unwrap();
    assert_eq!(results.len(), 1);

    let query = ListQuery::default().with_package("other");
    let results = repo.list(&query).unwrap();
    assert_eq!(results.len(), 0);
}

#[test]
fn test_list_with_namespace_filter() {
    let repo = InMemoryGtsRepository::new(default_config());

    let entity = json!({
        "$id": "gts://gts.acme.core.events.user_created.v1~",
        "$schema": JSON_SCHEMA_DRAFT_07,
        "type": "object"
    });

    repo.register(&entity, false).unwrap();
    repo.switch_to_ready().unwrap();

    let query = ListQuery::default().with_namespace("events");
    let results = repo.list(&query).unwrap();
    assert_eq!(results.len(), 1);

    let query = ListQuery::default().with_namespace("other");
    let results = repo.list(&query).unwrap();
    assert_eq!(results.len(), 0);
}

#[test]
fn test_list_with_pattern_filter() {
    let repo = InMemoryGtsRepository::new(default_config());

    let entity = json!({
        "$id": "gts://gts.acme.core.events.user_created.v1~",
        "$schema": JSON_SCHEMA_DRAFT_07,
        "type": "object"
    });

    repo.register(&entity, false).unwrap();
    repo.switch_to_ready().unwrap();

    let query = ListQuery::default().with_pattern("gts.acme.*");
    let results = repo.list(&query).unwrap();
    assert_eq!(results.len(), 1);

    let query = ListQuery::default().with_pattern("gts.other.*");
    let results = repo.list(&query).unwrap();
    assert_eq!(results.len(), 0);
}

#[test]
fn test_list_with_segment_scope_primary() {
    let repo = InMemoryGtsRepository::new(default_config());

    let entity = json!({
        "$id": "gts://gts.acme.core.events.user_created.v1~",
        "$schema": JSON_SCHEMA_DRAFT_07,
        "type": "object"
    });

    repo.register(&entity, false).unwrap();
    repo.switch_to_ready().unwrap();

    let query = ListQuery::default()
        .with_vendor("acme")
        .with_segment_scope(SegmentMatchScope::Primary);
    let results = repo.list(&query).unwrap();
    assert_eq!(results.len(), 1);
}

#[test]
fn test_register_with_description() {
    let repo = InMemoryGtsRepository::new(default_config());

    let entity = json!({
        "$id": "gts://gts.acme.core.events.user_created.v1~",
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
        "id": "gts.acme.core.events.user_created.v1~acme.core.events.instance.v1",
        "data": "value"
    });

    let result = repo.register(&entity, false).unwrap();
    assert!(result.is_instance());
}

#[test]
fn test_extract_gts_id_with_gtsid_field() {
    let repo = InMemoryGtsRepository::new(default_config());

    let entity = json!({
        "gtsId": "gts.acme.core.events.user_created.v1~",
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
        "id": "gts.acme.core.events.user_created.v1~",
        "$schema": JSON_SCHEMA_DRAFT_07,
        "type": "object"
    });

    let result = repo.register(&entity, false);
    assert!(result.is_ok());
}
