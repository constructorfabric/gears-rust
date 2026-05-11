//! Unit tests for the schema cache (bypassing types-registry via direct seeding).
#![allow(unknown_lints, de0901_gts_string_pattern)]

use event_broker_sdk::{EventBrokerError, internal_test_helpers::*};

#[tokio::test]
async fn validation_succeeds_with_valid_payload() {
    let schema = serde_json::json!({
        "type": "object",
        "required": ["order_id"],
        "properties": { "order_id": { "type": "string" } }
    });
    let type_id = "gts.cf.core.events.event.v1~test.orders.created.v1~";
    let cache = new_bare_schema_cache();
    schema_cache_seed(&cache, type_id, schema).await;

    let good = serde_json::json!({ "order_id": "abc123" });
    assert!(schema_cache_validate(&cache, type_id, &good).await.is_ok());
}

#[tokio::test]
async fn validation_fails_with_invalid_payload() {
    let schema = serde_json::json!({
        "type": "object",
        "required": ["order_id"],
        "properties": { "order_id": { "type": "string" } }
    });
    let type_id = "gts.cf.core.events.event.v1~test.orders.created.v1~";
    let cache = new_bare_schema_cache();
    schema_cache_seed(&cache, type_id, schema).await;

    let bad = serde_json::json!({ "total": 100 });
    assert!(matches!(
        schema_cache_validate(&cache, type_id, &bad).await,
        Err(EventBrokerError::EventDataInvalid { .. })
    ));
}

#[tokio::test]
async fn validate_returns_internal_for_uncached_type() {
    let cache = new_bare_schema_cache();
    let result = schema_cache_validate(&cache, "gts.unknown~", &serde_json::json!({})).await;
    assert!(matches!(result, Err(EventBrokerError::Internal(_))));
}

#[tokio::test]
async fn seeded_schema_is_cached() {
    let type_id = "gts.cf.core.events.event.v1~test.orders.v1~";
    let cache = new_bare_schema_cache();
    assert!(!schema_cache_is_cached(&cache, type_id).await);
    schema_cache_seed(&cache, type_id, serde_json::json!({ "type": "object" })).await;
    assert!(schema_cache_is_cached(&cache, type_id).await);
}
