//! Gear declaration of the storage plugin.

use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use quota_enforcement_sdk::{QuotaEnforcementStoragePluginSpecV1, QuotaEnforcementStoragePluginV1};
use toolkit::Gear;
use toolkit::context::GearCtx;
use toolkit::contracts::DatabaseCapability;
use toolkit::gts::PluginV1;
use toolkit::gts_publication::GtsPublicationInputs;
use toolkit::plugins::bind_plugin_instances;
use tracing::info;
use types_registry_sdk::GtsId;

use crate::config::StoragePluginConfig;
use crate::domain::StoragePlugin;
use crate::infra::outbox::{NotificationOutbox, QeOutbox, SqlNotificationPipeline};
use crate::infra::storage::config_cache::ConfigCache;
use crate::infra::storage::{
    SqlConsumptionStore, SqlFoundationStore, SqlPolicyStore, SqlQuotaStore,
};

/// GTS instance segment this backend registers under.
pub const INSTANCE_SEGMENT: &str = "cf.core._.qe_db_storage.v1";

/// Storage plugin gear.
///
/// The runtime publishes the plugin instance built from the configuration
/// (`gts_publication(entities = ...)`) and calls `init` only once it is
/// admitted. `init` validates the configuration, binds the plugin to the
/// gear's database — every store over the notification outbox handle, and the
/// one notification pipeline — and binds the scoped
/// `QuotaEnforcementStoragePluginV1` client to the admitted instance, under
/// which quota-enforcement selects its storage. The runtime applies the
/// migrations before `init` through [`DatabaseCapability`]. The outbox handle
/// stays unbound until the gear starts notification delivery; until then a
/// mutation that enqueues events rolls back, while bootstrap, which enqueues
/// none, succeeds.
// @cpt-dod:cpt-cf-quota-enforcement-dod-workspace-crates:p1
#[toolkit::gear(
    name = "quota-enforcement-storage-plugin",
    capabilities = [db],
    gts_publication(entities = Self::plugin_instance),
)]
pub struct StoragePluginGear {
    plugin: OnceLock<Arc<StoragePlugin>>,
    outbox: Arc<QeOutbox>,
}

impl Default for StoragePluginGear {
    fn default() -> Self {
        Self {
            plugin: OnceLock::new(),
            outbox: Arc::new(QeOutbox::new()),
        }
    }
}

impl StoragePluginGear {
    /// The publication's config-only document: the plugin instance.
    fn plugin_instance(
        inputs: &GtsPublicationInputs<'_>,
    ) -> anyhow::Result<Vec<(GtsId, serde_json::Value)>> {
        let cfg: StoragePluginConfig = inputs.config_or_default()?;
        cfg.validate()?;
        let (id, document) = PluginV1::<QuotaEnforcementStoragePluginSpecV1>::build_registration(
            INSTANCE_SEGMENT,
            cfg.vendor,
            cfg.priority,
        )?;
        Ok(vec![(GtsId::try_new(id.as_ref())?, document)])
    }

    /// The bound plugin, once `init` ran.
    #[must_use]
    pub fn plugin(&self) -> Option<Arc<StoragePlugin>> {
        self.plugin.get().cloned()
    }

    /// The late-bound notification outbox every mutation enqueues on.
    #[must_use]
    pub fn notification_outbox(&self) -> Arc<QeOutbox> {
        Arc::clone(&self.outbox)
    }

    fn notification_enqueuer(&self) -> Arc<dyn NotificationOutbox> {
        let outbox: Arc<QeOutbox> = Arc::clone(&self.outbox);
        outbox
    }
}

#[async_trait]
impl Gear for StoragePluginGear {
    async fn init(&self, ctx: &GearCtx) -> anyhow::Result<()> {
        if self.plugin.get().is_some() {
            anyhow::bail!("{} gear already initialized", Self::MODULE_NAME);
        }
        let cfg: StoragePluginConfig = ctx.config_or_default()?;
        cfg.validate()?;
        // The instance was admitted before `init`; without that manifest
        // nothing is built, so a failed `init` leaves the gear as it was.
        let manifest = ctx.gts_publication_manifest()?;

        let db = ctx.db_required()?;
        // One configuration cache for every store; one adapter serves both consumption ports.
        let config = Arc::new(ConfigCache::new(cfg.config_refresh()));
        let consumption = Arc::new(
            SqlConsumptionStore::new(db.db(), self.notification_enqueuer())
                .with_config(Arc::clone(&config)),
        );
        let plugin = Arc::new(
            StoragePlugin::new(
                Arc::new(SqlFoundationStore::new(db.db())),
                Arc::new(
                    SqlQuotaStore::new(db.db(), self.notification_enqueuer()).with_config(config),
                ),
                Arc::new(SqlPolicyStore::new(db.db(), self.notification_enqueuer())),
                Arc::clone(&consumption) as Arc<dyn crate::domain::ports::ConsumptionStore>,
                consumption as Arc<dyn crate::domain::ports::LeaseStore>,
            )
            .with_notifications(Arc::new(SqlNotificationPipeline::new(
                db.db(),
                Arc::clone(&self.outbox),
            ))),
        );
        self.plugin
            .set(Arc::clone(&plugin))
            .map_err(|_| anyhow::anyhow!("{} gear already initialized", Self::MODULE_NAME))?;

        // Bind the scoped client to the admitted instance the manifest lists.
        let service: Arc<dyn QuotaEnforcementStoragePluginV1> = plugin;
        bind_plugin_instances::<QuotaEnforcementStoragePluginSpecV1, _>(
            &ctx.client_hub(),
            manifest.documents(),
            &service,
        )?;
        info!(
            vendor = %cfg.vendor,
            priority = cfg.priority,
            "storage plugin bound"
        );
        Ok(())
    }
}

impl DatabaseCapability for StoragePluginGear {
    fn migrations(&self) -> Vec<Box<dyn sea_orm_migration::MigrationTrait>> {
        use sea_orm_migration::MigratorTrait;
        crate::infra::storage::Migrator::migrations()
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "gear_tests.rs"]
mod gear_tests;
