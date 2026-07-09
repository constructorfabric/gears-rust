//! Glue wiring the echo Service to `IdpPluginClient` and publishing the GTS instance — see crate root (lib.rs) for behaviour and prod-safety warnings.

use std::sync::{Arc, OnceLock};

use account_management_sdk::{IdpPluginClient, IdpPluginSpecV1};
use async_trait::async_trait;
use toolkit::Gear;
use toolkit::client_hub::ClientScope;
use toolkit::context::GearCtx;
use toolkit::gts::PluginV1;
use tracing::{info, warn};
use types_registry_sdk::{RegisterResult, TypesRegistryClient};

use crate::config::StaticIdpPluginConfig;
use crate::domain::Service;

/// Static `IdP` plugin gear.
///
/// Registers the permissive echo [`Service`] as a scoped
/// `IdpPluginClient` candidate so Account Management's bootstrap saga
/// and tenant lifecycle flows succeed without a real `IdP` deployment.
///
/// Selection flow (symmetric with Tenant Resolver / `AuthN` Resolver):
///
///   1. Plugin init publishes a `PluginV1<IdpPluginSpecV1>` instance
///      to types-registry carrying the configured `vendor` + `priority`.
///   2. Plugin init registers the trait object under
///      `ClientHub::register_scoped::<dyn IdpPluginClient>(scope = gts_id)`
///      so coexisting `IdP` plugins cannot silently overwrite each other.
///   3. AM resolves at gear init: enumerate every
///      `PluginV1<IdpPluginSpecV1>` instance, `choose_plugin_instance`
///      by `cfg.idp.vendor` (default `"cf"` — matches this plugin's
///      default vendor) + priority tiebreak, then `get_scoped` keyed
///      on the chosen `gts_id`.
#[toolkit::gear(
    name = "static-idp-plugin",
    deps = ["types-registry"]
)]
pub struct StaticIdpPlugin {
    service: OnceLock<Arc<Service>>,
}

impl Default for StaticIdpPlugin {
    fn default() -> Self {
        Self {
            service: OnceLock::new(),
        }
    }
}

#[async_trait]
impl Gear for StaticIdpPlugin {
    async fn init(&self, ctx: &GearCtx) -> anyhow::Result<()> {
        warn!(
            "Static IdP plugin is running in echo mode - every provision/deprovision \
             succeeds without contacting a real IdP. Do NOT use this plugin in production."
        );

        let cfg: StaticIdpPluginConfig = ctx.config_or_default()?;
        info!(
            vendor = %cfg.vendor,
            priority = cfg.priority,
            "Loaded plugin configuration"
        );

        let service = Arc::new(Service::new());

        // Build registration payload and instance id for this plugin.
        let (instance_id, instance_json) = PluginV1::<IdpPluginSpecV1>::build_registration(
            "cf.builtin.static_idp.plugin.v1",
            cfg.vendor.clone(),
            cfg.priority,
        )?;

        // Publish to types-registry for catalogue visibility.
        let registry = ctx.client_hub().get::<dyn TypesRegistryClient>()?;
        let results = registry.register(vec![instance_json.clone()]).await?;
        // Idempotent restart: treat `AlreadyExists` as success only when
        // the stored spec matches our current serialized instance; fail
        // otherwise so a stale registration under the same ID surfaces
        // immediately rather than masking a config drift. Mirrors the
        // pattern in `account_management::gear::init` for the AM-
        // owned TR plugin.
        for result in &results {
            if let RegisterResult::Err { error, .. } = result {
                if matches!(
                    error,
                    toolkit_canonical_errors::CanonicalError::AlreadyExists { .. }
                ) {
                    let existing =
                        registry
                            .get_instance(instance_id.as_ref())
                            .await
                            .map_err(|e| {
                                anyhow::anyhow!("static-idp-plugin: verify existing instance: {e}")
                            })?;
                    if existing.object != instance_json {
                        return Err(anyhow::anyhow!(
                            "static-idp-plugin: instance `{instance_id}` already registered \
                             with a different spec",
                        ));
                    }
                } else {
                    return Err(anyhow::anyhow!(
                        "static-idp-plugin: registration failed: {error}"
                    ));
                }
            }
        }

        self.service
            .set(service.clone())
            .map_err(|_| anyhow::anyhow!("{} gear already initialized", Self::MODULE_NAME))?;

        // AM's lazy IdP resolver (`account_management::infra::idp::
        // LazyIdpProvider`) reads this scoped registration on first
        // API call via `ClientHub::try_get_scoped` keyed on the
        // catalogue instance id. The scope MUST equal `instance_id`
        // (the same value `PluginV1::build_registration` derived
        // above) so the lazy `choose_plugin_instance` → `get_scoped`
        // chain finds this trait object.
        let api: Arc<dyn IdpPluginClient> = service;
        ctx.client_hub()
            .register_scoped::<dyn IdpPluginClient>(ClientScope::gts_id(&instance_id), api);

        info!(instance_id = %instance_id);
        Ok(())
    }
}
