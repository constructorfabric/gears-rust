//! Tool schema source over `TypesRegistryApi` (in-memory registry).

use std::sync::Arc;
use std::time::Duration;

use llm_gateway_sdk::LlmGatewayError;
use serde_json::json;
use toolkit::client_hub::ClientHub;
use types_registry_sdk::TypesRegistryApi;
use types_registry_sdk::testing_platform::{Fault, MockTypesRegistry};

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

// ── timeouts.dependency.types_registry ──────────────────────────────────────

fn seeded_slow_source(delay: Duration, timeout: Option<Duration>) -> TypesRegistryToolSchemas {
    let registry = MockTypesRegistry::new();
    registry.seed(
        WEATHER,
        json!({
            "$schema": "http://json-schema.org/draft-07/schema#",
            "$id": format!("gts://{WEATHER}"),
            "type": "object"
        }),
    );
    registry.inject(Fault::any().delay(delay));
    let source = source_with(registry);
    match timeout {
        Some(timeout) => source.with_timeout(timeout),
        None => source,
    }
}

#[tokio::test(start_paused = true)]
#[tracing_test::traced_test]
async fn a_read_over_the_bound_is_schema_not_found_at_the_bound() {
    let source = seeded_slow_source(Duration::from_mins(1), Some(Duration::from_secs(3)));
    let started = tokio::time::Instant::now();

    let err = source.schema(&ctx(), WEATHER).await.expect_err("timed out");

    assert!(
        matches!(&err, LlmGatewayError::SchemaNotFound { schema_id, .. } if schema_id == WEATHER),
        "the existing outage mapping: {err:?}"
    );
    assert_eq!(started.elapsed(), Duration::from_secs(3));
    assert!(logs_contain(WEATHER), "logged with the schema id");
}

#[tokio::test(start_paused = true)]
async fn the_default_bound_is_five_seconds() {
    let source = seeded_slow_source(Duration::from_mins(1), None);
    let started = tokio::time::Instant::now();

    source.schema(&ctx(), WEATHER).await.expect_err("timed out");

    assert_eq!(started.elapsed(), Duration::from_secs(5));
}

#[tokio::test(start_paused = true)]
async fn a_read_answering_just_before_the_bound_succeeds() {
    let source = seeded_slow_source(Duration::from_secs(2), Some(Duration::from_secs(3)));

    let schema = source.schema(&ctx(), WEATHER).await.expect("in time");

    assert_eq!(schema["type"], json!("object"));
}
