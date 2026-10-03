//! Vault / `OpenBao` KV v2 value store: turns the plugin operations into KV v2
//! REST calls and classifies the answers. The HTTP exchange itself goes
//! through the [`VaultTransport`] port (the `reqwest` adapter lives in
//! `crate::infra::http`). See the module docs on [`super::wire`] for the pure
//! path/status logic and [`crate`]'s README for the backend key shape.
use std::sync::Arc;

use credstore_sdk::{CredStoreError, DestroySelector, SecretValue, StoreKey, ValueVersion};

use super::transport::{HttpMethod, TransportError, VaultRequest, VaultResponse, VaultTransport};
use super::wire;
use crate::config::VaultCredStorePluginConfig;

/// Vault / `OpenBao` KV v2 backend client.
///
/// Holds the injected [`VaultTransport`] and the resolved addressing settings
/// (address, mount, path prefix). Authentication (`X-Vault-Token`, optional
/// `X-Vault-Namespace`) is the transport's concern: the token never reaches
/// this layer (see [`crate::config::VaultToken`] for the config-side
/// redaction).
pub struct Service {
    transport: Arc<dyn VaultTransport>,
    address: String,
    mount: String,
    path_prefix: String,
}

impl Service {
    /// Builds a service that talks to the backend through `transport`, using
    /// the addressing settings of `cfg`.
    #[must_use]
    pub fn new(transport: Arc<dyn VaultTransport>, cfg: &VaultCredStorePluginConfig) -> Self {
        Self {
            transport,
            address: cfg.address.clone(),
            mount: cfg.mount.clone(),
            path_prefix: cfg.path_prefix.clone(),
        }
    }

    /// Sends `method url` (with an optional serialized JSON body) through the
    /// transport; a failure to get any response is a "backend unavailable".
    async fn call(
        &self,
        method: HttpMethod,
        url: String,
        json_body: Option<String>,
    ) -> Result<VaultResponse, CredStoreError> {
        self.transport
            .send(VaultRequest {
                method,
                url,
                json_body,
            })
            .await
            .map_err(TransportError::into_sdk_error)
    }

    fn paths(&self, key: &StoreKey) -> (String, String) {
        let (t, r) = (key.tenant_id.0.to_string(), key.record_id.to_string());
        (
            wire::data_path(&self.mount, &self.path_prefix, &t, &r),
            wire::metadata_path(&self.mount, &self.path_prefix, &t, &r),
        )
    }

    /// Reads version `version` of `key` (`GET ...?version=N`); `None` when it
    /// is missing, deleted or destroyed (a `404`).
    ///
    /// # Errors
    /// [`CredStoreError::ServiceUnavailable`] on a network failure or a
    /// backend `5xx`; [`CredStoreError::Internal`] on an unexpected response
    /// shape or a non-numeric version.
    pub async fn get_value(
        &self,
        key: &StoreKey,
        version: &ValueVersion,
    ) -> Result<Option<SecretValue>, CredStoreError> {
        let n = wire::parse_version(version.as_str())?;
        let (data_path, _) = self.paths(key);
        let url = format!("{}?version={n}", wire::full_url(&self.address, &data_path));

        let response = self.call(HttpMethod::Get, url, None).await?;

        wire::classify_get_response(response.status, &response.body)
            .map(|maybe_bytes| maybe_bytes.map(SecretValue::new))
    }

    /// Writes a new version under `key` (no `cas`) and returns the version
    /// number the backend assigned, as a string.
    ///
    /// # Errors
    /// [`CredStoreError::ServiceUnavailable`] / [`CredStoreError::Internal`]
    /// as in [`Self::get_value`].
    pub async fn put_value(
        &self,
        key: &StoreKey,
        value: SecretValue,
    ) -> Result<ValueVersion, CredStoreError> {
        let (data_path, _) = self.paths(key);
        let url = wire::full_url(&self.address, &data_path);
        let body = wire::to_json_body(&wire::PutRequestBody::new(wire::encode_value(
            value.as_bytes(),
        )))?;

        let response = self.call(HttpMethod::Post, url, Some(body)).await?;

        wire::classify_put_response(response.status, &response.body).map(ValueVersion::new)
    }

    /// Deletes the key with all versions by removing the KV v2 metadata
    /// entry. Idempotent: a `404` (nothing to delete) is success.
    ///
    /// # Errors
    /// [`CredStoreError::ServiceUnavailable`] / [`CredStoreError::Internal`]
    /// as in [`Self::get_value`].
    pub async fn delete_key_value(&self, key: &StoreKey) -> Result<(), CredStoreError> {
        let (_, metadata_path) = self.paths(key);
        let url = wire::full_url(&self.address, &metadata_path);

        let response = self.call(HttpMethod::Delete, url, None).await?;

        wire::classify_delete_response(response.status)
    }

    /// Destroys versions of `key`: `Below(N)` lists the live versions from
    /// the metadata and destroys those older than `N`; `Exactly(N)` destroys
    /// `[N]`. Idempotent; a missing key is success.
    ///
    /// # Errors
    /// [`CredStoreError::ServiceUnavailable`] / [`CredStoreError::Internal`]
    /// as in [`Self::get_value`].
    pub async fn destroy_value(
        &self,
        key: &StoreKey,
        selector: &DestroySelector,
    ) -> Result<(), CredStoreError> {
        let versions = match selector {
            DestroySelector::Exactly(v) => vec![wire::parse_version(v.as_str())?],
            DestroySelector::Below(v) => {
                let n = wire::parse_version(v.as_str())?;
                let (_, metadata_path) = self.paths(key);
                let url = wire::full_url(&self.address, &metadata_path);
                let response = self.call(HttpMethod::Get, url, None).await?;
                wire::classify_metadata_response(response.status, &response.body)?
                    .into_iter()
                    .filter(|x| *x < n)
                    .collect()
            }
        };
        if versions.is_empty() {
            return Ok(());
        }
        let (t, r) = (key.tenant_id.0.to_string(), key.record_id.to_string());
        let path = wire::destroy_path(&self.mount, &self.path_prefix, &t, &r);
        let url = wire::full_url(&self.address, &path);
        let body = wire::to_json_body(&wire::DestroyRequestBody { versions })?;
        let response = self.call(HttpMethod::Post, url, Some(body)).await?;

        wire::classify_destroy_response(response.status)
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "service_tests.rs"]
mod service_tests;
