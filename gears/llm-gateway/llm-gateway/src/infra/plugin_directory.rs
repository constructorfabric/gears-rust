//! Plugin Directory over `LlmGatewayProviderPluginSpecV1` GTS instances.

use std::sync::Arc;

use async_trait::async_trait;
use llm_gateway_sdk::{
    LlmGatewayError, LlmGatewayProviderPluginClientV1, LlmGatewayProviderPluginSpecV1,
};
use tokio::sync::OnceCell;
use toolkit::client_hub::{ClientHub, ClientScope};
use types_registry_sdk::{InstanceQuery, TypesRegistryClient};

use crate::domain::ports::{PluginDirectory, ResolvedPlugin};

/// A registered plugin instance: `(instance id, instance JSON)`.
type PluginInstance = (String, serde_json::Value);

/// Pick the GTS instance id of the plugin serving `provider_type` from the
/// registered plugin instances (`(instance id, instance JSON)` pairs).
///
/// A plugin matches when its `provider_type` equals `provider_type`; among
/// matches the lowest `priority` value wins (toolkit `PluginV1` convention),
/// ties broken by instance id so the choice is deterministic. Instances
/// without a readable `provider_type` or `priority` never match.
///
/// # Errors
///
/// `provider_not_supported` when no instance matches.
pub fn select_plugin_instance(
    provider_type: &str,
    instances: &[(String, serde_json::Value)],
) -> Result<String, LlmGatewayError> {
    instances
        .iter()
        .filter(|(_, value)| {
            value
                .pointer("/properties/provider_type")
                .and_then(serde_json::Value::as_str)
                == Some(provider_type)
        })
        .filter_map(|(id, value)| {
            let priority = value.get("priority")?.as_i64()?;
            Some((priority, id))
        })
        .min()
        .map(|(_, id)| id.clone())
        .ok_or_else(|| LlmGatewayError::provider_not_supported(provider_type))
}

/// Resolves plugins from types-registry instances and scoped `ClientHub`
/// clients.
///
/// The instance list is configuration, not request state: it is tenant
/// neutral and cached after the first successful non-empty load. An empty
/// list is not cached, because plugins register at startup and may not be
/// there yet.
pub struct GtsPluginDirectory {
    hub: Arc<ClientHub>,
    instances: OnceCell<Vec<PluginInstance>>,
}

impl GtsPluginDirectory {
    /// Build the directory over `hub`.
    #[must_use]
    pub fn new(hub: Arc<ClientHub>) -> Self {
        Self {
            hub,
            instances: OnceCell::new(),
        }
    }

    async fn list_instances(&self) -> Result<Vec<PluginInstance>, LlmGatewayError> {
        let registry = self
            .hub
            .get::<dyn TypesRegistryClient>()
            .map_err(|e| LlmGatewayError::internal_from("types-registry client unavailable", e))?;
        let type_id = LlmGatewayProviderPluginSpecV1::gts_type_id();
        let found = registry
            .list_instances(InstanceQuery::new().with_pattern(format!("{type_id}*")))
            .await
            .map_err(|e| LlmGatewayError::internal_from("types-registry listing failed", e))?;
        Ok(found
            .into_iter()
            .map(|entry| (entry.id.to_string(), entry.object))
            .collect())
    }

    async fn cached_instances(&self) -> Result<Vec<PluginInstance>, LlmGatewayError> {
        if let Some(list) = self.instances.get() {
            return Ok(list.clone());
        }
        let list = self.list_instances().await?;
        // A concurrent loader may win the race; either list is valid.
        if !list.is_empty() && self.instances.set(list.clone()).is_err() {
            tracing::debug!("plugin instance list already cached");
        }
        Ok(list)
    }
}

#[async_trait]
impl PluginDirectory for GtsPluginDirectory {
    async fn resolve(&self, provider_type: &str) -> Result<ResolvedPlugin, LlmGatewayError> {
        let instances = self.cached_instances().await?;
        let instance_id = select_plugin_instance(provider_type, &instances)?;
        let scope = ClientScope::gts_id(instance_id.as_str());
        let client = self
            .hub
            .try_get_scoped::<dyn LlmGatewayProviderPluginClientV1>(&scope)
            .ok_or_else(|| {
                tracing::warn!(instance_id, "provider plugin client is not registered");
                LlmGatewayError::provider_not_supported(provider_type)
            })?;
        let tool_namespace = instances
            .iter()
            .find(|(id, _)| *id == instance_id)
            .and_then(|(_, value)| value.pointer("/properties/tool_namespace"))
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();
        Ok(ResolvedPlugin::new(client, tool_namespace))
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "plugin_directory_tests.rs"]
mod plugin_directory_tests;
