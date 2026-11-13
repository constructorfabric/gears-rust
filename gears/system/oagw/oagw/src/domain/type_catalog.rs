//! The GTS entities OAGW owns: 7 Type Schemas and 16 built-in Instances,
//! declared in this crate's collector and published with
//! `gts_publication(crates = [crate])` before the application group's `init`.
//!
//! Every schema is a base type left open (`additionalProperties: true`): an
//! Instance body carries its identifier in `id` plus any other fields; OAGW
//! validates the upstream and route payloads it reads itself.

use gts::GtsInstanceId;
use serde_json::{Value, json};
use toolkit_gts::gts_id;
use toolkit_gts::local::{gts_instance_raw, gts_type_schema};

/// `gts.cf.core.oagw.upstream.v1~`: upstream service definition.
#[gts_type_schema(
    dir_path = "schemas",
    type_id = gts_id!("cf.core.oagw.upstream.v1~"),
    description = "Upstream service definition",
    properties = "id",
    base = true
)]
#[schemars(extend("additionalProperties" = true))]
struct UpstreamV1 {
    /// The instance identifier (base-type contract).
    id: GtsInstanceId,
}

/// `gts.cf.core.oagw.route.v1~`: route definition.
#[gts_type_schema(
    dir_path = "schemas",
    type_id = gts_id!("cf.core.oagw.route.v1~"),
    description = "Route definition",
    properties = "id",
    base = true
)]
#[schemars(extend("additionalProperties" = true))]
struct RouteV1 {
    /// The instance identifier (base-type contract).
    id: GtsInstanceId,
}

/// `gts.cf.core.oagw.protocol.v1~`: protocol type.
#[gts_type_schema(
    dir_path = "schemas",
    type_id = gts_id!("cf.core.oagw.protocol.v1~"),
    description = "Protocol type",
    properties = "id",
    base = true
)]
#[schemars(extend("additionalProperties" = true))]
struct ProtocolV1 {
    /// The instance identifier (base-type contract).
    id: GtsInstanceId,
}

/// `gts.cf.core.oagw.auth_plugin.v1~`: auth plugin category.
#[gts_type_schema(
    dir_path = "schemas",
    type_id = gts_id!("cf.core.oagw.auth_plugin.v1~"),
    description = "Auth plugin category",
    properties = "id",
    base = true
)]
#[schemars(extend("additionalProperties" = true))]
struct AuthPluginV1 {
    /// The instance identifier (base-type contract).
    id: GtsInstanceId,
}

/// `gts.cf.core.oagw.guard_plugin.v1~`: guard plugin category.
#[gts_type_schema(
    dir_path = "schemas",
    type_id = gts_id!("cf.core.oagw.guard_plugin.v1~"),
    description = "Guard plugin category",
    properties = "id",
    base = true
)]
#[schemars(extend("additionalProperties" = true))]
struct GuardPluginV1 {
    /// The instance identifier (base-type contract).
    id: GtsInstanceId,
}

/// `gts.cf.core.oagw.transform_plugin.v1~`: transform plugin category.
#[gts_type_schema(
    dir_path = "schemas",
    type_id = gts_id!("cf.core.oagw.transform_plugin.v1~"),
    description = "Transform plugin category",
    properties = "id",
    base = true
)]
#[schemars(extend("additionalProperties" = true))]
struct TransformPluginV1 {
    /// The instance identifier (base-type contract).
    id: GtsInstanceId,
}

/// `gts.cf.core.oagw.proxy.v1~`: Proxy API (permissions).
#[gts_type_schema(
    dir_path = "schemas",
    type_id = gts_id!("cf.core.oagw.proxy.v1~"),
    description = "Proxy API (permissions)",
    properties = "id",
    base = true
)]
#[schemars(extend("additionalProperties" = true))]
struct ProxyV1 {
    /// The instance identifier (base-type contract).
    id: GtsInstanceId,
}

/// The JSON Schema for the `private_key_jwt` plugin's `auth.config`: `signing_alg` is a closed enum. Upstream create and update
/// enforce each rule of this schema ([`PrivateKeyJwtConfig::parse`]); the
/// schema publishes them.
///
/// [`PrivateKeyJwtConfig::parse`]: crate::domain::plugin::credentials::PrivateKeyJwtConfig::parse
fn private_key_jwt_config_schema() -> Value {
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
        "type": "object",
        "properties": properties,
        "required": [CLIENT_ID_REF_KEY, PRIVATE_KEY_REF_KEY, SIGNING_ALG_KEY],
        "not": {"required": [CLIENT_SECRET_REF_KEY]},
        "oneOf": [
            {"required": [TOKEN_ENDPOINT_KEY], "not": {"required": [ISSUER_URL_KEY]}},
            {"required": [ISSUER_URL_KEY], "not": {"required": [TOKEN_ENDPOINT_KEY]}}
        ]
    })
}

gts_instance_raw!({
    "id": gts_id!("cf.core.oagw.protocol.v1~cf.core.oagw.http.v1"),
    "description": "HTTP protocol"
});
gts_instance_raw!({
    "id": gts_id!("cf.core.oagw.protocol.v1~cf.core.oagw.grpc.v1"),
    "description": "gRPC protocol"
});
gts_instance_raw!({
    "id": gts_id!("cf.core.oagw.auth_plugin.v1~cf.core.oagw.noop.v1"),
    "description": "No-op (passthrough) auth"
});
gts_instance_raw!({
    "id": gts_id!("cf.core.oagw.auth_plugin.v1~cf.core.oagw.apikey.v1"),
    "description": "API key injection"
});
gts_instance_raw!({
    "id": gts_id!("cf.core.oagw.auth_plugin.v1~cf.core.oagw.basic.v1"),
    "description": "HTTP Basic auth"
});
gts_instance_raw!({
    "id": gts_id!("cf.core.oagw.auth_plugin.v1~cf.core.oagw.bearer.v1"),
    "description": "Bearer token"
});
gts_instance_raw!({
    "id": gts_id!("cf.core.oagw.auth_plugin.v1~cf.core.oagw.oauth2_client_cred.v1"),
    "description": "OAuth2 client credentials"
});
gts_instance_raw!({
    "id": gts_id!("cf.core.oagw.auth_plugin.v1~cf.core.oagw.oauth2_client_cred_basic.v1"),
    "description": "OAuth2 client credentials (Basic)"
});
gts_instance_raw!({
    "id": gts_id!("cf.core.oagw.auth_plugin.v1~cf.core.oagw.aws_sigv4.v1"),
    "description": "AWS Signature Version 4"
});
gts_instance_raw!({
    "id": gts_id!("cf.core.oagw.auth_plugin.v1~cf.core.oagw.oauth2_client_cred_private_key_jwt.v1"),
    "description": "OAuth2 client credentials (private_key_jwt)",
    "config_schema": private_key_jwt_config_schema()
});
gts_instance_raw!({
    "id": gts_id!("cf.core.oagw.guard_plugin.v1~cf.core.oagw.timeout.v1"),
    "description": "Request timeout"
});
gts_instance_raw!({
    "id": gts_id!("cf.core.oagw.guard_plugin.v1~cf.core.oagw.cors.v1"),
    "description": "CORS handling"
});
gts_instance_raw!({
    "id": gts_id!("cf.core.oagw.guard_plugin.v1~cf.core.oagw.required_headers.v1"),
    "description": "Required headers enforcement"
});
gts_instance_raw!({
    "id": gts_id!("cf.core.oagw.transform_plugin.v1~cf.core.oagw.logging.v1"),
    "description": "Request/response logging"
});
gts_instance_raw!({
    "id": gts_id!("cf.core.oagw.transform_plugin.v1~cf.core.oagw.metrics.v1"),
    "description": "Prometheus metrics"
});
gts_instance_raw!({
    "id": gts_id!("cf.core.oagw.transform_plugin.v1~cf.core.oagw.request_id.v1"),
    "description": "Request ID injection"
});

#[cfg(test)]
#[path = "type_catalog_tests.rs"]
mod type_catalog_tests;
