//! E2E policy `AuthZ` resolver plugin gear.
//!
//! Registers one `PluginV1<AuthZResolverPluginSpecV1>` instance with the types registry and a
//! scoped [`AuthZResolverPluginClient`] answering from the compiled policy. Test hosts only;
//! never deploy.

use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use authz_resolver_sdk::{AuthZResolverPluginClient, AuthZResolverPluginSpecV1};
use toolkit::Gear;
use toolkit::client_hub::ClientScope;
use toolkit::context::GearCtx;
use toolkit::gts::PluginV1;
use tracing::info;
use types_registry_sdk::{RegisterResult, TypesRegistryClient};

use crate::config::OrdersTestAuthZPluginConfig;
use crate::domain::{Policy, Service};

/// E2E policy `AuthZ` resolver plugin gear (test hosts only; never deploy).
#[toolkit::gear(
    name = "bss-orders-test-authz-plugin",
    deps = [types_registry]
)]
pub struct OrdersTestAuthZPlugin {
    service: OnceLock<Arc<Service>>,
}

impl Default for OrdersTestAuthZPlugin {
    fn default() -> Self {
        Self {
            service: OnceLock::new(),
        }
    }
}

#[async_trait]
impl Gear for OrdersTestAuthZPlugin {
    async fn init(&self, ctx: &GearCtx) -> anyhow::Result<()> {
        let cfg: OrdersTestAuthZPluginConfig = ctx.config_or_default()?;
        // A linked feature without a policy would register a deny-all PDP that outranks the
        // static plugin (priority 10 < 100): refuse it instead.
        if cfg.rules.is_empty() && cfg.unconditional_grants.is_empty() {
            anyhow::bail!(
                "{} gear: the `bss-orders-test-authz-plugin` config block declares no `rules` and no \
                 `unconditional_grants`; a linked e2e policy plugin needs a policy",
                Self::MODULE_NAME
            );
        }
        info!(
            vendor = %cfg.vendor,
            priority = cfg.priority,
            "Loaded plugin configuration"
        );

        // Validate and compile the declared policy before anything is registered, so a bad
        // suite config fails the gear init.
        let policy = Policy::compile(&cfg)?;
        let (rules, grants) = (policy.rule_count(), policy.grant_count());

        // Build registration payload and instance id for this plugin.
        let (instance_id, instance_json) =
            PluginV1::<AuthZResolverPluginSpecV1>::build_registration(
                "cf.builtin.bss_orders_test_authz_resolver.plugin.v1",
                cfg.vendor.clone(),
                cfg.priority,
            )?;

        // Publish to types-registry.
        let registry = ctx.client_hub().get::<dyn TypesRegistryClient>()?;
        let results = registry.register(vec![instance_json]).await?;
        RegisterResult::ensure_all_ok(&results)?;

        // Create service
        let service = Arc::new(Service::new(policy));
        self.service
            .set(Arc::clone(&service))
            .map_err(|_| anyhow::anyhow!("{} gear already initialized", Self::MODULE_NAME))?;

        // Register scoped client in ClientHub
        let api: Arc<dyn AuthZResolverPluginClient> = service;
        ctx.client_hub()
            .register_scoped::<dyn AuthZResolverPluginClient>(
                ClientScope::gts_id(&instance_id),
                api,
            );

        info!(
            instance_id = %instance_id,
            policy_revision = %cfg.policy_revision,
            rules,
            grants,
            "e2e policy AuthZ plugin registered (test hosts only; never deploy)"
        );
        Ok(())
    }
}
