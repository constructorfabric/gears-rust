//! Gear declaration of the storage plugin.

use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use quota_enforcement_sdk::{QuotaEnforcementStoragePluginSpecV1, QuotaEnforcementStoragePluginV1};
use toolkit::Gear;
use toolkit::client_hub::ClientScope;
use toolkit::context::GearCtx;
use toolkit::contracts::DatabaseCapability;
use toolkit::gts::PluginV1;
use tracing::info;
use types_registry_sdk::{RegisterResult, TypesRegistryClient};

use crate::config::StoragePluginConfig;
use crate::domain::StoragePlugin;
use crate::infra::outbox::{NotificationOutbox, QeOutbox, SqlNotificationPipeline};
use crate::infra::storage::{
    SqlConsumptionStore, SqlFoundationStore, SqlPolicyStore, SqlQuotaStore,
};

/// GTS instance segment this backend registers under.
pub const INSTANCE_SEGMENT: &str = "cf.core._.qe_db_storage.v1";

/// Storage plugin gear.
///
/// `init` validates the configuration, binds the plugin to the gear's
/// database — every store over the notification outbox handle, and the one
/// notification pipeline — and publishes it: the plugin instance in
/// types-registry, under which the gear selects its storage, and the scoped
/// `QuotaEnforcementStoragePluginV1` client. The runtime applies the
/// migrations before `init` through [`DatabaseCapability`]. The outbox handle
/// stays unbound until the gear starts notification delivery; until then a
/// mutation that enqueues events rolls back, while bootstrap, which enqueues
/// none, succeeds.
// @cpt-dod:cpt-cf-quota-enforcement-dod-workspace-crates:p1
#[toolkit::gear(
    name = "quota-enforcement-storage-plugin",
    deps = [types_registry],
    capabilities = [db]
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

        let db = ctx.db_required()?;
        let plugin = Arc::new(
            StoragePlugin::new(
                Arc::new(SqlFoundationStore::new(db.db())),
                Arc::new(SqlQuotaStore::new(db.db(), self.notification_enqueuer())),
                Arc::new(SqlPolicyStore::new(db.db(), self.notification_enqueuer())),
                {
                    // Leases settle against the counters and records the
                    // consumption store owns, so one adapter serves both ports.
                    let consumption = Arc::new(SqlConsumptionStore::new(
                        db.db(),
                        self.notification_enqueuer(),
                    ));
                    Arc::clone(&consumption) as Arc<dyn crate::domain::ports::ConsumptionStore>
                },
                {
                    let leases = Arc::new(SqlConsumptionStore::new(
                        db.db(),
                        self.notification_enqueuer(),
                    ));
                    leases as Arc<dyn crate::domain::ports::LeaseStore>
                },
            )
            .with_notifications(Arc::new(SqlNotificationPipeline::new(
                db.db(),
                Arc::clone(&self.outbox),
            ))),
        );
        self.plugin
            .set(Arc::clone(&plugin))
            .map_err(|_| anyhow::anyhow!("{} gear already initialized", Self::MODULE_NAME))?;

        let (instance_id, instance) =
            PluginV1::<QuotaEnforcementStoragePluginSpecV1>::build_registration(
                INSTANCE_SEGMENT,
                cfg.vendor.clone(),
                cfg.priority,
            )?;
        let registry = ctx.client_hub().get::<dyn TypesRegistryClient>()?;
        RegisterResult::ensure_all_ok(&registry.register(vec![instance]).await?)?;
        ctx.client_hub()
            .register_scoped::<dyn QuotaEnforcementStoragePluginV1>(
                ClientScope::gts_id(&instance_id),
                plugin as Arc<dyn QuotaEnforcementStoragePluginV1>,
            );
        info!(
            instance_id = %instance_id,
            vendor = %cfg.vendor,
            priority = cfg.priority,
            "storage plugin registered"
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
