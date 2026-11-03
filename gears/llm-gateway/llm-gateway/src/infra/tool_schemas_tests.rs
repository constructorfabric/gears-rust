//! Tool schema source over `TypesRegistryApi` (in-memory registry).

use std::sync::Arc;

use llm_gateway_sdk::LlmGatewayError;
use serde_json::json;
use toolkit::client_hub::ClientHub;
use types_registry_sdk::TypesRegistryApi;
use types_registry_sdk::testing_platform::MockTypesRegistry;

use super::TypesRegistryToolSchemas;
use crate::domain::ports::ToolSchemaSource;
use crate::domain::test_support::ctx;

const WEATHER: &str = "gts.cf.test.tools.weather.v1~";
const MISSING: &str = "gts.cf.test.tools.missing.v1~";

fn source_with(registry: MockTypesRegistry) -> TypesRegistryToolSchemas {
    let hub = Arc::new(ClientHub::new());
    let api: Arc<dyn TypesRegistryApi> = Arc::new(registry);
    hub.register::<dyn TypesRegistryApi>(api);
    TypesRegistryToolSchemas::new(hub)
}

#[tokio::test]
async fn found_schema_is_returned_as_the_resolved_document() {
    let registry = MockTypesRegistry::new();
    registry.seed(
        WEATHER,
        json!({
            "$schema": "http://json-schema.org/draft-07/schema#",
            "$id": format!("gts://{WEATHER}"),
            "type": "object",
            "title": "get_weather"
        }),
    );

    let schema = source_with(registry)
        .schema(&ctx(), WEATHER)
        .await
        .expect("schema is found");

    assert_eq!(schema["type"], json!("object"));
    assert_eq!(schema["title"], json!("get_weather"));
}

#[tokio::test]
async fn unknown_schema_is_schema_not_found_with_the_id() {
    let err = source_with(MockTypesRegistry::new())
        .schema(&ctx(), MISSING)
        .await
        .expect_err("unknown schema");

    assert!(
        matches!(&err, LlmGatewayError::SchemaNotFound { schema_id, .. } if schema_id == MISSING),
        "{err:?}"
    );
}

#[tokio::test]
async fn malformed_schema_id_is_schema_not_found() {
    let err = source_with(MockTypesRegistry::new())
        .schema(&ctx(), "not a gts id")
        .await
        .expect_err("malformed id");
    assert!(
        matches!(err, LlmGatewayError::SchemaNotFound { .. }),
        "{err:?}"
    );
}

#[tokio::test]
async fn missing_registry_client_is_schema_not_found() {
    let source = TypesRegistryToolSchemas::new(Arc::new(ClientHub::new()));
    let err = source.schema(&ctx(), WEATHER).await.expect_err("no client");
    assert!(
        matches!(err, LlmGatewayError::SchemaNotFound { .. }),
        "{err:?}"
    );
}
