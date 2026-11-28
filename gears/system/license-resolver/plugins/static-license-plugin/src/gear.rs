//! Static license resolver plugin gear.

use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use license_resolver_sdk::{LicenseResolverPluginClient, LicenseResolverPluginSpecV1};
use toolkit::Gear;
use toolkit::context::GearCtx;
use toolkit::gts::PluginV1;
use toolkit::gts_publication::GtsPublicationInputs;
use toolkit::plugins::bind_plugin_instances;
use tracing::info;
use types_registry_sdk::GtsId;

use crate::config::StaticLicensePluginConfig;
use crate::domain::Service;

/// GTS instance suffix identifying this backend implementation.
///
/// The full instance id is this appended to
/// [`LicenseResolverPluginSpecV1`]'s type id — that is the id the gateway
/// resolves the scoped client under, and the one a deployment check looks for in
/// types-registry.
pub const INSTANCE_SUFFIX: &str = "cf.builtin.static_license_resolver.plugin.v1";

/// Static license resolver plugin gear.
///
/// license-resolver publishes the plugin spec; the runtime publishes this gear's
/// instance (vendor + priority, from the configuration) before `init`, and
/// `init` binds the scoped client to the admitted instance.
#[toolkit::gear(
    name = "static-license-plugin",
    gts_publication(entities = Self::plugin_instance),
)]
pub struct StaticLicensePlugin {
    service: OnceLock<Arc<Service>>,
}

impl Default for StaticLicensePlugin {
    fn default() -> Self {
        Self {
            service: OnceLock::new(),
        }
    }
}

impl StaticLicensePlugin {
    /// The publication's config-only document: the plugin instance, admitted
    /// by the runtime before `init`.
    fn plugin_instance(
        inputs: &GtsPublicationInputs<'_>,
    ) -> anyhow::Result<Vec<(GtsId, serde_json::Value)>> {
        let cfg: StaticLicensePluginConfig = inputs.config_or_default()?;
        // An invalid rule set is never advertised as a backend.
        Service::from_config(&cfg)?;
        let (id, document) = PluginV1::<LicenseResolverPluginSpecV1>::build_registration(
            INSTANCE_SUFFIX,
            cfg.vendor,
            cfg.priority,
        )?;
        Ok(vec![(GtsId::try_new(id.as_ref())?, document)])
    }
}

#[async_trait]
impl Gear for StaticLicensePlugin {
    async fn init(&self, ctx: &GearCtx) -> anyhow::Result<()> {
        if self.service.get().is_some() {
            anyhow::bail!("{} gear already initialized", Self::MODULE_NAME);
        }

        let cfg: StaticLicensePluginConfig = ctx.config_or_default()?;
        info!(
            vendor = %cfg.vendor,
            priority = cfg.priority,
            grant_count = cfg.grants.len(),
            "Loaded plugin configuration"
        );

        // Built before registering anything: an invalid rule set must abort
        // startup rather than advertise a backend that denies everything.
        let service = Arc::new(Service::from_config(&cfg)?);

        // The instance was admitted before `init`: bind the scoped client to
        // it, as the admitted manifest lists it.
        let manifest = ctx.gts_publication_manifest()?;
        self.service
            .set(service.clone())
            .map_err(|_| anyhow::anyhow!("{} gear already initialized", Self::MODULE_NAME))?;

        let api: Arc<dyn LicenseResolverPluginClient> = service;
        bind_plugin_instances::<LicenseResolverPluginSpecV1, _>(
            &ctx.client_hub(),
            manifest.documents(),
            &api,
        )?;
        Ok(())
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "gear_tests.rs"]
mod gear_tests;
