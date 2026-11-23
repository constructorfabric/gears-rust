//! What the quota intake asks outside its own store (P-D-268): types-registry for resolution,
//! ancestry and schema validation, and the discovery plugin client of a target. Both are asked
//! before any transaction begins and never inside one.
use std::sync::Arc;

use bss_products_sdk::ProductsQuotaDiscoveryPluginClientV1;
use serde_json::Value;
use toolkit_macros::domain_model;
use toolkit_security::SecurityContext;
use uuid::Uuid;

/// One registered instance as types-registry answers it.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegistryInstance {
    pub gts_id: String,
    pub uuid: Uuid,
    /// The type schema the instance conforms to: its id without the last segment.
    pub type_id: String,
    /// The registry's version of the instance (its managed origin).
    pub resource_version: u64,
    /// The authored document.
    pub content: Value,
}

/// One registered type schema as types-registry answers it.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegistryTypeSchema {
    pub gts_id: String,
    pub uuid: Uuid,
    /// The registry's version of the schema (its managed origin).
    pub resource_version: u64,
    /// The authored schema.
    pub content: Value,
    /// The authored leaf's `x-gts-abstract: true`.
    pub is_abstract: bool,
}

/// What schema validation of one payload answers.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RegistryVerdict {
    /// The payload conforms to the schema.
    Valid,
    /// The payload does not conform; the registry's explanation.
    Invalid(String),
    /// No registered, live schema has this id.
    UnknownSchema,
    /// The live schema does not compile into a validator; the registry's explanation. It rejects
    /// the record that names it and is no outage.
    Uncompilable(String),
}

/// types-registry could not answer: absent from this process, failing, or answering a shape the
/// intake cannot read. Never a refusal of the record (P-D-268: 503 `QUOTA_REGISTRY_UNAVAILABLE`).
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("types-registry unavailable: {0}")]
pub struct RegistryUnavailable(pub String);

/// types-registry as the quota intake reads it (P-D-268): resolution, ancestry and schema
/// validation, as the caller. `Ok(None)` is "not registered" (unknown or deleted); `Err` is an
/// outage.
#[async_trait::async_trait]
pub trait QuotaRegistry: Send + Sync {
    /// The live instance `id`.
    ///
    /// # Errors
    /// [`RegistryUnavailable`] when the registry cannot answer.
    async fn instance(
        &self,
        ctx: &SecurityContext,
        id: &str,
    ) -> Result<Option<RegistryInstance>, RegistryUnavailable>;

    /// The live type schema `id`.
    ///
    /// # Errors
    /// [`RegistryUnavailable`] when the registry cannot answer.
    async fn type_schema(
        &self,
        ctx: &SecurityContext,
        id: &str,
    ) -> Result<Option<RegistryTypeSchema>, RegistryUnavailable>;

    /// Validate `payload` against the live type schema `type_id`. A live schema that does not
    /// compile is the verdict [`RegistryVerdict::Uncompilable`], not an outage.
    ///
    /// # Errors
    /// [`RegistryUnavailable`] when the registry cannot answer.
    async fn validate(
        &self,
        ctx: &SecurityContext,
        type_id: &str,
        payload: &Value,
    ) -> Result<RegistryVerdict, RegistryUnavailable>;
}

/// The discovery client of a target's plugin instance, or none (P-D-268: no adapter is built).
#[domain_model]
pub enum ResolvedDiscovery {
    /// The plugin instance's scoped client.
    Client(Arc<dyn ProductsQuotaDiscoveryPluginClientV1>),
    /// No client is registered for the plugin instance: discovery is closed before any effect.
    Closed,
}

/// Finds the discovery client of a plugin instance, once per discovery.
pub trait QuotaDiscoveryResolver: Send + Sync {
    /// The scoped client registered for `plugin_instance_id`, or [`ResolvedDiscovery::Closed`].
    fn resolve(&self, plugin_instance_id: &str) -> ResolvedDiscovery;
}
