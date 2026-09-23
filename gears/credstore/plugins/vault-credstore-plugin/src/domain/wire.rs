//! Pure, network-free logic for the Vault / `OpenBao` KV v2 HTTP API: URL
//! construction, request/response JSON shapes, value encoding, and
//! status-code / error-body classification.
//!
//! Kept separate from [`super::service`] (which owns the `reqwest::Client`
//! and actually makes the calls) so this module's logic is unit-testable
//! without a network or a mock server.
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use credstore_sdk::CredStoreError;
use reqwest::StatusCode;
use serde::{Deserialize, Serialize};

/// Builds the KV v2 **data** path (read/write the current version) for
/// `(tenant_id, value_id)`, relative to `{address}/v1/`.
///
/// Backend key shape (ADR-0006, `credstore_sdk::plugin_api`):
/// `{mount}/data/{path_prefix}/{tenant_id}/{value_id}`.
pub fn data_path(mount: &str, path_prefix: &str, tenant_id: &str, value_id: &str) -> String {
    format!("{mount}/data/{path_prefix}/{tenant_id}/{value_id}")
}

/// Builds the KV v2 **metadata** path (all versions) for
/// `(tenant_id, value_id)`, relative to `{address}/v1/`. Deleting this path
/// removes every version at once — what `CredStorePluginClientV1::delete`
/// needs (the plugin holds no version history of its own).
pub fn metadata_path(mount: &str, path_prefix: &str, tenant_id: &str, value_id: &str) -> String {
    format!("{mount}/metadata/{path_prefix}/{tenant_id}/{value_id}")
}

/// Joins a base address (`http://host:port`, trailing slash tolerated) with
/// a `v1/...` API path into a full request URL.
pub fn full_url(address: &str, api_path: &str) -> String {
    format!("{}/v1/{api_path}", address.trim_end_matches('/'))
}

/// Base64-encodes secret bytes for the KV v2 `data.value` field.
pub fn encode_value(bytes: &[u8]) -> String {
    BASE64.encode(bytes)
}

/// Decodes the KV v2 `data.data.value` field back into secret bytes.
///
/// # Errors
/// Returns [`CredStoreError::Internal`] if the stored value is not valid
/// base64 — a corrupted or hand-edited backend row, not a caller mistake.
/// Never echoes the malformed payload (it may be attacker- or
/// operator-supplied noise, never the secret itself since decoding failed).
pub fn decode_value(encoded: &str) -> Result<Vec<u8>, CredStoreError> {
    BASE64.decode(encoded).map_err(|_| {
        CredStoreError::internal("vault credstore plugin: stored value is not valid base64")
    })
}

/// Body of a KV v2 create-only write: `cas: 0` means "only write if the key
/// currently has no version" — exactly the immutability guarantee
/// `CredStorePluginClientV1::put` must provide (ADR-0006).
#[derive(Serialize)]
pub struct PutRequestBody {
    pub options: PutOptions,
    pub data: PutData,
}

#[derive(Serialize)]
pub struct PutOptions {
    pub cas: u64,
}

#[derive(Serialize)]
pub struct PutData {
    pub value: String,
}

impl PutRequestBody {
    /// Builds a create-only (`cas: 0`) write body carrying `value_b64`.
    #[must_use]
    pub fn create_only(value_b64: String) -> Self {
        Self {
            options: PutOptions { cas: 0 },
            data: PutData { value: value_b64 },
        }
    }
}

/// Shape of a successful KV v2 read response: `{"data": {"data": {"value": "..."}}}`.
#[derive(Deserialize)]
pub struct GetResponseBody {
    pub data: GetResponseOuterData,
}

#[derive(Deserialize)]
pub struct GetResponseOuterData {
    pub data: GetResponseInnerData,
}

#[derive(Deserialize)]
pub struct GetResponseInnerData {
    pub value: String,
}

/// Parses a successful (`200`) KV v2 read response body and decodes the
/// stored value.
///
/// # Errors
/// [`CredStoreError::Internal`] if the body is not the expected KV v2 shape,
/// or the stored value is not valid base64. Never echoes the raw body (it
/// could carry the secret's base64 form) or the request's Vault token.
pub fn parse_get_body(body: &str) -> Result<Vec<u8>, CredStoreError> {
    let parsed: GetResponseBody = serde_json::from_str(body).map_err(|_| {
        CredStoreError::internal("vault credstore plugin: unexpected read-response shape")
    })?;
    decode_value(&parsed.data.data.value)
}

/// `true` iff a Vault/`OpenBao` error-response body indicates a KV v2
/// check-and-set mismatch — the signal `put` needs to distinguish "value
/// already exists" (immutability conflict) from any other failure. Vault's
/// wire format is `{"errors": ["check-and-set parameter did not match the
/// current version"]}`; matched case-insensitively on the stable substring
/// rather than the full sentence, since the exact wording is not a
/// documented API contract.
pub fn is_cas_conflict_body(body: &str) -> bool {
    body.to_ascii_lowercase().contains("check-and-set")
}

/// Classifies a non-2xx status into the SDK's stable error taxonomy,
/// without ever including the response body (which may carry request
/// echoes) or any header in the resulting message.
fn map_error_status(status: StatusCode) -> CredStoreError {
    if status.is_server_error() || status == StatusCode::REQUEST_TIMEOUT {
        CredStoreError::service_unavailable(format!(
            "vault credstore plugin: backend responded with {status}"
        ))
    } else {
        CredStoreError::internal(format!(
            "vault credstore plugin: backend responded with unexpected status {status}"
        ))
    }
}

/// Classifies a `GET` (read) response: `200` carries a value, `404` means no
/// entry (`Ok(None)`, never an error — see `credstore_sdk::plugin_api`),
/// anything else is a backend failure.
pub fn classify_get_response(
    status: StatusCode,
    body: &str,
) -> Result<Option<Vec<u8>>, CredStoreError> {
    if status == StatusCode::NOT_FOUND {
        return Ok(None);
    }
    if status.is_success() {
        return parse_get_body(body).map(Some);
    }
    Err(map_error_status(status))
}

/// Classifies a `POST` (create-only write) response: any 2xx is success,
/// a `400` whose body reports a check-and-set mismatch is
/// [`CredStoreError::Conflict`] (an id the plugin already holds — the
/// immutability guard the fence-key bootstrap relies on), anything else is a
/// backend failure.
pub fn classify_put_response(status: StatusCode, body: &str) -> Result<(), CredStoreError> {
    if status.is_success() {
        return Ok(());
    }
    if status == StatusCode::BAD_REQUEST && is_cas_conflict_body(body) {
        return Err(CredStoreError::Conflict);
    }
    Err(map_error_status(status))
}

/// Classifies a `DELETE` response: `2xx` or `404` (already gone) both
/// succeed — deleting an id the plugin does not hold is success
/// (idempotent, see `credstore_sdk::plugin_api`).
pub fn classify_delete_response(status: StatusCode) -> Result<(), CredStoreError> {
    if status.is_success() || status == StatusCode::NOT_FOUND {
        Ok(())
    } else {
        Err(map_error_status(status))
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "wire_tests.rs"]
mod wire_tests;
