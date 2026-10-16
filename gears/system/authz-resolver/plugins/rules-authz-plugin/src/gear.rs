//! Rules `AuthZ` resolver plugin gear.

use std::sync::{Arc, OnceLock};

use anyhow::Context;
use async_trait::async_trait;
use authz_resolver_sdk::{AuthZResolverPluginClient, AuthZResolverPluginSpecV1};
use toolkit::Gear;
use toolkit::client_hub::ClientScope;
use toolkit::context::GearCtx;
use toolkit::gts::PluginV1;
use tracing::info;
use types_registry_sdk::{RegisterResult, TypesRegistryClient};

use crate::config::RulesAuthZPluginConfig;
use crate::domain::Service;

/// Rules `AuthZ` resolver plugin gear.
#[toolkit::gear(
    name = "rules-authz-plugin",
    deps = [types_registry]
)]
pub struct RulesAuthZPlugin {
    service: OnceLock<Arc<Service>>,
}

impl Default for RulesAuthZPlugin {
    fn default() -> Self {
        Self {
            service: OnceLock::new(),
        }
    }
}

#[async_trait]
impl Gear for RulesAuthZPlugin {
    async fn init(&self, ctx: &GearCtx) -> anyhow::Result<()> {
        // Explicit configuration is required: there is no default policy.
        let cfg: RulesAuthZPluginConfig = ctx
            .config()
            .context("rules-authz-plugin: explicit policy configuration required")?;
        let service = Arc::new(Service::from_config(&cfg)?);
        info!(
            vendor = %cfg.vendor,
            priority = cfg.priority,
            policy_revision = %cfg.policy_revision,
            rules = cfg.rules.len(),
            unconditional_grants = cfg.unconditional_grants.len(),
            "Loaded rules authz policy"
        );

        let (instance_id, instance_json) =
            PluginV1::<AuthZResolverPluginSpecV1>::build_registration(
                "cf.builtin.rules_authz_resolver.plugin.v1",
                cfg.vendor.clone(),
                cfg.priority,
            )?;

        let registry = ctx.client_hub().get::<dyn TypesRegistryClient>()?;
        let results = registry.register(vec![instance_json]).await?;
        RegisterResult::ensure_all_ok(&results)?;

        self.service
            .set(service.clone())
            .map_err(|_| anyhow::anyhow!("{} gear already initialized", Self::MODULE_NAME))?;

        let api: Arc<dyn AuthZResolverPluginClient> = service;
        ctx.client_hub()
            .register_scoped::<dyn AuthZResolverPluginClient>(
                ClientScope::gts_id(&instance_id),
                api,
            );

        info!(instance_id = %instance_id);
        Ok(())
    }
}
