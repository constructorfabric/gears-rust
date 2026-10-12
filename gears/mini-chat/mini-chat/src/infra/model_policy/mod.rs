use std::sync::Arc;

use async_trait::async_trait;
use mini_chat_sdk::{
    MiniChatModelPolicyPluginClientV1, MiniChatModelPolicyPluginSpecV1, PolicySnapshot,
};
use toolkit::client_hub::{ClientHub, ClientScope};
use toolkit::plugins::{GtsPluginSelector, choose_plugin_instance};
use types_registry_sdk::{InstanceQuery, TypesRegistryClient};
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
}

impl ModelPolicyGateway {
    pub(crate) fn new(hub: Arc<ClientHub>, vendor: String) -> Self {
        Self {
            hub,
            vendor,
            policy_selector: GtsPluginSelector::new(),
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
            .get_current_policy_version(user_id)
            .await
            .map_err(|e| DomainError::internal(e.to_string()))?;
        plugin
            .get_policy_snapshot(user_id, version_info.policy_version)
            .await
            .map_err(|e| DomainError::internal(e.to_string()))
    }

    /// Resolve the policy plugin instance from types-registry.
    async fn resolve_policy_plugin(&self) -> Result<String, anyhow::Error> {
        let registry = self.hub.get::<dyn TypesRegistryClient>()?;
        let plugin_type_id = MiniChatModelPolicyPluginSpecV1::gts_type_id().clone();
        let instances = registry
            .list_instances(InstanceQuery::new().with_pattern(format!("{plugin_type_id}*")))
            .await?;

        let gts_id = choose_plugin_instance::<MiniChatModelPolicyPluginSpecV1>(
            &self.vendor,
            instances.iter().map(|e| (e.id.as_ref(), &e.object)),
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
                    .find(|m| m.id == model && m.enabled);

                match entry {
                    Some(e) => Ok(ResolvedModel::from(e)),
                    None => Err(DomainError::invalid_model(&model)),
                }
            }
        }
    }

    async fn resolve_chat_model(
        &self,
        user_id: Uuid,
        model_id: &str,
    ) -> Result<ResolvedModel, DomainError> {
        let snapshot = self.current_snapshot(user_id).await?;
        snapshot
            .model_catalog
            .iter()
            .find(|m| m.id == model_id)
            .map(ResolvedModel::from)
            .ok_or_else(|| DomainError::invalid_model(model_id))
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
            .find(|m| m.id == model_id && m.enabled)
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
            .get_policy_snapshot(user_id, policy_version)
            .await
            .map_err(|e| DomainError::internal(e.to_string()))
    }

    async fn get_current_version(&self, user_id: Uuid) -> Result<u64, DomainError> {
        let plugin = self.get_policy_plugin().await?;
        let info = plugin
            .get_current_policy_version(user_id)
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
            .get_user_limits(user_id, policy_version)
            .await
            .map_err(|e| DomainError::internal(e.to_string()))
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use mini_chat_sdk::{
        KillSwitches, MiniChatModelPolicyPluginError, ModelTier, PolicyVersionInfo, PublishError,
        UsageEvent,
    };

    use super::*;
    use crate::domain::service::test_helpers::{TestCatalogEntryParams, test_catalog_entry};

    const INSTANCE_ID: &str = "test.model_policy.plugin.v1~test._.mock.v1";

    struct SnapshotPlugin {
        snapshot: PolicySnapshot,
    }

    #[async_trait]
    impl MiniChatModelPolicyPluginClientV1 for SnapshotPlugin {
        async fn get_current_policy_version(
            &self,
            user_id: Uuid,
        ) -> Result<PolicyVersionInfo, MiniChatModelPolicyPluginError> {
            Ok(PolicyVersionInfo {
                user_id,
                policy_version: self.snapshot.policy_version,
                generated_at: time::OffsetDateTime::now_utc(),
            })
        }

        async fn get_policy_snapshot(
            &self,
            _user_id: Uuid,
            _policy_version: u64,
        ) -> Result<PolicySnapshot, MiniChatModelPolicyPluginError> {
            Ok(self.snapshot.clone())
        }

        async fn get_user_limits(
            &self,
            _user_id: Uuid,
            _policy_version: u64,
        ) -> Result<UserLimits, MiniChatModelPolicyPluginError> {
            unimplemented!("not used")
        }

        async fn publish_usage(&self, _payload: UsageEvent) -> Result<(), PublishError> {
            unimplemented!("not used")
        }
    }

    fn entry(model_id: &str, enabled: bool) -> mini_chat_sdk::ModelCatalogEntry {
        test_catalog_entry(TestCatalogEntryParams {
            model_id: model_id.to_owned(),
            provider_model_id: format!("provider-{model_id}"),
            display_name: model_id.to_owned(),
            tier: ModelTier::Standard,
            enabled,
            is_default: false,
            input_tokens_credit_multiplier_micro: 1_000_000,
            output_tokens_credit_multiplier_micro: 1_000_000,
            multimodal_capabilities: vec![],
            context_window: 128_000,
            max_output_tokens: 4096,
            description: String::new(),
            provider_display_name: String::new(),
            multiplier_display: "1x".to_owned(),
            provider_id: "openai".to_owned(),
        })
    }

    async fn gateway(catalog: Vec<mini_chat_sdk::ModelCatalogEntry>) -> ModelPolicyGateway {
        let hub = Arc::new(ClientHub::new());
        hub.register_scoped::<dyn MiniChatModelPolicyPluginClientV1>(
            ClientScope::gts_id(INSTANCE_ID),
            Arc::new(SnapshotPlugin {
                snapshot: PolicySnapshot {
                    user_id: Uuid::nil(),
                    policy_version: 1,
                    model_catalog: catalog,
                    kill_switches: KillSwitches::default(),
                },
            }),
        );
        let policy_selector = GtsPluginSelector::new();
        policy_selector
            .get_or_init(|| async { Ok::<_, anyhow::Error>(INSTANCE_ID.to_owned()) })
            .await
            .expect("pre-warm selector");
        ModelPolicyGateway {
            hub,
            vendor: String::new(),
            policy_selector,
        }
    }

    #[tokio::test]
    async fn resolve_chat_model_returns_globally_disabled_model() {
        let gw = gateway(vec![entry("gpt-5.2", true), entry("gpt-old", false)]).await;

        let resolved = gw
            .resolve_chat_model(Uuid::new_v4(), "gpt-old")
            .await
            .expect("a chat keeps its model after the model is disabled");
        assert_eq!(resolved.model_id, "gpt-old");
    }

    #[tokio::test]
    async fn resolve_chat_model_rejects_id_missing_from_catalog() {
        let gw = gateway(vec![entry("gpt-5.2", true)]).await;

        let err = gw
            .resolve_chat_model(Uuid::new_v4(), "gpt-gone")
            .await
            .expect_err("unknown model id must be rejected");
        assert!(
            matches!(&err, DomainError::InvalidModel { model } if model == "gpt-gone"),
            "expected InvalidModel, got: {err:?}"
        );
    }
}
