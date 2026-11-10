// Created: 2026-09-23 by Constructor Tech
// Updated: 2026-10-06 by Constructor Tech
//! `ToolKit` gear registration for the Vault / `OpenBao` credential backend.
//!
//! The runtime publishes the GTS plugin instance built from the configuration
//! before `init`; `init` loads and validates configuration and binds a scoped
//! `CredStorePluginClientV2` to the admitted instance through `ClientHub`.

use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use credstore_sdk::{CredStorePluginClientV2, CredStorePluginSpecV2};
use toolkit::Gear;
use toolkit::context::GearCtx;
use toolkit::gts::PluginV1;
use toolkit::gts_publication::GtsPublicationInputs;
use toolkit::plugins::bind_plugin_instances;
use tracing::info;
use types_registry_sdk::GtsId;

use crate::config::VaultCredStorePluginConfig;
use crate::domain::Service;
use crate::factory::build_service;

/// The GTS segment of this plugin's instance.
const INSTANCE_SEGMENT: &str = "cf.core._.vault_credstore.v1";

/// Vault / `OpenBao` credstore plugin gear.
///
/// Stores secret bytes in a Vault / `OpenBao` KV v2 secrets engine over HTTP,
/// authenticating with a Vault token. See the crate README and `docs/` for
/// the mount requirements, the ACL policy and the configuration reference.
#[toolkit::gear(
    name = "vault-credstore-plugin-v2",
    gts_publication(entities = Self::plugin_instance),
)]
pub struct VaultCredStorePlugin {
    service: OnceLock<Arc<Service>>,
}

impl Default for VaultCredStorePlugin {
    fn default() -> Self {
        Self {
            service: OnceLock::new(),
        }
    }
}

impl VaultCredStorePlugin {
    /// The publication's config-only documents: the plugin instance.
    fn plugin_instance(
        inputs: &GtsPublicationInputs<'_>,
    ) -> anyhow::Result<Vec<(GtsId, serde_json::Value)>> {
        let cfg: VaultCredStorePluginConfig = inputs.config_expanded_or_default()?;
        let (id, document) = PluginV1::<CredStorePluginSpecV2>::build_registration(
            INSTANCE_SEGMENT,
            cfg.vendor,
            cfg.priority,
        )?;
        Ok(vec![(GtsId::try_new(id.as_ref())?, document)])
    }
}

#[async_trait]
impl Gear for VaultCredStorePlugin {
    async fn init(&self, ctx: &GearCtx) -> anyhow::Result<()> {
        // Load configuration (`${VAR}` placeholders in `token` and
        // `token_file` are expanded from the process environment here).
        let cfg: VaultCredStorePluginConfig = ctx.config_expanded_or_default()?;

        // Validate the config, then build the HTTP transport and the service
        // before binding (the same construction `client_from_config`
        // offers outside the gear). Neither talks to Vault: the plugin makes
        // no startup check of the server, the mount or the token.
        let (service, token_source) = build_service(&cfg)?;

        info!(
            vendor = %cfg.vendor,
            priority = cfg.priority,
            address = %cfg.address,
            mount = %cfg.mount,
            path_prefix = %cfg.path_prefix,
            token_source,
            retry_max_attempts = cfg.retry.max_attempts,
            "Loaded plugin configuration"
        );

        // All fallible steps done: commit service to shared state.
        self.service
            .set(Arc::clone(&service))
            .map_err(|_| anyhow::anyhow!("{} gear already initialized", Self::MODULE_NAME))?;

        // The instance was admitted before `init`: bind the scoped client to
        // it, as the admitted manifest lists it.
        let api: Arc<dyn CredStorePluginClientV2> = service;
        bind_plugin_instances::<CredStorePluginSpecV2, _>(
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
