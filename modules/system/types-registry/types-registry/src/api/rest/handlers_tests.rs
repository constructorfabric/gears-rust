use super::*;
use crate::infra::InMemoryGtsRepository;
use gts::GtsConfig;
use serde_json::json;

const JSON_SCHEMA_DRAFT_07: &str = "http://json-schema.org/draft-07/schema#";

fn default_config() -> GtsConfig {
    crate::config::TypesRegistryConfig::default().to_gts_config()
}

fn create_service() -> Arc<TypesRegistryService> {
    let repo = Arc::new(InMemoryGtsRepository::new(default_config()));
    Arc::new(TypesRegistryService::new(
        repo,
        crate::config::TypesRegistryConfig::default(),
    ))
}

#[tokio::test]
async fn test_register_entities_returns_503_when_not_ready() {
    let service = create_service();
    // Service is not ready yet

    let req = RegisterEntitiesRequest {
        entities: vec![json!({
            "$id": "gts://gts.acme.core.events.user_created.v1~",
            "$schema": JSON_SCHEMA_DRAFT_07,
            "type": "object"
        })],
    };

    let result = register_entities(Extension(service), Json(req)).await;
    assert!(result.is_err());
}

#[tokio::test]
async fn test_list_entities_returns_503_when_not_ready() {
    let service = create_service();
    // Service is not ready yet

    let query = ListEntitiesQuery::default();
    let result = list_entities(Extension(service), Query(query)).await;
    assert!(result.is_err());
}

#[tokio::test]
async fn test_get_entity_returns_503_when_not_ready() {
    let service = create_service();
    // Service is not ready yet

    let result = get_entity(
        Extension(service),
        Path("gts.acme.core.events.user_created.v1~".to_owned()),
    )
    .await;
    assert!(result.is_err());
}

#[tokio::test]
async fn test_register_entities_handler_when_ready() {
    let service = create_service();
    service.switch_to_ready().unwrap();

    let req = RegisterEntitiesRequest {
        entities: vec![json!({
            "$id": "gts://gts.acme.core.events.user_created.v1~",
            "$schema": JSON_SCHEMA_DRAFT_07,
            "type": "object"
        })],
    };

    let result = register_entities(Extension(service), Json(req)).await;
    assert!(result.is_ok());

    let (status, Json(response)) = result.unwrap();
    assert_eq!(status, StatusCode::OK);
    assert_eq!(response.summary.total, 1);
    assert_eq!(response.summary.succeeded, 1);
    assert_eq!(response.summary.failed, 0);
}

#[tokio::test]
async fn test_list_entities_handler_when_ready() {
    let service = create_service();

    // Register entities via internal API (before ready)
    _ = service.register(vec![
        json!({
            "$id": "gts://gts.acme.core.events.user_created.v1~",
            "$schema": JSON_SCHEMA_DRAFT_07,
            "type": "object"
        }),
        json!({
            "$id": "gts://gts.globex.core.events.order_placed.v1~",
            "$schema": JSON_SCHEMA_DRAFT_07,
            "type": "object"
        }),
    ]);
    service.switch_to_ready().unwrap();

    let query = ListEntitiesQuery::default();
    let result = list_entities(Extension(service), Query(query)).await;
    assert!(result.is_ok());

    let Json(response) = result.unwrap();
    assert_eq!(response.count, 2);
}

#[tokio::test]
async fn test_get_entity_handler_when_ready() {
    let service = create_service();

    // Register entity via internal API (before ready)
    _ = service.register(vec![json!({
        "$id": "gts://gts.acme.core.events.user_created.v1~",
        "$schema": JSON_SCHEMA_DRAFT_07,
        "type": "object"
    })]);
    service.switch_to_ready().unwrap();

    let result = get_entity(
        Extension(service),
        Path("gts.acme.core.events.user_created.v1~".to_owned()),
    )
    .await;
    assert!(result.is_ok());

    let Json(entity) = result.unwrap();
    assert_eq!(entity.gts_id, "gts.acme.core.events.user_created.v1~");
}

#[tokio::test]
async fn test_get_entity_not_found() {
    let service = create_service();
    service.switch_to_ready().unwrap();

    let result = get_entity(
        Extension(service),
        Path("gts.unknown.pkg.ns.type.v1~".to_owned()),
    )
    .await;
    assert!(result.is_err());
}
