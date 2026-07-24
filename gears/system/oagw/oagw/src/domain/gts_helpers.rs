//! OAGW-specific GTS identifier helpers.
//!
//! Thin wrappers around the external `gts` crate for formatting and parsing
//! resource GTS identifiers of the form `gts.cf.core.oagw.<type>.v1~<uuid>`.

use crate::domain::error::DomainError;
use oagw_sdk::field;
use toolkit_gts::gts_id;
use uuid::Uuid;

// -- Schema GTS identifiers --
//
// The error-bearing scopes are re-exported from `oagw-sdk` so the SDK and
// impl crate share a single source of truth for the wire `resource_type`
// strings. `PROTOCOL_SCHEMA` stays local because it identifies a config
// type, not an error scope, and never appears in the SDK error surface.
pub use oagw_sdk::gts::{
    AUTH_PLUGIN_SCHEMA, GUARD_PLUGIN_SCHEMA, PROXY_SCHEMA, ROUTE_SCHEMA, TRANSFORM_PLUGIN_SCHEMA,
    UPSTREAM_SCHEMA,
};

pub const PROTOCOL_SCHEMA: &str = gts_id!("cf.core.oagw.protocol.v1~");

// -- Builtin protocol instances --
pub const HTTP_PROTOCOL_ID: &str = gts_id!("cf.core.oagw.protocol.v1~cf.core.oagw.http.v1");
pub const GRPC_PROTOCOL_ID: &str = gts_id!("cf.core.oagw.protocol.v1~cf.core.oagw.grpc.v1");

// -- Builtin auth plugin instances --
pub const NOOP_AUTH_PLUGIN_ID: &str = gts_id!("cf.core.oagw.auth_plugin.v1~cf.core.oagw.noop.v1");
pub const APIKEY_AUTH_PLUGIN_ID: &str =
    gts_id!("cf.core.oagw.auth_plugin.v1~cf.core.oagw.apikey.v1");
pub const BASIC_AUTH_PLUGIN_ID: &str = gts_id!("cf.core.oagw.auth_plugin.v1~cf.core.oagw.basic.v1");
pub const BEARER_AUTH_PLUGIN_ID: &str =
    gts_id!("cf.core.oagw.auth_plugin.v1~cf.core.oagw.bearer.v1");
pub const OAUTH2_CLIENT_CRED_AUTH_PLUGIN_ID: &str =
    gts_id!("cf.core.oagw.auth_plugin.v1~cf.core.oagw.oauth2_client_cred.v1");
pub const OAUTH2_CLIENT_CRED_BASIC_AUTH_PLUGIN_ID: &str =
    gts_id!("cf.core.oagw.auth_plugin.v1~cf.core.oagw.oauth2_client_cred_basic.v1");
pub const OAUTH2_AUTH_CODE_AUTH_PLUGIN_ID: &str =
    gts_id!("cf.core.oagw.auth_plugin.v1~cf.core.oagw.oauth2_auth_code.v1");

// -- Builtin guard plugin instances --
pub const TIMEOUT_GUARD_PLUGIN_ID: &str =
    gts_id!("cf.core.oagw.guard_plugin.v1~cf.core.oagw.timeout.v1");
pub const CORS_GUARD_PLUGIN_ID: &str = gts_id!("cf.core.oagw.guard_plugin.v1~cf.core.oagw.cors.v1");
pub const REQUIRED_HEADERS_GUARD_PLUGIN_ID: &str =
    gts_id!("cf.core.oagw.guard_plugin.v1~cf.core.oagw.required_headers.v1");

// -- Builtin transform plugin instances --
pub const LOGGING_TRANSFORM_PLUGIN_ID: &str =
    gts_id!("cf.core.oagw.transform_plugin.v1~cf.core.oagw.logging.v1");
pub const METRICS_TRANSFORM_PLUGIN_ID: &str =
    gts_id!("cf.core.oagw.transform_plugin.v1~cf.core.oagw.metrics.v1");
pub const REQUEST_ID_TRANSFORM_PLUGIN_ID: &str =
    gts_id!("cf.core.oagw.transform_plugin.v1~cf.core.oagw.request_id.v1");

/// Format an upstream resource as a GTS identifier.
#[must_use]
pub fn format_upstream_gts(id: Uuid) -> String {
    format!("{UPSTREAM_SCHEMA}{}", id.hyphenated())
}

/// Format a route resource as a GTS identifier.
#[must_use]
pub fn format_route_gts(id: Uuid) -> String {
    format!("{ROUTE_SCHEMA}{}", id.hyphenated())
}

/// Parse a resource GTS identifier, extracting the schema and UUID instance.
///
/// Validates and decomposes the full identifier through the `gts` crate.
pub fn parse_resource_gts(s: &str) -> Result<(String, Uuid), DomainError> {
    let parsed = gts::GtsId::try_new(s).map_err(|e| DomainError::Validation {
        field: "gts_id",
        reason: field::INVALID_GTS_FORMAT,
        detail: format!("invalid GTS identifier: {e}"),
        instance: s.to_string(),
    })?;

    let uuid = parsed
        .segments()
        .last()
        .and_then(gts::GtsIdSegment::uuid)
        .ok_or_else(|| DomainError::Validation {
            field: "gts_id",
            reason: field::INVALID_GTS_UUID,
            detail: "GTS identifier must end with an anonymous UUID instance segment".into(),
            instance: s.to_string(),
        })?;
    let schema = parsed
        .get_type_id()
        .ok_or_else(|| DomainError::Validation {
            field: "gts_id",
            reason: field::MISSING_GTS_TILDE,
            detail: "missing type-schema segment in GTS identifier".into(),
            instance: s.to_string(),
        })?;
    Ok((schema, uuid))
}
