use std::sync::{Arc, OnceLock};

use anyhow::Context as _;
use async_trait::async_trait;
use axum::Router;
use toolkit::api::OpenApiRegistry;
use toolkit::{Gear, GearCtx};
use toolkit_db::DBProvider;
use toolkit_db::DbError;
use tracing::info;

use authz_resolver_sdk::{AuthZResolverApi, PolicyEnforcer};

use construct_sdk::ConstructClientV1;

use crate::api::rest::routes;
use crate::api::rest::types::ConcreteIntake;
use crate::config::ConstructConfig;
use crate::domain::local_client::LocalClient;
use crate::domain::record_intake::{IntakePorts, RecordIntakeService};
use crate::domain::subject_settings::SubjectSettingsService;
use crate::infra::connector_switch::ConfigConnectorSwitch;
use crate::infra::planner_stub::NoProcessingHandOff;
use crate::infra::record_types::RegistryRecordTypes;
use crate::infra::storage::record_ids_repo::SeaOrmRecordIdRepository;
use crate::infra::storage::subject_settings_repo::SeaOrmSubjectSettingsRepository;
use crate::infra::tenant_defaults::SettingsServiceDefaults;

/// The Construct gear: record intake for connectors, with tenant-scoped
/// storage and an in-process client.
///
/// @cpt-dod:cpt-cf-construct-dod-gear-foundation-anatomy:p1
#[toolkit::gear(
    name = "construct",
    deps = [authz_resolver, types_registry],
    capabilities = [rest, db]
)]
pub struct ConstructGear {
    intake: OnceLock<Arc<ConcreteIntake>>,
}

impl Default for ConstructGear {
    fn default() -> Self {
        Self {
            intake: OnceLock::new(),
        }
    }
}

impl toolkit::contracts::DatabaseCapability for ConstructGear {
    fn migrations(&self) -> Vec<Box<dyn sea_orm_migration::MigrationTrait>> {
        use sea_orm_migration::MigratorTrait;
        // @cpt-begin:cpt-cf-construct-algo-gear-foundation-start-gear:p1:inst-start-migrations
        info!("Providing construct database migrations");
        crate::infra::storage::migrations::Migrator::migrations()
        // @cpt-end:cpt-cf-construct-algo-gear-foundation-start-gear:p1:inst-start-migrations
    }
}

#[async_trait]
impl Gear for ConstructGear {
    async fn init(&self, ctx: &GearCtx) -> anyhow::Result<()> {
        // @cpt-begin:cpt-cf-construct-algo-gear-foundation-start-gear:p1:inst-start-config
        // @cpt-begin:cpt-cf-construct-algo-gear-foundation-start-gear:p1:inst-start-config-fail
        let cfg: ConstructConfig = ctx.config_or_default()?;
        // @cpt-end:cpt-cf-construct-algo-gear-foundation-start-gear:p1:inst-start-config-fail
        // @cpt-end:cpt-cf-construct-algo-gear-foundation-start-gear:p1:inst-start-config

        // @cpt-begin:cpt-cf-construct-algo-gear-foundation-start-gear:p1:inst-start-db
        let db: Arc<DBProvider<DbError>> = Arc::new(ctx.db_required()?);
        // @cpt-end:cpt-cf-construct-algo-gear-foundation-start-gear:p1:inst-start-db

        // @cpt-begin:cpt-cf-construct-algo-gear-foundation-start-gear:p1:inst-start-authz
        let authz = ctx
            .client_hub()
            .get::<dyn AuthZResolverApi>()
            .context("failed to get AuthZ resolver")?;
        let policy_enforcer = PolicyEnforcer::new(authz);
        // @cpt-end:cpt-cf-construct-algo-gear-foundation-start-gear:p1:inst-start-authz

        // @cpt-begin:cpt-cf-construct-algo-gear-foundation-start-gear:p1:inst-start-service
        // The settings service and the types registry are resolved from
        // ClientHub on each read, so their start order does not matter.
        let hub = ctx.client_hub();
        let defaults = SettingsServiceDefaults::new(Arc::clone(&hub), cfg.personalization_default)
            .context("personalization default setting key")?;
        let settings = Arc::new(SubjectSettingsService::new(
            Arc::clone(&db),
            Arc::new(SeaOrmSubjectSettingsRepository::new()),
            Arc::new(defaults),
            policy_enforcer.clone(),
        ));
        let ports = IntakePorts {
            types: Arc::new(RegistryRecordTypes::new(hub)?),
            switch: Arc::new(ConfigConnectorSwitch::new(cfg.connectors_off)),
            hand_off: Arc::new(NoProcessingHandOff),
        };
        let intake = Arc::new(RecordIntakeService::new(
            db,
            Arc::new(SeaOrmRecordIdRepository::new()),
            settings,
            ports,
            policy_enforcer,
        ));
        self.intake
            .set(Arc::clone(&intake))
            .map_err(|_| anyhow::anyhow!("{} gear already initialized", Self::MODULE_NAME))?;
        // @cpt-end:cpt-cf-construct-algo-gear-foundation-start-gear:p1:inst-start-service

        // @cpt-begin:cpt-cf-construct-algo-gear-foundation-start-gear:p1:inst-start-client
        let client: Arc<dyn ConstructClientV1> = Arc::new(LocalClient::new(intake));
        ctx.client_hub().register(client);
        // @cpt-end:cpt-cf-construct-algo-gear-foundation-start-gear:p1:inst-start-client

        // @cpt-begin:cpt-cf-construct-algo-gear-foundation-start-gear:p1:inst-start-return
        Ok(())
        // @cpt-end:cpt-cf-construct-algo-gear-foundation-start-gear:p1:inst-start-return
    }
}

#[async_trait]
impl toolkit::contracts::RestApiCapability for ConstructGear {
    fn register_rest(
        &self,
        _ctx: &GearCtx,
        router: Router,
        openapi: &dyn OpenApiRegistry,
    ) -> anyhow::Result<Router> {
        // @cpt-begin:cpt-cf-construct-algo-gear-foundation-start-gear:p1:inst-start-rest
        let intake = self
            .intake
            .get()
            .map(Arc::clone)
            .ok_or_else(|| anyhow::anyhow!("Service not initialized"))?;

        let router = routes::register_routes(router, openapi, intake);
        info!("Construct gear: REST routes registered");
        Ok(router)
        // @cpt-end:cpt-cf-construct-algo-gear-foundation-start-gear:p1:inst-start-rest
    }
}
