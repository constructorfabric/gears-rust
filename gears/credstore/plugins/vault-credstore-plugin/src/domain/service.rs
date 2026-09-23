//! HTTP-backed value store: talks to a Vault / `OpenBao` KV v2 secrets engine
//! over its REST API. See the module docs on [`super::wire`] for the pure
//! path/status logic and [`crate`]'s README for the backend key shape.
use std::time::Duration;

use credstore_sdk::{CredStoreError, SecretValue, TenantId, ValueId};
use reqwest::Client;

use super::wire;
use crate::config::VaultCredStorePluginConfig;

/// Vault / `OpenBao` KV v2 backend client.
///
/// Holds a configured `reqwest::Client` and the resolved connection
/// settings (address, mount, path prefix, token, optional namespace). The
/// token is kept as a plain `String` here (never `Debug`/logged — see
/// [`crate::config::VaultToken`] for the config-side redaction) and is
/// attached to every outbound request as `X-Vault-Token`.
pub struct Service {
    http: Client,
    address: String,
    mount: String,
    path_prefix: String,
    token: String,
    namespace: Option<String>,
}

impl Service {
    /// Builds a service from plugin configuration.
    ///
    /// # Errors
    /// Returns an error if the underlying HTTP client fails to build (e.g.
    /// an invalid TLS configuration) — this constructor performs no network
    /// I/O itself.
    pub fn from_config(cfg: &VaultCredStorePluginConfig) -> anyhow::Result<Self> {
        let http = Client::builder()
            .timeout(Duration::from_secs(cfg.timeout_secs.max(1)))
            .build()?;
        Ok(Self {
            http,
            address: cfg.address.clone(),
            mount: cfg.mount.clone(),
            path_prefix: cfg.path_prefix.clone(),
            token: cfg.token.expose().to_owned(),
            namespace: cfg.namespace.clone(),
        })
    }

    fn apply_headers(&self, builder: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        let builder = builder.header("X-Vault-Token", &self.token);
        match &self.namespace {
            Some(ns) => builder.header("X-Vault-Namespace", ns),
            None => builder,
        }
    }

    /// Reads the value stored at `(tenant_id, value_id)`, or `None` when no
    /// entry exists (a `404` from the backend).
    ///
    /// # Errors
    /// [`CredStoreError::ServiceUnavailable`] on a network failure or a
    /// backend `5xx`; [`CredStoreError::Internal`] on an unexpected response
    /// shape.
    pub async fn get_value(
        &self,
        tenant_id: &TenantId,
        value_id: &ValueId,
    ) -> Result<Option<SecretValue>, CredStoreError> {
        let path = wire::data_path(
            &self.mount,
            &self.path_prefix,
            &tenant_id.0.to_string(),
            &value_id.0.to_string(),
        );
        let url = wire::full_url(&self.address, &path);

        let response = self
            .apply_headers(self.http.get(&url))
            .send()
            .await
            .map_err(|e| map_reqwest_err(&e))?;
        let status = response.status();
        let body = response.text().await.unwrap_or_default();

        wire::classify_get_response(status, &body)
            .map(|maybe_bytes| maybe_bytes.map(SecretValue::new))
    }

    /// Writes a brand-new, immutable entry at `(tenant_id, value_id)` using
    /// a KV v2 create-only (`cas: 0`) write.
    ///
    /// # Errors
    /// [`CredStoreError::Conflict`] if the backend already holds a version
    /// at this path (the check-and-set guard rejects it) —
    /// `CredStorePluginClientV1::put`'s required immutability guarantee.
    /// [`CredStoreError::ServiceUnavailable`] / [`CredStoreError::Internal`]
    /// as in [`Self::get_value`].
    pub async fn put_value(
        &self,
        tenant_id: &TenantId,
        value_id: &ValueId,
        value: SecretValue,
    ) -> Result<(), CredStoreError> {
        let path = wire::data_path(
            &self.mount,
            &self.path_prefix,
            &tenant_id.0.to_string(),
            &value_id.0.to_string(),
        );
        let url = wire::full_url(&self.address, &path);
        let body = wire::PutRequestBody::create_only(wire::encode_value(value.as_bytes()));

        let response = self
            .apply_headers(self.http.post(&url))
            .json(&body)
            .send()
            .await
            .map_err(|e| map_reqwest_err(&e))?;
        let status = response.status();
        let response_body = response.text().await.unwrap_or_default();

        wire::classify_put_response(status, &response_body)
    }

    /// Deletes all versions at `(tenant_id, value_id)` by removing the KV v2
    /// metadata entry. Idempotent: a `404` (nothing to delete) is success.
    ///
    /// # Errors
    /// [`CredStoreError::ServiceUnavailable`] / [`CredStoreError::Internal`]
    /// as in [`Self::get_value`].
    pub async fn delete_value(
        &self,
        tenant_id: &TenantId,
        value_id: &ValueId,
    ) -> Result<(), CredStoreError> {
        let path = wire::metadata_path(
            &self.mount,
            &self.path_prefix,
            &tenant_id.0.to_string(),
            &value_id.0.to_string(),
        );
        let url = wire::full_url(&self.address, &path);

        let response = self
            .apply_headers(self.http.delete(&url))
            .send()
            .await
            .map_err(|e| map_reqwest_err(&e))?;

        wire::classify_delete_response(response.status())
    }
}

/// Maps a `reqwest` transport-level failure (connect refused, timeout, DNS,
/// TLS) to the SDK's "backend unavailable" variant. Deliberately coarse —
/// never includes `reqwest::Error`'s `Display` text, which can embed the
/// request URL; the vendor/priority/address are not secret, but there is no
/// value in taking on that leak surface for a diagnostic string.
fn map_reqwest_err(err: &reqwest::Error) -> CredStoreError {
    let kind = if err.is_timeout() {
        "timeout"
    } else if err.is_connect() {
        "connection failed"
    } else {
        "request failed"
    };
    CredStoreError::service_unavailable(format!("vault credstore plugin: {kind}"))
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "service_tests.rs"]
mod service_tests;
