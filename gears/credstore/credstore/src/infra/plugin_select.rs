use std::sync::Arc;

use async_trait::async_trait;
use credstore_sdk::{CredStorePluginClientV1, CredStorePluginSpecV1};
use toolkit::client_hub::{ClientHub, ClientScope};
use toolkit::plugins::{GtsPluginSelector, choose_plugin_instance};
use types_registry_sdk::{InstanceQuery, TypesRegistryClient};

use crate::domain::error::DomainError;
use crate::domain::ports::plugin::PluginSelector;

/// Resolves the active `CredStore` backend plugin via the GTS types-registry.
pub struct GtsCredStorePluginSelector {
    hub: Arc<ClientHub>,
    vendor: String,
    selector: GtsPluginSelector,
}

impl GtsCredStorePluginSelector {
    /// Creates a new selector with lazy plugin resolution.
    #[must_use]
    pub fn new(hub: Arc<ClientHub>, vendor: String) -> Self {
        Self {
            hub,
            vendor,
            selector: GtsPluginSelector::new(),
        }
    }

    async fn resolve_instance(&self) -> Result<String, DomainError> {
        // Wire-visible details stay curated (`with_detail` contract); raw
        // dependency errors live only in the cause chain / logs.
        let registry = self.hub.get::<dyn TypesRegistryClient>().map_err(|e| {
            tracing::warn!(err = %e, "types-registry client unavailable");
            DomainError::ServiceUnavailable {
                detail: "types registry unavailable".to_owned(),
                retry_after: None,
                cause: Some(Box::new(e)),
            }
        })?;

        let type_id = CredStorePluginSpecV1::gts_type_id();
        let instances = registry
            .list_instances(InstanceQuery::new().with_pattern(format!("{type_id}*")))
            .await
            .map_err(|e| {
                tracing::warn!(err = %e, "types-registry list_instances failed");
                DomainError::ServiceUnavailable {
                    detail: "types registry unavailable".to_owned(),
                    retry_after: None,
                    cause: Some(Box::new(e)),
                }
            })?;

        let gts_id = choose_plugin_instance::<CredStorePluginSpecV1>(
            &self.vendor,
            instances.iter().map(|e| (e.id.as_ref(), &e.object)),
        )
        .map_err(|e| match e {
            toolkit::plugins::ChoosePluginError::PluginNotFound { vendor, .. } => {
                DomainError::ServiceUnavailable {
                    detail: format!("no credstore plugin found for vendor '{vendor}'"),
                    retry_after: None,
                    cause: None,
                }
            }
            toolkit::plugins::ChoosePluginError::InvalidPluginInstance { gts_id, reason } => {
                DomainError::Internal {
                    diagnostic: format!("invalid credstore plugin instance '{gts_id}': {reason}"),
                    cause: None,
                }
            }
        })?;

        Ok(gts_id)
    }
}

#[async_trait]
impl PluginSelector for GtsCredStorePluginSelector {
    async fn resolve(&self) -> Result<Arc<dyn CredStorePluginClientV1>, DomainError> {
        let instance_id = self
            .selector
            .get_or_init(|| self.resolve_instance())
            .await?;

        let scope = ClientScope::gts_id(instance_id.as_ref());

        self.hub
            .try_get_scoped::<dyn CredStorePluginClientV1>(&scope)
            .ok_or_else(|| DomainError::ServiceUnavailable {
                detail: format!("credstore plugin client not registered yet for '{instance_id}'"),
                retry_after: None,
                cause: None,
            })
    }
}
