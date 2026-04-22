use async_trait::async_trait;
use modkit::Module;
use modkit::context::ModuleCtx;
use tracing::info;
use types_registry_sdk::{RegisterResult, RegisterSummary, TypesRegistryClient};

use crate::config::StaticTypesRegistryPluginConfig;

/// Static types-registry plugin module.
///
/// Seeds pre-configured GTS entities into the Types Registry from YAML configuration.
#[modkit::module(
    name = "static-types-registry-plugin",
    deps = ["types-registry"]
)]
pub struct StaticTypesRegistryPlugin;

impl Default for StaticTypesRegistryPlugin {
    fn default() -> Self {
        Self
    }
}

#[async_trait]
impl Module for StaticTypesRegistryPlugin {
    async fn init(&self, ctx: &ModuleCtx) -> anyhow::Result<()> {
        let cfg: StaticTypesRegistryPluginConfig = ctx.config_or_default()?;

        if cfg.entities.is_empty() {
            info!("No static entities configured — nothing to register");
            return Ok(());
        }

        let registry = ctx.client_hub().get::<dyn TypesRegistryClient>()?;

        let tenant_id_str = cfg.default_tenant_id.to_string();
        let entities: Vec<serde_json::Value> = cfg
            .entities
            .into_iter()
            .map(|mut v| {
                if let Some(obj) = v.as_object_mut() {
                    obj.entry("tenant_id")
                        .or_insert_with(|| serde_json::Value::String(tenant_id_str.clone()));
                }
                v
            })
            .collect();

        let entity_count = entities.len();
        let results = registry.register(entities).await?;
        let summary = RegisterSummary::from_results(&results);

        if !summary.all_succeeded() {
            for result in &results {
                if let RegisterResult::Err { gts_id, error } = result {
                    tracing::error!(
                        gts_id = gts_id.as_deref().unwrap_or("<unknown>"),
                        error = %error,
                        "Failed to register static GTS entity"
                    );
                }
            }
            anyhow::bail!(
                "Static types-registry plugin: {}/{} entities failed to register",
                summary.failed,
                summary.total()
            );
        }

        info!(
            count = entity_count,
            "Registered static GTS entities in types-registry"
        );

        Ok(())
    }
}
