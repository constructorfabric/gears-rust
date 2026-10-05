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
        info!("Providing construct database migrations");
        crate::infra::storage::migrations::Migrator::migrations()
    }
}

#[async_trait]
impl Gear for ConstructGear {
    async fn init(&self, ctx: &GearCtx) -> anyhow::Result<()> {
        let cfg: ConstructConfig = ctx.config_or_default()?;

        let db: Arc<DBProvider<DbError>> = Arc::new(ctx.db_required()?);

        let repo = Arc::new(SeaOrmNoteRepository::new());

        let authz = ctx
            .client_hub()
            .get::<dyn AuthZResolverApi>()
            .context("failed to get AuthZ resolver")?;
        let policy_enforcer = PolicyEnforcer::new(authz);

        let service_config = ServiceConfig::try_from(&cfg)?;
        let service = Arc::new(Service::new(db, repo, policy_enforcer, service_config));
        self.service
            .set(Arc::clone(&service))
            .map_err(|_| anyhow::anyhow!("{} gear already initialized", Self::MODULE_NAME))?;

        let client: Arc<dyn ConstructClientV1> = Arc::new(LocalClient::new(service));
        ctx.client_hub().register(client);

        Ok(())
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
        let service = self
            .service
            .get()
            .map(Arc::clone)
            .ok_or_else(|| anyhow::anyhow!("Service not initialized"))?;

        let router = routes::register_routes(router, openapi, service);
        info!("Construct gear: REST routes registered");
        Ok(router)
    }
}
