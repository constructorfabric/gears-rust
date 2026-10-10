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
use crate::config::ConstructConfig;
use crate::domain::local_client::LocalClient;
use crate::domain::service::{Service, ServiceConfig};
use crate::infra::storage::sea_orm_repo::SeaOrmNoteRepository;

/// Type alias for the concrete service type with ORM repository.
type ConcreteService = Service<SeaOrmNoteRepository>;

/// The Construct gear: a tenant-scoped shell with one route and an in-process client.
///
/// @cpt-dod:cpt-cf-construct-dod-gear-foundation-anatomy:p1
#[toolkit::gear(
    name = "construct",
    deps = [authz_resolver],
    capabilities = [rest, db]
)]
pub struct ConstructGear {
    service: OnceLock<Arc<ConcreteService>>,
}

impl Default for ConstructGear {
    fn default() -> Self {
        Self {
            service: OnceLock::new(),
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
        let cfg: ConstructConfig = ctx.config_or_default()?;
        // @cpt-end:cpt-cf-construct-algo-gear-foundation-start-gear:p1:inst-start-config

        // @cpt-begin:cpt-cf-construct-algo-gear-foundation-start-gear:p1:inst-start-db
        let db: Arc<DBProvider<DbError>> = Arc::new(ctx.db_required()?);
        // @cpt-end:cpt-cf-construct-algo-gear-foundation-start-gear:p1:inst-start-db

        let repo = Arc::new(SeaOrmNoteRepository::new());

        // @cpt-begin:cpt-cf-construct-algo-gear-foundation-start-gear:p1:inst-start-authz
        let authz = ctx
            .client_hub()
            .get::<dyn AuthZResolverApi>()
            .context("failed to get AuthZ resolver")?;
        let policy_enforcer = PolicyEnforcer::new(authz);
        // @cpt-end:cpt-cf-construct-algo-gear-foundation-start-gear:p1:inst-start-authz

        // @cpt-begin:cpt-cf-construct-algo-gear-foundation-start-gear:p1:inst-start-config-range
        let service_config = ServiceConfig::try_from(&cfg)?;
        // @cpt-end:cpt-cf-construct-algo-gear-foundation-start-gear:p1:inst-start-config-range
        // @cpt-begin:cpt-cf-construct-algo-gear-foundation-start-gear:p1:inst-start-service
        let service = Arc::new(Service::new(db, repo, policy_enforcer, service_config));
        self.service
            .set(Arc::clone(&service))
            .map_err(|_| anyhow::anyhow!("{} gear already initialized", Self::MODULE_NAME))?;
        // @cpt-end:cpt-cf-construct-algo-gear-foundation-start-gear:p1:inst-start-service

        // @cpt-begin:cpt-cf-construct-algo-gear-foundation-start-gear:p1:inst-start-client
        let client: Arc<dyn ConstructClientV1> = Arc::new(LocalClient::new(service));
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
        let service = self
            .service
            .get()
            .map(Arc::clone)
            .ok_or_else(|| anyhow::anyhow!("Service not initialized"))?;

        let router = routes::register_routes(router, openapi, service);
        info!("Construct gear: REST routes registered");
        Ok(router)
        // @cpt-end:cpt-cf-construct-algo-gear-foundation-start-gear:p1:inst-start-rest
    }
}
