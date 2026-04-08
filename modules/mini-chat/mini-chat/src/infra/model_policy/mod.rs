use std::sync::Arc;

use async_trait::async_trait;
use mini_chat_sdk::{
    MiniChatModelPolicyPluginClientV1, MiniChatModelPolicyPluginSpecV1, PolicySnapshot,
};
use modkit::client_hub::{ClientHub, ClientScope};
use modkit::plugins::{GtsPluginSelector, choose_plugin_instance};
use tokio_util::sync::CancellationToken;
use types_registry_sdk::{ListQuery, TypesRegistryClient};
use uuid::Uuid;

use mini_chat_sdk::UserLimits;

use crate::domain::error::DomainError;
use crate::domain::models::ResolvedModel;
use crate::domain::repos::{ModelResolver, PolicySnapshotProvider, UserLimitsProvider};

/// Resolves model IDs by querying the policy plugin discovered via GTS.
pub struct ModelPolicyGateway {
    hub: Arc<ClientHub>,
    vendor: String,
    policy_selector: GtsPluginSelector,
    cancel: CancellationToken,
}

impl ModelPolicyGateway {
    pub(crate) fn new(hub: Arc<ClientHub>, vendor: String, cancel: CancellationToken) -> Self {
        Self {
            hub,
            vendor,
            policy_selector: GtsPluginSelector::new(),
            cancel,
        }
    }

    /// Lazily resolve the policy plugin from `ClientHub`.
    pub(crate) async fn get_policy_plugin(
        &self,
    ) -> Result<Arc<dyn MiniChatModelPolicyPluginClientV1>, DomainError> {
        let instance_id = self
            .policy_selector
            .get_or_init(|| self.resolve_policy_plugin())
            .await
            .map_err(|e| DomainError::internal(e.to_string()))?;

        let scope = ClientScope::gts_id(instance_id.as_ref());
        self.hub
            .try_get_scoped::<dyn MiniChatModelPolicyPluginClientV1>(&scope)
            .ok_or_else(|| {
                DomainError::internal(format!(
                    "Policy plugin client not registered: {instance_id}"
                ))
            })
    }

    /// Fetch the current policy snapshot for a user.
    async fn current_snapshot(&self, user_id: Uuid) -> Result<PolicySnapshot, DomainError> {
        let plugin = self.get_policy_plugin().await?;
        let version_info = plugin
            .get_current_policy_version(user_id, self.cancel.clone())
            .await
            .map_err(|e| DomainError::internal(e.to_string()))?;
        plugin
            .get_policy_snapshot(user_id, version_info.policy_version, self.cancel.clone())
            .await
            .map_err(|e| DomainError::internal(e.to_string()))
    }

    /// Resolve the policy plugin instance from types-registry.
    async fn resolve_policy_plugin(&self) -> Result<String, anyhow::Error> {
        let registry = self.hub.get::<dyn TypesRegistryClient>()?;
        let plugin_type_id = MiniChatModelPolicyPluginSpecV1::gts_schema_id().clone();
        let instances = registry
            .list(
                ListQuery::new()
                    .with_pattern(format!("{plugin_type_id}*"))
                    .with_is_type(false),
            )
            .await?;

        let gts_id = choose_plugin_instance::<MiniChatModelPolicyPluginSpecV1>(
            &self.vendor,
            instances.iter().map(|e| (e.gts_id.as_str(), &e.content)),
        )?;

        Ok(gts_id)
    }
}

#[async_trait]
impl ModelResolver for ModelPolicyGateway {
    async fn resolve_model(
        &self,
        user_id: Uuid,
        model: Option<String>,
    ) -> Result<ResolvedModel, DomainError> {
        let snapshot = self.current_snapshot(user_id).await?;

        match model {
            None => {
                // Find default model (prefer is_default + enabled, else first enabled)
                let default = snapshot
                    .model_catalog
                    .iter()
                    .find(|m| m.preference.as_ref().is_some_and(|p| p.is_default) && m.enabled)
                    .or_else(|| snapshot.model_catalog.iter().find(|m| m.enabled));

                match default {
                    Some(entry) => Ok(ResolvedModel::from(entry)),
                    None => Err(DomainError::invalid_model("no models available in catalog")),
                }
            }
            Some(model) if model.is_empty() => {
                Err(DomainError::invalid_model("model must not be empty"))
            }
            Some(model) => {
                let entry = snapshot
                    .model_catalog
                    .iter()
                    .find(|m| m.model_id == model && m.enabled);

                match entry {
                    Some(e) => Ok(ResolvedModel::from(e)),
                    None => Err(DomainError::invalid_model(&model)),
                }
            }
        }
    }

    async fn list_visible_models(&self, user_id: Uuid) -> Result<Vec<ResolvedModel>, DomainError> {
        let snapshot = self.current_snapshot(user_id).await?;

        Ok(snapshot
            .model_catalog
            .iter()
            .filter(|m| m.enabled)
            .map(ResolvedModel::from)
            .collect())
    }

    async fn get_visible_model(
        &self,
        user_id: Uuid,
        model_id: &str,
    ) -> Result<ResolvedModel, DomainError> {
        let snapshot = self.current_snapshot(user_id).await?;

        snapshot
            .model_catalog
            .iter()
            .find(|m| m.model_id == model_id && m.enabled)
            .map(ResolvedModel::from)
            .ok_or_else(|| DomainError::model_not_found(model_id))
    }

    async fn get_kill_switches(
        &self,
        user_id: Uuid,
    ) -> Result<mini_chat_sdk::KillSwitches, DomainError> {
        let snapshot = self.current_snapshot(user_id).await?;
        Ok(snapshot.kill_switches)
    }
}

#[async_trait]
impl PolicySnapshotProvider for ModelPolicyGateway {
    async fn get_snapshot(
        &self,
        user_id: Uuid,
        policy_version: u64,
    ) -> Result<PolicySnapshot, DomainError> {
        let plugin = self.get_policy_plugin().await?;
        plugin
            .get_policy_snapshot(user_id, policy_version, self.cancel.clone())
            .await
            .map_err(|e| DomainError::internal(e.to_string()))
    }

    async fn get_current_version(&self, user_id: Uuid) -> Result<u64, DomainError> {
        let plugin = self.get_policy_plugin().await?;
        let info = plugin
            .get_current_policy_version(user_id, self.cancel.clone())
            .await
            .map_err(|e| DomainError::internal(e.to_string()))?;
        Ok(info.policy_version)
    }
}

#[async_trait]
impl UserLimitsProvider for ModelPolicyGateway {
    async fn get_limits(
        &self,
        user_id: Uuid,
        policy_version: u64,
    ) -> Result<UserLimits, DomainError> {
        let plugin = self.get_policy_plugin().await?;
        plugin
            .get_user_limits(user_id, policy_version, self.cancel.clone())
            .await
            .map_err(|e| DomainError::internal(e.to_string()))
    }
}
