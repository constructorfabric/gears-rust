// Created: 2026-09-23 by Constructor Tech
// Updated: 2026-10-06 by Constructor Tech
//! Pure, network-free logic for the Vault / `OpenBao` KV v2 HTTP API: URL
//! construction, request/response JSON shapes, value encoding, and
//! status-code / error-body classification.
//!
//! Kept separate from [`super::service`] (which orchestrates the calls through
//! the [`super::transport::VaultTransport`] port) so this module's logic is
//! unit-testable without a network or a mock server. HTTP status codes are
//! plain `u16`s here: the transport adapter owns the HTTP client types.
//!
//! # What Vault answers
//!
//! Observed against `hashicorp/vault` (dev server) and relied on below:
//!
//! * `GET data/...?version=N` of a version that was never written, was
//!   evicted by `max_versions`, or of a key that does not exist: `404` with
//!   `{"errors":[]}`.
//! * The same read of a **soft-deleted** version: `404` with a body that still
//!   carries `data.data = null` and `data.metadata.deletion_time` set; of a
//!   **destroyed** version: `404` with `data.data = null` and
//!   `data.metadata.destroyed = true`. A `200` with such metadata does not
//!   occur, but is handled the same way (a version without a value is gone).
//! * `GET data/...?version=0` returns the *latest* version, not "version 0".
//!   The service never sends it (see [`super::service`]).
//! * A path whose mount does not exist: `404` with a non-empty `errors` list
//!   (`no handler for route ...`). That is a misconfiguration, not an absent
//!   key, and is reported as an error rather than as "not found".
//! * `DELETE metadata/...` and `POST destroy/...` of a missing key or version:
//!   `204`.
//! * `403` with `{"errors":["permission denied"]}` for a missing, revoked or
//!   under-privileged token.
use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use credstore_sdk::CredStorePluginError;
use serde::{Deserialize, Serialize};
use toolkit_macros::domain_model;

/// `403 Forbidden`.
pub const FORBIDDEN: u16 = 403;
/// `404 Not Found`.
const NOT_FOUND: u16 = 404;
/// `408 Request Timeout`.
const REQUEST_TIMEOUT: u16 = 408;
/// `429 Too Many Requests`.
const TOO_MANY_REQUESTS: u16 = 429;
/// Longest Vault error text kept in an error message or log line.
const MAX_ERROR_TEXT_CHARS: usize = 200;

/// Whether `status` is a `2xx` success.
fn is_success(status: u16) -> bool {
    (200..300).contains(&status)
}

/// Whether `status` is a `5xx` server error.
fn is_server_error(status: u16) -> bool {
    (500..600).contains(&status)
}

/// Whether a response with `status` is worth retrying for an idempotent
/// operation: a server error, a request timeout or a rate limit.
#[must_use]
pub fn is_transient_status(status: u16) -> bool {
    is_server_error(status) || status == REQUEST_TIMEOUT || status == TOO_MANY_REQUESTS
}

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
/// Returns [`CredStorePluginError::Internal`] if the stored value is not valid
/// base64: the entry exists but is not something this plugin wrote (a
/// hand-edited or foreign row), so retrying cannot help. Never echoes the
/// payload.
pub fn decode_value(encoded: &str) -> Result<Vec<u8>, CredStorePluginError> {
    BASE64.decode(encoded).map_err(|_| not_a_written_value())
}

/// The permanent error for an entry that is present but not a value this
/// plugin wrote. A curated message: no URL, no payload.
fn not_a_written_value() -> CredStorePluginError {
    CredStorePluginError::internal(
        "vault credstore plugin: stored entry is not a value written by this plugin",
    )
}

/// Body of a KV v2 write. No `options.cas`: the plugin relies on the
/// gear's PG compare-and-set, never on a store-side one (ADR-0006), so the
/// mount must have `cas_required = false`.
#[domain_model]
#[derive(Serialize)]
pub struct PutRequestBody<'a> {
    pub data: PutData<'a>,
}

#[domain_model]
#[derive(Serialize)]
pub struct PutData<'a> {
    pub value: &'a str,
}

impl<'a> PutRequestBody<'a> {
    /// Builds a write body carrying `value_b64`.
    #[must_use]
    pub fn new(value_b64: &'a str) -> Self {
        Self {
            data: PutData { value: value_b64 },
        }
    }
}

/// Body of a KV v2 `destroy` call: the version numbers to remove.
#[domain_model]
#[derive(Serialize)]
pub struct DestroyRequestBody {
    pub versions: Vec<u64>,
}

/// Body of a KV v2 `POST metadata/...` call that sets the key's own version
/// limit. Vault ignores `options.max_versions` in a data write, so the limit
/// has to be set through the metadata endpoint.
#[domain_model]
#[derive(Serialize)]
pub struct MetadataRequestBody {
    pub max_versions: u32,
}

/// Serializes a request body for the transport.
///
/// # Errors
/// [`CredStorePluginError::Internal`] if serialization fails (not reachable for the
/// plain body types above; mapped rather than unwrapped).
pub fn to_json_body<T: Serialize>(body: &T) -> Result<String, CredStorePluginError> {
    serde_json::to_string(body).map_err(|_| {
        CredStorePluginError::internal("vault credstore plugin: failed to encode request body")
    })
}

/// Shape of a KV v2 read response:
/// `{"data": {"data": {"value": "..."} | null, "metadata": {...}}}`.
#[domain_model]
#[derive(Deserialize)]
pub struct GetResponseBody {
    pub data: GetResponseOuterData,
}

#[domain_model]
#[derive(Deserialize)]
pub struct GetResponseOuterData {
    /// `null` for a soft-deleted or destroyed version.
    #[serde(default)]
    pub data: Option<serde_json::Value>,
    #[serde(default)]
    pub metadata: Option<VersionMetadata>,
}

/// Per-version metadata Vault returns next to the value.
#[domain_model]
#[derive(Deserialize)]
pub struct VersionMetadata {
    /// RFC 3339 time of the soft delete; empty (or `null`) while the version
    /// is live.
    #[serde(default)]
    pub deletion_time: Option<String>,
    #[serde(default)]
    pub destroyed: bool,
}

impl VersionMetadata {
    /// Whether the version was soft-deleted or destroyed.
    fn is_gone(&self) -> bool {
        self.destroyed || self.deletion_time.as_deref().is_some_and(|t| !t.is_empty())
    }
}

/// Parses a successful (`200`) KV v2 read response body and decodes the
/// stored value; `None` when the version carries no value (soft-deleted or
/// destroyed).
///
/// # Errors
/// * [`CredStorePluginError::Internal`] if the body is not a KV v2 read response.
///   Never echoes the raw body (it could carry the secret's base64 form).
/// * [`CredStorePluginError::Internal`] (permanent) if the entry has data but no
///   string `value`, or the value is not valid base64: a version Vault holds
///   but this plugin can never return.
pub fn parse_get_body(body: &str) -> Result<Option<Vec<u8>>, CredStorePluginError> {
    let parsed: GetResponseBody = serde_json::from_str(body).map_err(|_| {
        CredStorePluginError::internal("vault credstore plugin: unexpected read-response shape")
    })?;
    if parsed
        .data
        .metadata
        .as_ref()
        .is_some_and(VersionMetadata::is_gone)
    {
        return Ok(None);
    }
    match parsed.data.data {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(data) => match data.get("value").and_then(serde_json::Value::as_str) {
            Some(value) => decode_value(value).map(Some),
            None => Err(not_a_written_value()),
        },
    }
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
/// [`CredStorePluginError::Internal`] if the body is not the expected shape.
pub fn parse_put_body(body: &str) -> Result<String, CredStorePluginError> {
    let parsed: PutResponseBody = serde_json::from_str(body).map_err(|_| {
        CredStorePluginError::internal("vault credstore plugin: unexpected write-response shape")
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
/// are not yet destroyed. A soft-deleted version (`deletion_time` set,
/// `destroyed = false`) still counts: its data is recoverable until it is
/// destroyed, so a cleanup must destroy it.
///
/// # Errors
/// [`CredStorePluginError::Internal`] if the body is not the expected shape.
pub fn parse_live_versions(body: &str) -> Result<Vec<u64>, CredStorePluginError> {
    let parsed: MetadataResponseBody = serde_json::from_str(body).map_err(|_| {
        CredStorePluginError::internal("vault credstore plugin: unexpected metadata-response shape")
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
/// [`CredStorePluginError::Internal`] if it is not a non-negative integer (the gear
/// only ever passes back what `put` returned).
pub fn parse_version(v: &str) -> Result<u64, CredStorePluginError> {
    v.parse().map_err(|_| {
        CredStorePluginError::internal(
            "vault credstore plugin: value version is not a KV v2 version number",
        )
    })
}

/// Shape of Vault's error body: `{"errors": ["..."]}`.
#[derive(Deserialize)]
struct ErrorBody {
    #[serde(default)]
    errors: Vec<String>,
}

/// The messages of Vault's `errors` list, or an empty list when the body is
/// not that shape.
fn error_messages(body: &str) -> Vec<String> {
    serde_json::from_str::<ErrorBody>(body)
        .map(|b| b.errors)
        .unwrap_or_default()
}

/// Vault's error text from an error response body, made safe for a log line
/// or an error message: control characters and runs of whitespace collapse to
/// one space, every occurrence of a string in `redact` (the secret being
/// written, in its encoded form) is replaced, and the result is cut to
/// 200 characters. Empty when the body carries no error text.
#[must_use]
pub fn error_text(body: &str, redact: &[&str]) -> String {
    let mut text = error_messages(body).join("; ");
    for secret in redact.iter().filter(|s| !s.is_empty()) {
        text = text.replace(secret, "<redacted>");
    }
    let flat: String = text
        .split(|c: char| c.is_whitespace() || c.is_control())
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    flat.chars().take(MAX_ERROR_TEXT_CHARS).collect()
}

/// `404` that means "nothing at this path": Vault answers `{"errors":[]}`
/// for an absent key or version, and a body without `errors` (carrying the
/// version metadata) for a soft-deleted or destroyed one. A `404` that
/// explains itself (`no handler for route ...`: the mount does not exist) is
/// a misconfiguration.
fn is_plain_not_found(status: u16, body: &str) -> bool {
    status == NOT_FOUND && error_messages(body).is_empty()
}

/// Maps a response that is not a success into the plugin error taxonomy.
/// `redact` lists strings that must not appear in the message (the encoded
/// secret of a `put`).
///
/// | Status | Error |
/// |--------|-------|
/// | `5xx`, `408`, `429` | `Unavailable` (with `retry_after` when given) |
/// | `403` | `CredentialsRejected`: the plugin's token is missing, revoked or lacks the policy |
/// | `400` | `Internal` with Vault's error text |
/// | anything else | `Internal` |
#[must_use]
pub fn map_error_status(
    status: u16,
    body: &str,
    retry_after: Option<Duration>,
    redact: &[&str],
) -> CredStorePluginError {
    let text = error_text(body, redact);
    let with_text = |head: String| {
        if text.is_empty() {
            head
        } else {
            format!("{head}: {text}")
        }
    };
    if status == FORBIDDEN {
        return CredStorePluginError::credentials_rejected(
            "vault credstore plugin: backend rejected the plugin's token (HTTP 403); \
             check that the token is valid and its policy covers the mount and path_prefix",
        );
    }
    if is_transient_status(status) {
        let detail = with_text(format!(
            "vault credstore plugin: backend responded with {status}"
        ));
        return CredStorePluginError::unavailable(detail, retry_after);
    }
    if status == 400 {
        return CredStorePluginError::internal(with_text(
            "vault credstore plugin: backend rejected the request (400)".to_owned(),
        ));
    }
    CredStorePluginError::internal(with_text(format!(
        "vault credstore plugin: backend responded with unexpected status {status}"
    )))
}

/// Classifies a `GET ?version=N` response: `200` carries a value (or none,
/// for a deleted version); a plain `404` means the version is missing,
/// soft-deleted or destroyed (`Ok(None)`, never an error); anything else is a
/// backend failure.
///
/// # Errors
/// See [`map_error_status`] and [`parse_get_body`].
pub fn classify_get_response(
    status: u16,
    body: &str,
) -> Result<Option<Vec<u8>>, CredStorePluginError> {
    if is_plain_not_found(status, body) {
        return Ok(None);
    }
    if is_success(status) {
        return parse_get_body(body);
    }
    Err(map_error_status(status, body, None, &[]))
}

/// Classifies a `POST` (write) response: 2xx yields the assigned version,
/// anything else is a backend failure. `encoded_value` is the base64 secret
/// that was sent, scrubbed from any error text.
///
/// # Errors
/// See [`map_error_status`] and [`parse_put_body`].
pub fn classify_put_response(
    status: u16,
    body: &str,
    encoded_value: &str,
) -> Result<String, CredStorePluginError> {
    if is_success(status) {
        return parse_put_body(body);
    }
    Err(map_error_status(status, body, None, &[encoded_value]))
}

/// Classifies a `POST metadata` (key settings) response: `2xx` (Vault answers
/// `204`) succeeds, anything else is a backend failure. Unlike a delete, a
/// `404` is not success: the call creates the metadata, so a `404` means a
/// wrong mount.
///
/// # Errors
/// See [`map_error_status`].
pub fn classify_metadata_write_response(
    status: u16,
    body: &str,
) -> Result<(), CredStorePluginError> {
    if is_success(status) {
        Ok(())
    } else {
        Err(map_error_status(status, body, None, &[]))
    }
}

/// Classifies a `DELETE` metadata or `POST` destroy response: `2xx` or a
/// plain `404` (already gone) both succeed (idempotent).
///
/// # Errors
/// See [`map_error_status`].
pub fn classify_delete_response(status: u16, body: &str) -> Result<(), CredStorePluginError> {
    if is_success(status) || is_plain_not_found(status, body) {
        Ok(())
    } else {
        Err(map_error_status(status, body, None, &[]))
    }
}

/// Classifies a metadata `GET` response used to list versions: a plain `404`
/// (no such key) is an empty list, 2xx parses the live versions.
///
/// # Errors
/// See [`map_error_status`] and [`parse_live_versions`].
pub fn classify_metadata_response(
    status: u16,
    body: &str,
) -> Result<Vec<u64>, CredStorePluginError> {
    if is_plain_not_found(status, body) {
        return Ok(Vec::new());
    }
    if is_success(status) {
        return parse_live_versions(body);
    }
    Err(map_error_status(status, body, None, &[]))
}

/// Classifies a `POST destroy` response: `2xx` or a plain `404` succeed
/// (idempotent).
///
/// # Errors
/// See [`map_error_status`].
pub fn classify_destroy_response(status: u16, body: &str) -> Result<(), CredStorePluginError> {
    classify_delete_response(status, body)
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "wire_tests.rs"]
mod wire_tests;
