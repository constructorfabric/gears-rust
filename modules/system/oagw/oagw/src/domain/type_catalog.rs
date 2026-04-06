//! Centralized catalog of all OAGW GTS entities for Types Registry registration.
//!
//! Returns all 21 entities (7 schemas + 14 instances) in a single batch,
//! ready for `TypesRegistryClient::register()`.

use serde_json::{Value, json};

use super::gts_helpers::*;

/// Build a JSON Schema entity with `$id` and `$schema` fields.
///
/// The `$schema` field is required by the GTS store to recognize the entity as
/// a schema (type definition) rather than an instance.
fn schema_entity(gts_id: &str, description: &str) -> Value {
    json!({
        "$id": format!("gts://{gts_id}"),
        "$schema": "http://json-schema.org/draft-07/schema#",
        "description": description,
        "type": "object"
    })
}

/// Build an instance entity with a `$id` field and optional description.
fn instance_entity(gts_id: &str, description: &str) -> Value {
    json!({
        "$id": gts_id,
        "description": description
    })
}

/// Returns all OAGW GTS entities (7 schemas + 13 instances) for batch registration.
pub fn oagw_gts_entities() -> Vec<Value> {
    vec![
        // -- Schemas (7) --
        schema_entity(UPSTREAM_SCHEMA, "Upstream service definition"),
        schema_entity(ROUTE_SCHEMA, "Route definition"),
        schema_entity(PROTOCOL_SCHEMA, "Protocol type"),
        schema_entity(AUTH_PLUGIN_SCHEMA, "Auth plugin category"),
        schema_entity(GUARD_PLUGIN_SCHEMA, "Guard plugin category"),
        schema_entity(TRANSFORM_PLUGIN_SCHEMA, "Transform plugin category"),
        schema_entity(PROXY_SCHEMA, "Proxy API (permissions)"),
        // -- Protocol instances (2) --
        instance_entity(HTTP_PROTOCOL_ID, "HTTP protocol"),
        instance_entity(GRPC_PROTOCOL_ID, "gRPC protocol"),
        // -- Auth plugin instances (6) --
        instance_entity(NOOP_AUTH_PLUGIN_ID, "No-op (passthrough) auth"),
        instance_entity(APIKEY_AUTH_PLUGIN_ID, "API key injection"),
        instance_entity(BASIC_AUTH_PLUGIN_ID, "HTTP Basic auth"),
        instance_entity(BEARER_AUTH_PLUGIN_ID, "Bearer token"),
        instance_entity(
            OAUTH2_CLIENT_CRED_AUTH_PLUGIN_ID,
            "OAuth2 client credentials",
        ),
        instance_entity(
            OAUTH2_CLIENT_CRED_BASIC_AUTH_PLUGIN_ID,
            "OAuth2 client credentials (Basic)",
        ),
        // -- Guard plugin instances (3) --
        instance_entity(TIMEOUT_GUARD_PLUGIN_ID, "Request timeout"),
        instance_entity(CORS_GUARD_PLUGIN_ID, "CORS handling"),
        instance_entity(
            REQUIRED_HEADERS_GUARD_PLUGIN_ID,
            "Required headers enforcement",
        ),
        // -- Transform plugin instances (3) --
        instance_entity(LOGGING_TRANSFORM_PLUGIN_ID, "Request/response logging"),
        instance_entity(METRICS_TRANSFORM_PLUGIN_ID, "Prometheus metrics"),
        instance_entity(REQUEST_ID_TRANSFORM_PLUGIN_ID, "Request ID injection"),
    ]
}

#[cfg(test)]
#[path = "type_catalog_tests.rs"]
mod type_catalog_tests;
