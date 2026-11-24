//! Centralized catalog of all OAGW GTS entities for Types Registry registration.
//!
//! Returns all 23 entities (7 schemas + 16 instances) in a single batch,
//! ready for `TypesRegistryClient::register()`.

use serde_json::{Value, json};
use toolkit_gts::gts_uri;

use super::gts_helpers::*;

/// Build a JSON Schema entity with `$id` and `$schema` fields.
///
/// The `$schema` field is required by the GTS store to recognize the entity as
/// a schema (type definition) rather than an instance.
fn schema_entity(gts_id: &str, description: &str) -> Value {
    json!({
        "$id": gts_uri!(gts_id),
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

/// The `private_key_jwt` plugin's instance, with the JSON Schema of its
/// `auth.config`: `signing_alg` is a closed enum. Upstream create and update
/// enforce each rule of this schema ([`PrivateKeyJwtConfig::parse`]); the
/// schema publishes them.
///
/// [`PrivateKeyJwtConfig::parse`]: crate::domain::plugin::credentials::PrivateKeyJwtConfig::parse
fn private_key_jwt_plugin_entity() -> Value {
    use crate::domain::plugin::credentials::{
        AUDIENCE_KEY, CLIENT_ID_REF_KEY, CLIENT_SECRET_REF_KEY, ISSUER_URL_KEY, KID_KEY,
        MAX_AUDIENCE_LEN, MAX_KID_LEN, PRIVATE_KEY_REF_KEY, SIGNING_ALG_KEY, SigningAlg,
        TOKEN_ENDPOINT_KEY,
    };
    let algs: Vec<&str> = SigningAlg::ALL.iter().map(|a| a.as_str()).collect();
    let mut properties = serde_json::Map::new();
    for (key, schema) in [
        (
            TOKEN_ENDPOINT_KEY,
            json!({"type": "string", "format": "uri"}),
        ),
        (ISSUER_URL_KEY, json!({"type": "string", "format": "uri"})),
        (CLIENT_ID_REF_KEY, json!({"type": "string"})),
        (PRIVATE_KEY_REF_KEY, json!({"type": "string"})),
        (SIGNING_ALG_KEY, json!({"type": "string", "enum": algs})),
        (
            KID_KEY,
            json!({"type": "string", "minLength": 1, "maxLength": MAX_KID_LEN}),
        ),
        (
            AUDIENCE_KEY,
            json!({"type": "string", "minLength": 1, "maxLength": MAX_AUDIENCE_LEN}),
        ),
        ("scopes", json!({"type": "string"})),
    ] {
        properties.insert(key.to_owned(), schema);
    }
    json!({
        "$id": OAUTH2_CLIENT_CRED_PRIVATE_KEY_JWT_AUTH_PLUGIN_ID,
        "description": "OAuth2 client credentials (private_key_jwt)",
        "config_schema": {
            "type": "object",
            "properties": properties,
            "required": [CLIENT_ID_REF_KEY, PRIVATE_KEY_REF_KEY, SIGNING_ALG_KEY],
            "not": {"required": [CLIENT_SECRET_REF_KEY]},
            "oneOf": [
                {"required": [TOKEN_ENDPOINT_KEY], "not": {"required": [ISSUER_URL_KEY]}},
                {"required": [ISSUER_URL_KEY], "not": {"required": [TOKEN_ENDPOINT_KEY]}}
            ]
        }
    })
}

/// Returns all OAGW GTS entities (7 schemas + 15 instances) for batch registration.
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
        // -- Auth plugin instances (8) --
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
        instance_entity(AWS_SIGV4_AUTH_PLUGIN_ID, "AWS Signature Version 4"),
        private_key_jwt_plugin_entity(),
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
mod tests {
    use super::*;
    use toolkit_gts::GTS_ID_URI_PREFIX;

    /// Strip the `gts://` URI prefix that schema `$id` fields use.
    fn strip_gts_uri(id: &str) -> &str {
        id.strip_prefix(GTS_ID_URI_PREFIX).unwrap_or(id)
    }

    #[test]
    fn catalog_returns_exactly_23_entities() {
        let entities = oagw_gts_entities();
        assert_eq!(
            entities.len(),
            23,
            "expected 23 entities (7 schemas + 16 instances)"
        );
    }

    #[test]
    fn all_entities_have_valid_id_fields() {
        for entity in &oagw_gts_entities() {
            let raw_id = entity["$id"]
                .as_str()
                .unwrap_or_else(|| panic!("entity missing $id: {entity}"));
            let id = strip_gts_uri(raw_id);
            assert!(
                gts::GtsId::try_new(id).is_ok(),
                "GTS ID should parse as a concrete GTS ID: {id}"
            );
        }
    }

    #[test]
    fn schemas_use_gts_uri_prefix() {
        for entity in &oagw_gts_entities() {
            if entity.get("$schema").is_some() {
                let id = entity["$id"].as_str().unwrap();
                assert!(
                    id.starts_with(GTS_ID_URI_PREFIX),
                    "Schema $id must use gts:// URI format: {id}"
                );
            }
        }
    }

    #[test]
    fn seven_schemas_and_fifteen_instances() {
        let entities = oagw_gts_entities();
        let schemas: Vec<_> = entities
            .iter()
            .filter(|e| e.get("$schema").is_some())
            .collect();
        let instances: Vec<_> = entities
            .iter()
            .filter(|e| e.get("$schema").is_none())
            .collect();

        assert_eq!(schemas.len(), 7, "expected 7 schemas");
        assert_eq!(instances.len(), 16, "expected 16 instances");
    }

    #[test]
    fn all_gts_ids_have_valid_segment_format() {
        for entity in &oagw_gts_entities() {
            let raw_id = entity["$id"].as_str().unwrap();
            let id = strip_gts_uri(raw_id);
            // Validate via the gts crate
            gts::GtsId::try_new(id).unwrap_or_else(|e| panic!("invalid GTS ID '{id}': {e}"));
        }
    }

    /// The published config schema lists exactly the algorithms the write
    /// check accepts.
    #[test]
    fn private_key_jwt_schema_lists_the_accepted_algorithms() {
        use crate::domain::plugin::credentials::SigningAlg;
        let entity = private_key_jwt_plugin_entity();
        let algs: Vec<&str> = entity["config_schema"]["properties"]["signing_alg"]["enum"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert_eq!(algs, ["ES256", "RS256", "PS256"]);
        for alg in algs {
            assert!(SigningAlg::parse(alg).is_some(), "{alg}");
        }
    }

    /// The schema's length bounds are the write check's, and every key the
    /// write check reads is a published property.
    #[test]
    fn private_key_jwt_schema_matches_the_write_check() {
        use crate::domain::plugin::credentials::{MAX_AUDIENCE_LEN, MAX_KID_LEN};
        let entity = private_key_jwt_plugin_entity();
        let properties = &entity["config_schema"]["properties"];
        assert_eq!(properties["kid"]["maxLength"], MAX_KID_LEN);
        assert_eq!(properties["audience"]["maxLength"], MAX_AUDIENCE_LEN);
        let mut keys: Vec<&str> = properties
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            [
                "audience",
                "client_id_ref",
                "issuer_url",
                "kid",
                "private_key_ref",
                "scopes",
                "signing_alg",
                "token_endpoint"
            ]
        );
    }
}
