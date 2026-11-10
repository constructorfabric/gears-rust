// Created: 2026-09-23 by Constructor Tech
// Updated: 2026-10-06 by Constructor Tech
//! Vault / `OpenBao` KV v2 value store: turns the plugin operations into KV v2
//! REST calls and classifies the answers. The HTTP exchange itself goes
//! through the [`VaultTransport`] port (the `reqwest` adapter lives in
//! `crate::infra::http`). See the module docs on `wire` for the pure
//! path/status logic and the crate README for the backend key shape.
//!
//! # Retries
//!
//! `get`, `delete_key` and `destroy` (including the metadata read inside
//! `destroy(Below)`) are idempotent and are retried on transient failures
//! (no response, `5xx`, `408`, `429`) under the configured [`RetryPolicy`].
//! `put` is never retried: a failure after the request was sent is ambiguous
//! (Vault may have stored a version), and the gear's write intent already
//! covers that. The same goes for the metadata call that precedes each `put`.
use std::sync::Arc;
use toolkit_macros::domain_model;

use credstore_sdk::{CredStorePluginError, DestroySelector, SecretValue, StoreKey, ValueVersion};
use tracing::{debug, warn};

use super::retry::RetryPolicy;
use super::transport::{HttpMethod, TransportError, VaultRequest, VaultResponse, VaultTransport};
use super::wire;
use crate::config::VaultCredStorePluginConfig;

/// The `max_versions` the plugin sets on every key before each `put`.
///
/// Vault applies the larger of the mount's and the key's limit (both unset
/// means 10), counts destroyed versions against it, and drops everything
/// older than the window on the next write. A key-level 100 keeps the versions
/// the gear still refers to from being evicted, whatever the mount says. It is
/// a constant of the plugin and **MUST NOT** be lowered in a later version:
/// the next write to every existing key would delete the versions beyond the
/// new window.
pub const KEY_MAX_VERSIONS: u32 = 100;

/// Vault / `OpenBao` KV v2 backend client.
///
/// Holds the injected [`VaultTransport`] and the resolved addressing settings
/// (address, mount, path prefix). Authentication (`X-Vault-Token`, optional
/// `X-Vault-Namespace`) is the transport's concern: the token never reaches
/// this layer (see [`crate::config::VaultToken`] for the config-side
/// redaction).
#[domain_model]
pub struct Service {
    transport: Arc<dyn VaultTransport>,
    address: String,
    mount: String,
    path_prefix: String,
    retry: RetryPolicy,
}

impl Service {
    /// Builds a service that talks to the backend through `transport`, using
    /// the addressing and retry settings of `cfg`.
    #[must_use]
    pub fn new(transport: Arc<dyn VaultTransport>, cfg: &VaultCredStorePluginConfig) -> Self {
        Self {
            transport,
            address: cfg.address.clone(),
            mount: cfg.mount.clone(),
            path_prefix: cfg.path_prefix.clone(),
            retry: RetryPolicy::new(&cfg.retry),
        }
    }

    /// Sends `request` through the transport under `policy`.
    ///
    /// A transient failure (no response, `5xx`, `408`, `429`) is retried with
    /// backoff while attempts remain; once they are used up it becomes
    /// [`CredStorePluginError::Unavailable`] (with the server's
    /// `Retry-After` when it gave one). Any other response is returned for
    /// the caller to classify. `redact` lists strings that must not appear
    /// in an error message.
    async fn call(
        &self,
        request: VaultRequest,
        policy: RetryPolicy,
        redact: &[&str],
    ) -> Result<VaultResponse, CredStorePluginError> {
        let mut attempt = 1;
        loop {
            let sent = self.transport.send(request.clone()).await;
            match Self::settle(sent, attempt, redact) {
                Ok(response) => return Ok(response),
                Err(failure) if attempt >= policy.max_attempts() => {
                    Self::log_given_up(attempt);
                    return Err(failure);
                }
                Err(_) => {}
            }
            tokio::time::sleep(policy.jittered_backoff(attempt)).await;
            attempt += 1;
        }
    }

    /// Splits the result of one attempt: `Ok` is a response to classify (a
    /// transient status is not one), `Err` is a transient failure.
    fn settle(
        sent: Result<VaultResponse, TransportError>,
        attempt: u32,
        redact: &[&str],
    ) -> Result<VaultResponse, CredStorePluginError> {
        match sent {
            Ok(response) if !wire::is_transient_status(response.status) => {
                if response.status == wire::FORBIDDEN {
                    Self::log_forbidden();
                }
                Ok(response)
            }
            Ok(response) => {
                debug!(
                    attempt,
                    status = response.status,
                    "vault credstore plugin: transient backend status"
                );
                Err(wire::map_error_status(
                    response.status,
                    &response.body,
                    response.retry_after,
                    redact,
                ))
            }
            Err(error) => {
                debug!(attempt, ?error, "vault credstore plugin: transport failure");
                Err(error.into_sdk_error())
            }
        }
    }

    fn log_forbidden() {
        warn!(
            "vault credstore plugin: Vault answered 403 to the plugin's token (a re-read of its \
             source, where it has one, did not help); check the token and its policy"
        );
    }

    fn log_given_up(attempts: u32) {
        warn!(
            attempts,
            "vault credstore plugin: backend call failed, giving up"
        );
    }

    fn request(method: HttpMethod, url: String, json_body: Option<String>) -> VaultRequest {
        VaultRequest {
            method,
            url,
            json_body,
        }
    }

    /// `(tenant, record)` as the strings used in the KV path.
    fn ids(key: &StoreKey) -> (String, String) {
        (key.tenant_id.0.to_string(), key.record_id.to_string())
    }

    fn data_url(&self, key: &StoreKey) -> String {
        let (t, r) = Self::ids(key);
        wire::full_url(
            &self.address,
            &wire::data_path(&self.mount, &self.path_prefix, &t, &r),
        )
    }

    fn metadata_url(&self, key: &StoreKey) -> String {
        let (t, r) = Self::ids(key);
        wire::full_url(
            &self.address,
            &wire::metadata_path(&self.mount, &self.path_prefix, &t, &r),
        )
    }

    fn destroy_url(&self, key: &StoreKey) -> String {
        let (t, r) = Self::ids(key);
        wire::full_url(
            &self.address,
            &wire::destroy_path(&self.mount, &self.path_prefix, &t, &r),
        )
    }

    /// Reads version `version` of `key` (`GET ...?version=N`); `None` when it
    /// is missing, soft-deleted, destroyed or evicted. Version `0` is never
    /// sent: Vault reads "latest" for it, which would return bytes of a
    /// different version than the one asked for; it was never issued, so the
    /// answer is `None`.
    ///
    /// # Errors
    /// [`CredStorePluginError::Unavailable`] on a network failure, a backend
    /// `5xx`/`429`, or a token Vault rejects; [`CredStorePluginError::Internal`] on
    /// an unexpected response shape, a non-numeric version, or a stored entry
    /// that is not a value this plugin wrote (permanent).
    pub async fn get_value(
        &self,
        key: &StoreKey,
        version: &ValueVersion,
    ) -> Result<Option<SecretValue>, CredStorePluginError> {
        let n = wire::parse_version(version.as_str())?;
        if n == 0 {
            return Ok(None);
        }
        debug!(
            tenant_id = %key.tenant_id.0,
            record_id = %key.record_id,
            version = n,
            "vault credstore plugin: get"
        );
        let url = format!("{}?version={n}", self.data_url(key));

        let response = self
            .call(Self::request(HttpMethod::Get, url, None), self.retry, &[])
            .await?;

        let value = wire::classify_get_response(response.status, &response.body)?;
        Ok(value.map(SecretValue::new))
    }

    /// Writes a new version under `key` (no `cas`) and returns the version
    /// number the backend assigned, as a string. Before the write it sets the
    /// key's own `max_versions` to [`KEY_MAX_VERSIONS`] (`POST metadata`, `204`,
    /// idempotent, allowed before the first write); if that call fails, the
    /// `put` fails and nothing is written. Neither call is retried.
    ///
    /// # Errors
    /// As in [`Self::get_value`].
    pub async fn put_value(
        &self,
        key: &StoreKey,
        value: SecretValue,
    ) -> Result<ValueVersion, CredStorePluginError> {
        debug!(
            tenant_id = %key.tenant_id.0,
            record_id = %key.record_id,
            "vault credstore plugin: put"
        );
        let limit = wire::to_json_body(&wire::MetadataRequestBody {
            max_versions: KEY_MAX_VERSIONS,
        })?;
        let limit_response = self
            .call(
                Self::request(HttpMethod::Post, self.metadata_url(key), Some(limit)),
                RetryPolicy::no_retry(),
                &[],
            )
            .await?;
        wire::classify_metadata_write_response(limit_response.status, &limit_response.body)?;

        let encoded = wire::encode_value(value.as_bytes());
        let body = wire::to_json_body(&wire::PutRequestBody::new(&encoded))?;

        let response = self
            .call(
                Self::request(HttpMethod::Post, self.data_url(key), Some(body)),
                RetryPolicy::no_retry(),
                &[&encoded],
            )
            .await?;

        wire::classify_put_response(response.status, &response.body, &encoded)
            .map(ValueVersion::new)
    }

    /// Deletes the key with all versions by removing the KV v2 metadata
    /// entry. Idempotent: a `404` (nothing to delete) is success.
    ///
    /// # Errors
    /// As in [`Self::get_value`].
    pub async fn delete_key_value(&self, key: &StoreKey) -> Result<(), CredStorePluginError> {
        debug!(
            tenant_id = %key.tenant_id.0,
            record_id = %key.record_id,
            "vault credstore plugin: delete_key"
        );
        let response = self
            .call(
                Self::request(HttpMethod::Delete, self.metadata_url(key), None),
                self.retry,
                &[],
            )
            .await?;

        wire::classify_delete_response(response.status, &response.body)
    }

    /// Destroys versions of `key`: `Below(N)` lists the not-yet-destroyed
    /// versions from the metadata and destroys those older than `N`;
    /// `Exactly(N)` destroys `[N]`. Idempotent; a missing key is success, and
    /// no destroy call is made when there is nothing to destroy.
    ///
    /// # Errors
    /// As in [`Self::get_value`].
    pub async fn destroy_value(
        &self,
        key: &StoreKey,
        selector: &DestroySelector,
    ) -> Result<(), CredStorePluginError> {
        debug!(
            tenant_id = %key.tenant_id.0,
            record_id = %key.record_id,
            ?selector,
            "vault credstore plugin: destroy"
        );
        let versions = match selector {
            // Version 0 was never issued (Vault numbers from 1).
            DestroySelector::Exactly(v) => match wire::parse_version(v.as_str())? {
                0 => Vec::new(),
                n => vec![n],
            },
            DestroySelector::Below(v) => {
                self.versions_below(key, wire::parse_version(v.as_str())?)
                    .await?
            }
        };
        if versions.is_empty() {
            return Ok(());
        }
        let body = wire::to_json_body(&wire::DestroyRequestBody { versions })?;
        let response = self
            .call(
                Self::request(HttpMethod::Post, self.destroy_url(key), Some(body)),
                self.retry,
                &[],
            )
            .await?;

        wire::classify_destroy_response(response.status, &response.body)
    }

    /// The versions of `key` older than `below` that are not destroyed yet,
    /// from the key's metadata (empty when the key does not exist). Versions
    /// are numbered from 1, so nothing is older than 1 and no call is made.
    async fn versions_below(
        &self,
        key: &StoreKey,
        below: u64,
    ) -> Result<Vec<u64>, CredStorePluginError> {
        if below <= 1 {
            return Ok(Vec::new());
        }
        let response = self
            .call(
                Self::request(HttpMethod::Get, self.metadata_url(key), None),
                self.retry,
                &[],
            )
            .await?;
        Ok(
            wire::classify_metadata_response(response.status, &response.body)?
                .into_iter()
                .filter(|v| *v < below)
                .collect(),
        )
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "service_tests.rs"]
mod service_tests;

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "service_retry_tests.rs"]
mod service_retry_tests;
