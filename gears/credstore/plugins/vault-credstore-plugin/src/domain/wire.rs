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

/// Builds the KV v2 **data** path (read/write a version) for the record key
/// `(tenant_id, record_id)`, relative to `{address}/v1/`.
///
/// Backend key shape (ADR-0006, `credstore_sdk::plugin_api`):
/// `{mount}/data/{path_prefix}/{tenant_id}/{record_id}`.
pub fn data_path(mount: &str, path_prefix: &str, tenant_id: &str, record_id: &str) -> String {
    format!("{mount}/data/{path_prefix}/{tenant_id}/{record_id}")
}

/// Builds the KV v2 **metadata** path (version list; `DELETE` removes the key
/// with all its versions) for the record key, relative to `{address}/v1/`.
pub fn metadata_path(mount: &str, path_prefix: &str, tenant_id: &str, record_id: &str) -> String {
    format!("{mount}/metadata/{path_prefix}/{tenant_id}/{record_id}")
}

/// Builds the KV v2 **destroy** path (permanently remove listed versions) for
/// the record key, relative to `{address}/v1/`.
pub fn destroy_path(mount: &str, path_prefix: &str, tenant_id: &str, record_id: &str) -> String {
    format!("{mount}/destroy/{path_prefix}/{tenant_id}/{record_id}")
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

/// Body of a KV v2 write. No `options.cas`: the plugin relies on the
/// gear's PG compare-and-set, never on a store-side one (ADR-0006), so the
/// mount must have `cas_required = false`.
#[derive(Serialize)]
pub struct PutRequestBody {
    pub data: PutData,
}

#[derive(Serialize)]
pub struct PutData {
    pub value: String,
}

impl PutRequestBody {
    /// Builds a write body carrying `value_b64`.
    #[must_use]
    pub fn new(value_b64: String) -> Self {
        Self {
            data: PutData { value: value_b64 },
        }
    }
}

/// Body of a KV v2 `destroy` call: the version numbers to remove.
#[derive(Serialize)]
pub struct DestroyRequestBody {
    pub versions: Vec<u64>,
}

/// Shape of a KV v2 read response: `{"data": {"data": {"value": "..."}}}`.
/// `data.data` is `null` for a deleted version on some server versions.
#[derive(Deserialize)]
pub struct GetResponseBody {
    pub data: GetResponseOuterData,
}

#[derive(Deserialize)]
pub struct GetResponseOuterData {
    pub data: Option<GetResponseInnerData>,
}

#[derive(Deserialize)]
pub struct GetResponseInnerData {
    pub value: String,
}

/// Parses a successful (`200`) KV v2 read response body and decodes the
/// stored value; `None` when the version carries no data (deleted).
///
/// # Errors
/// [`CredStoreError::Internal`] if the body is not the expected KV v2 shape,
/// or the stored value is not valid base64. Never echoes the raw body (it
/// could carry the secret's base64 form) or the request's Vault token.
pub fn parse_get_body(body: &str) -> Result<Option<Vec<u8>>, CredStoreError> {
    let parsed: GetResponseBody = serde_json::from_str(body).map_err(|_| {
        CredStoreError::internal("vault credstore plugin: unexpected read-response shape")
    })?;
    parsed.data.data.map(|d| decode_value(&d.value)).transpose()
}

/// Shape of a KV v2 write response: `{"data": {"version": N, ...}}`.
#[derive(Deserialize)]
struct PutResponseBody {
    data: PutResponseData,
}

#[derive(Deserialize)]
struct PutResponseData {
    version: u64,
}

/// Parses the version number the backend assigned from a write response.
///
/// # Errors
/// [`CredStoreError::Internal`] if the body is not the expected shape.
pub fn parse_put_body(body: &str) -> Result<String, CredStoreError> {
    let parsed: PutResponseBody = serde_json::from_str(body).map_err(|_| {
        CredStoreError::internal("vault credstore plugin: unexpected write-response shape")
    })?;
    Ok(parsed.data.version.to_string())
}

/// Shape of a KV v2 metadata read: `{"data": {"versions": {"1": {...}}}}`.
#[derive(Deserialize)]
struct MetadataResponseBody {
    data: MetadataData,
}

#[derive(Deserialize)]
struct MetadataData {
    versions: std::collections::HashMap<String, VersionInfo>,
}

#[derive(Deserialize)]
struct VersionInfo {
    #[serde(default)]
    destroyed: bool,
}

/// Parses a metadata response into the sorted numbers of the versions that
/// are not yet destroyed.
///
/// # Errors
/// [`CredStoreError::Internal`] if the body is not the expected shape.
pub fn parse_live_versions(body: &str) -> Result<Vec<u64>, CredStoreError> {
    let parsed: MetadataResponseBody = serde_json::from_str(body).map_err(|_| {
        CredStoreError::internal("vault credstore plugin: unexpected metadata-response shape")
    })?;
    let mut live: Vec<u64> = parsed
        .data
        .versions
        .iter()
        .filter(|(_, info)| !info.destroyed)
        .filter_map(|(n, _)| n.parse().ok())
        .collect();
    live.sort_unstable();
    Ok(live)
}

/// Parses a version string the gear passed back into a KV v2 version number.
///
/// # Errors
/// [`CredStoreError::Internal`] if it is not a non-negative integer (the gear
/// only ever passes back what `put` returned).
pub fn parse_version(v: &str) -> Result<u64, CredStoreError> {
    v.parse().map_err(|_| {
        CredStoreError::internal(
            "vault credstore plugin: value version is not a KV v2 version number",
        )
    })
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

/// Classifies a `GET ?version=N` response: `200` carries a value; `404` means
/// the version is missing, deleted or destroyed (`Ok(None)`, never an error);
/// anything else is a backend failure.
pub fn classify_get_response(
    status: StatusCode,
    body: &str,
) -> Result<Option<Vec<u8>>, CredStoreError> {
    if status == StatusCode::NOT_FOUND {
        return Ok(None);
    }
    if status.is_success() {
        return parse_get_body(body);
    }
    Err(map_error_status(status))
}

/// Classifies a `POST` (write) response: 2xx yields the assigned version,
/// anything else is a backend failure.
pub fn classify_put_response(status: StatusCode, body: &str) -> Result<String, CredStoreError> {
    if status.is_success() {
        return parse_put_body(body);
    }
    Err(map_error_status(status))
}

/// Classifies a `DELETE` metadata response: `2xx` or `404` (already gone)
/// both succeed (idempotent).
pub fn classify_delete_response(status: StatusCode) -> Result<(), CredStoreError> {
    if status.is_success() || status == StatusCode::NOT_FOUND {
        Ok(())
    } else {
        Err(map_error_status(status))
    }
}

/// Classifies a metadata `GET` response used to list versions: `404` (no such
/// key) is an empty list, 2xx parses the live versions.
pub fn classify_metadata_response(
    status: StatusCode,
    body: &str,
) -> Result<Vec<u64>, CredStoreError> {
    if status == StatusCode::NOT_FOUND {
        return Ok(Vec::new());
    }
    if status.is_success() {
        return parse_live_versions(body);
    }
    Err(map_error_status(status))
}

/// Classifies a `POST destroy` response: `2xx` or `404` succeed (idempotent).
pub fn classify_destroy_response(status: StatusCode) -> Result<(), CredStoreError> {
    classify_delete_response(status)
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "wire_tests.rs"]
mod wire_tests;
