/// API-key auth plugin.
pub(crate) mod apikey_auth;
/// No-op auth plugin.
pub(crate) mod noop_auth;
/// OAuth2 client-credentials auth plugin.
pub(crate) mod oauth2_client_cred_auth;
/// Plugin registries keyed by GTS plugin ID.
pub(crate) mod registry;
/// Transform plugin that injects a request ID header.
pub(crate) mod request_id_transform;
/// Guard plugin that requires configured headers on the request.
pub(crate) mod required_headers_guard;

pub(crate) use registry::AuthPluginRegistry;
pub(crate) use registry::GuardPluginRegistry;
pub(crate) use registry::TransformPluginRegistry;
