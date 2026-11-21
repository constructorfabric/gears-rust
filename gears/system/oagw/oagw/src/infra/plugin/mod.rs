/// API-key auth plugin.
pub(crate) mod apikey_auth;
/// AWS Signature Version 4 auth plugin.
pub(crate) mod aws_sigv4_auth;
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

use credstore_sdk::{CredStoreClientV1, SecretRef, SecretValue};
use toolkit_security::SecurityContext;

use crate::domain::plugin::PluginError;
use crate::domain::plugin::credentials::{credstore_read_error, secret_missing};

pub(crate) use registry::AuthPluginRegistry;
pub(crate) use registry::GuardPluginRegistry;
pub(crate) use registry::TransformPluginRegistry;

/// The secret behind `secret_ref`, from the auth config `key`, read as the
/// caller. Store errors follow the one credential mapping of every auth
/// plugin ([`credstore_read_error`]); a missing secret is `SecretNotFound`.
/// Each names the key only, never the reference or the store's own text,
/// which may quote it (F0008).
pub(crate) async fn read_credential(
    credstore: &dyn CredStoreClientV1,
    security_context: &SecurityContext,
    key: &'static str,
    secret_ref: &SecretRef,
) -> Result<SecretValue, PluginError> {
    let response = credstore
        .get(security_context, secret_ref)
        .await
        .map_err(|e| credstore_read_error(key, &e))?
        .ok_or_else(|| secret_missing(key))?;
    Ok(response.value)
}
