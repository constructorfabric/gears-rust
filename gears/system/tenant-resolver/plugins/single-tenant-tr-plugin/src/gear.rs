//! Single-tenant resolver plugin gear.

use std::sync::Arc;

use async_trait::async_trait;
use tenant_resolver_sdk::{TenantResolverPluginClient, TenantResolverPluginSpecV1};
use toolkit::Gear;
use toolkit::context::GearCtx;
use toolkit::gts::PluginV1;
use toolkit::gts_publication::GtsPublicationInputs;
use toolkit::plugins::bind_plugin_instances;
use tracing::info;

use crate::config::SingleTenantTrPluginConfig;
use crate::domain::Service;

/// The GTS segment of this plugin's instance.
const INSTANCE_SEGMENT: &str = "cf.builtin.single_tenant_resolver.plugin.v1";

/// Single-tenant resolver plugin gear.
///
/// Zero-configuration plugin for single-tenant deployments.
/// Returns the tenant from security context as the only accessible tenant.
///
/// The runtime publishes the instance built from the configuration
/// (`entities`) — locally or to a remote registry — and calls `init` only once
/// it is admitted; `init` binds the scoped client to the admitted instance.
#[toolkit::gear(
    name = "single-tenant-tr-plugin",
    gts_publication(entities = Self::plugin_instance),
)]
pub struct SingleTenantTrPlugin;

impl Default for SingleTenantTrPlugin {
    fn default() -> Self {
        Self
    }
}

impl SingleTenantTrPlugin {
    /// The publication's config-only documents: the plugin instance.
    fn plugin_instance(
        inputs: &GtsPublicationInputs<'_>,
    ) -> anyhow::Result<Vec<(String, serde_json::Value)>> {
        let cfg: SingleTenantTrPluginConfig = inputs.config_or_default()?;
        let (id, document) = PluginV1::<TenantResolverPluginSpecV1>::build_registration(
            INSTANCE_SEGMENT,
            cfg.vendor,
            cfg.priority,
        )?;
        Ok(vec![(id.as_ref().to_owned(), document)])
    }
}

#[async_trait]
impl Gear for SingleTenantTrPlugin {
    async fn init(&self, ctx: &GearCtx) -> anyhow::Result<()> {
        let cfg: SingleTenantTrPluginConfig = ctx.config_or_default()?;
        info!(
            vendor = %cfg.vendor,
            priority = cfg.priority,
            "Loaded single-tenant resolver plugin configuration"
        );

        // The instance was admitted before `init`: bind the scoped client to
        // it, as the admitted manifest lists it.
        let api: Arc<dyn TenantResolverPluginClient> = Arc::new(Service);
        bind_plugin_instances::<TenantResolverPluginSpecV1, _>(
            &ctx.client_hub(),
            ctx.gts_publication_manifest()?.documents(),
            &api,
        )
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "gear_tests.rs"]
mod gear_tests;
