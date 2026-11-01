//! License enforcement gear.

use std::sync::{Arc, OnceLock};

use gts::GtsTypeId;

use async_trait::async_trait;
use authz_resolver_sdk::{AuthZResolverApi, PolicyEnforcer};
use license_enforcement_sdk::{LicenseEnforcementApi, linked_admission_plugin, linked_evaluator};
use license_resolver_sdk::{LicenseResolverPluginClient, LicenseResolverPluginSpecV1};
use sea_orm_migration::MigrationTrait;
use time::Duration;
use tokio_util::sync::CancellationToken;
use toolkit::api::OpenApiRegistry;
use toolkit::client_hub::ClientScope;
use toolkit::contracts::RunnableCapability;
use toolkit::gts::PluginV1;
use toolkit::{DatabaseCapability, Gear, GearCtx, RestApiCapability};
use toolkit_db::DBProvider;
use toolkit_db::odata::LimitCfg;
use tracing::info;
use types_registry_sdk::{RegisterResult, TypesRegistryClient};

use crate::config::LicenseEnforcementConfig;
use crate::domain::check::CheckService;
use crate::domain::error::DomainError;
use crate::domain::grants::GrantService;
use crate::domain::local_client::{LocalClient, ResolverAdapter};
use crate::domain::types::TypeResolver;
use crate::infra::storage::Store;

/// The domain services over the store of this gear.
pub type Grants = GrantService<Store>;

/// GTS instance suffix of this gear as a license-resolver backend.
pub const INSTANCE_SUFFIX: &str = "cf.builtin.license_enforcement.plugin.v1";

/// License Enforcement gear.
///
/// The binary links exactly one admission plugin and one evaluator; init fails
/// otherwise. The gear registers its management contract in `ClientHub` and
/// itself as a license-resolver backend. `start` fails when a type that the
/// evaluator names is not registered: types-registry serves types only after
/// every gear finished `init`.
#[toolkit::gear(
    name = "license-enforcement",
    deps = [types_registry, authz_resolver, tenant_resolver],
    capabilities = [db, rest, stateful]
)]
#[derive(Default)]
pub struct LicenseEnforcement {
    grants: OnceLock<Arc<Grants>>,
    named_types: OnceLock<(TypeResolver, Vec<GtsTypeId>)>,
}

#[async_trait]
impl Gear for LicenseEnforcement {
    async fn init(&self, ctx: &GearCtx) -> anyhow::Result<()> {
        let cfg: LicenseEnforcementConfig = ctx.config_or_default()?;
        cfg.validate()
            .map_err(|e| anyhow::anyhow!("{} config: {e}", Self::MODULE_NAME))?;
        let plugin = linked_admission_plugin()
            .map_err(|e| anyhow::anyhow!("license-enforcement admission plugin: {e}"))?;
        let evaluator = linked_evaluator()
            .map_err(|e| anyhow::anyhow!("license-enforcement evaluator: {e}"))?;
        let hub = ctx.client_hub();
        self.named_types
            .set((TypeResolver::new(Arc::clone(&hub)), evaluator.named_types()))
            .map_err(|_| anyhow::anyhow!("{} gear already initialized", Self::MODULE_NAME))?;

        let db = Arc::new(DBProvider::<DomainError>::new(ctx.db_required()?.db()));
        let store = Store::new(
            LimitCfg {
                default: cfg.default_page_size,
                max: cfg.max_page_size,
            },
            cfg.max_candidates,
        );
        let authz = hub.get::<dyn AuthZResolverApi>()?;
        let check = Arc::new(CheckService::new(
            Arc::clone(&db),
            store,
            Arc::clone(&evaluator),
            Arc::clone(&plugin),
        ));
        let grants = Arc::new(GrantService::new(
            db,
            store,
            PolicyEnforcer::new(authz),
            Arc::clone(&hub),
            evaluator,
            Arc::clone(&plugin),
            check.material(),
            Duration::seconds(cfg.idempotency_window_secs),
        ));
        self.grants
            .set(Arc::clone(&grants))
            .map_err(|_| anyhow::anyhow!("{} gear already initialized", Self::MODULE_NAME))?;

        hub.register::<dyn LicenseEnforcementApi>(Arc::new(LocalClient::new(grants)));

        let (instance_id, instance_json) =
            PluginV1::<LicenseResolverPluginSpecV1>::build_registration(
                INSTANCE_SUFFIX,
                cfg.vendor.clone(),
                cfg.priority,
            )?;
        let registry = hub.get::<dyn TypesRegistryClient>()?;
        RegisterResult::ensure_all_ok(&registry.register(vec![instance_json]).await?)?;
        let adapter: Arc<dyn LicenseResolverPluginClient> = Arc::new(ResolverAdapter::new(check));
        hub.register_scoped::<dyn LicenseResolverPluginClient>(
            ClientScope::gts_id(&instance_id),
            adapter,
        );

        let info = plugin.info();
        info!(
            %instance_id,
            vendor = %cfg.vendor,
            plugin_id = info.id,
            verifies_evidence = info.verifies_evidence,
            "license-enforcement initialized"
        );
        Ok(())
    }
}

#[async_trait]
impl RunnableCapability for LicenseEnforcement {
    async fn start(&self, _cancel: CancellationToken) -> anyhow::Result<()> {
        let (types, named) = self
            .named_types
            .get()
            .ok_or_else(|| anyhow::anyhow!("{} gear not initialized", Self::MODULE_NAME))?;
        types.ensure_registered(named).await?;
        Ok(())
    }

    async fn stop(&self, _deadline: CancellationToken) -> anyhow::Result<()> {
        Ok(())
    }
}

impl DatabaseCapability for LicenseEnforcement {
    fn migrations(&self) -> Vec<Box<dyn MigrationTrait>> {
        use sea_orm_migration::MigratorTrait;
        crate::infra::storage::migrations::Migrator::migrations()
    }
}

impl RestApiCapability for LicenseEnforcement {
    fn register_rest(
        &self,
        ctx: &GearCtx,
        router: axum::Router,
        openapi: &dyn OpenApiRegistry,
    ) -> anyhow::Result<axum::Router> {
        let api = ctx.client_hub().get::<dyn LicenseEnforcementApi>()?;
        let grants = self
            .grants
            .get()
            .ok_or_else(|| anyhow::anyhow!("{} gear not initialized", Self::MODULE_NAME))?;
        let router = license_enforcement_sdk::rest::register_license_enforcement_api_rest_routes(
            router, openapi, api,
        );
        Ok(crate::api::register_list_routes(
            router,
            openapi,
            Arc::clone(grants),
        ))
    }
}
