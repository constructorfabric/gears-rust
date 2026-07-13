#![allow(clippy::unwrap_used, clippy::expect_used)]

//! Integration tests for list and query operations.

mod common;

use axum::extract::Json;
use common::create_service;
use serde_json::json;
use toolkit_gts::gts_uri;
use toolkit_gts::{GTS_ID_PREFIX, gts_id};
use types_registry::api::rest::dto::ListEntitiesQuery;
use types_registry::domain::model::ListQuery;

// =============================================================================
// List and Query Tests
// =============================================================================

#[tokio::test]
async fn test_list_with_pattern_filter() {
    let service = create_service();

    let entities = vec![
        json!({ "$id": gts_uri!("acme.core.events.user_created.v1~"), "$schema": "http://json-schema.org/draft-07/schema#", "type": "object" }),
        json!({ "$id": gts_uri!("acme.core.events.user_updated.v1~"), "$schema": "http://json-schema.org/draft-07/schema#", "type": "object" }),
        json!({ "$id": gts_uri!("acme.core.events.order_created.v1~"), "$schema": "http://json-schema.org/draft-07/schema#", "type": "object" }),
        json!({ "$id": gts_uri!("globex.core.events.shipment.v1~"), "$schema": "http://json-schema.org/draft-07/schema#", "type": "object" }),
    ];

    _ = service.register(entities);
    service.switch_to_ready().unwrap();

    // Wildcard limited to one vendor.
    let acme = service
        .list(&ListQuery::default().with_pattern(format!("{GTS_ID_PREFIX}acme.*")))
        .unwrap();
    assert_eq!(acme.len(), 3);
    assert!(acme.iter().all(|e| e.vendor() == Some("acme")));

    // Tighter wildcard: vendor + package.
    let acme_core = service
        .list(&ListQuery::default().with_pattern(format!("{GTS_ID_PREFIX}acme.core.*")))
        .unwrap();
    assert_eq!(acme_core.len(), 3);

    // No matches.
    let none = service
        .list(&ListQuery::default().with_pattern(format!("{GTS_ID_PREFIX}nope.*")))
        .unwrap();
    assert!(none.is_empty());
}

#[tokio::test]
async fn test_list_with_is_type_filter() {
    let service = create_service();

    let type_schema = json!({
        "$id": gts_uri!("acme.core.models.filter_test.v1~"),
        "$schema": "http://json-schema.org/draft-07/schema#",
        "type": "object",
        "properties": { "name": { "type": "string" } }
    });

    _ = service.register(vec![type_schema]);
    service.switch_to_ready().unwrap();

    let instances = vec![
        json!({
            "id": gts_id!("acme.core.models.filter_test.v1~acme.core.instances.i1.v1"),
            "name": "instance1"
        }),
        json!({
            "id": gts_id!("acme.core.models.filter_test.v1~acme.core.instances.i2.v1"),
            "name": "instance2"
        }),
    ];
    _ = service.register(instances);

    let types = service
        .list(&ListQuery::default().with_is_type(true))
        .unwrap();
    assert_eq!(types.len(), 1);
    assert!(types[0].is_type());

    let only_instances = service
        .list(&ListQuery::default().with_is_type(false))
        .unwrap();
    assert_eq!(only_instances.len(), 2);
    assert!(
        only_instances
            .iter()
            .all(types_registry::domain::model::GtsEntity::is_instance)
    );

    let all = service.list(&ListQuery::default()).unwrap();
    assert_eq!(all.len(), 3);
}

// =============================================================================
// REST Handler List Tests
// =============================================================================

#[tokio::test]
async fn test_rest_list_handler_integration() {
    use axum::extract::{Extension, Query};
    use types_registry::api::rest::handlers::list_entities;

    let service = create_service();

    _ = service.register(vec![
        json!({ "$id": gts_uri!("acme.core.events.list_test1.v1~"), "$schema": "http://json-schema.org/draft-07/schema#", "type": "object" }),
        json!({ "$id": gts_uri!("acme.core.events.list_test2.v1~"), "$schema": "http://json-schema.org/draft-07/schema#", "type": "object" }),
    ]);
    service.switch_to_ready().unwrap();

    let query = ListEntitiesQuery {
        pattern: Some(format!("{GTS_ID_PREFIX}acme.*")),
        ..Default::default()
    };

    let result = list_entities(Extension(service), Query(query)).await;
    assert!(result.is_ok());

    let Json(response) = result.unwrap();
    assert_eq!(response.count, 2);
}

#[tokio::test]
async fn test_rest_list_empty_results() {
    use axum::extract::{Extension, Query};
    use types_registry::api::rest::handlers::list_entities;

    let service = create_service();
    service.switch_to_ready().unwrap();

    let query = ListEntitiesQuery {
        pattern: Some(format!("{GTS_ID_PREFIX}nonexistent.*")),
        ..Default::default()
    };

    let result = list_entities(Extension(service), Query(query)).await;
    assert!(result.is_ok());

    let Json(response) = result.unwrap();
    assert_eq!(response.count, 0);
    assert!(response.entities.is_empty());
}

// =============================================================================
// REST Handler Get Tests
// =============================================================================

#[tokio::test]
async fn test_rest_get_handler_integration() {
    use axum::extract::{Extension, Path};
    use types_registry::api::rest::handlers::get_entity;

    let service = create_service();

    _ = service.register(vec![json!({
        "$id": gts_uri!("acme.core.events.get_test.v1~"),
        "$schema": "http://json-schema.org/draft-07/schema#",
        "type": "object",
        "description": "Test entity for GET handler"
    })]);
    service.switch_to_ready().unwrap();

    let result = get_entity(
        Extension(service),
        Path(gts_id!("acme.core.events.get_test.v1~").to_owned()),
    )
    .await;
    assert!(result.is_ok());

    let Json(entity) = result.unwrap();
    assert_eq!(entity.gts_id, gts_id!("acme.core.events.get_test.v1~"));
    assert_eq!(
        entity.description,
        Some("Test entity for GET handler".to_owned())
    );
}

#[tokio::test]
async fn test_rest_get_handler_not_found() {
    use axum::extract::{Extension, Path};
    use types_registry::api::rest::handlers::get_entity;

    let service = create_service();
    service.switch_to_ready().unwrap();

    let result = get_entity(
        Extension(service),
        Path(gts_id!("nonexistent.pkg.ns.type.v1~").to_owned()),
    )
    .await;

    assert!(result.is_err());
}
