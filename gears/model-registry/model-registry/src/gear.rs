//! Gear registration and wiring (DESIGN §3.4, §3.8).

use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use authz_resolver_sdk::{AuthZResolverApi, PolicyEnforcer};
use axum::Router;
use model_registry_sdk::ModelRegistryClientV1;
use tenant_resolver_sdk::TenantResolverClient;
use toolkit::api::OpenApiRegistry;
use toolkit::{Gear, GearCtx};
use toolkit_db::DBProvider;
use tracing::info;
use types_registry_sdk::TypesRegistryApi;

use crate::api::rest::routes;
use crate::config::ModelRegistryConfig;
use crate::domain::access_guard::AccessGuard;
use crate::domain::clock::SystemClock;
use crate::domain::error::DomainError;
use crate::domain::local_client::LocalClient;
use crate::domain::service::Service;
use crate::infra::chain_cache::ChainCache;
use crate::infra::credstore::CredStoreSecretRefSource;
use crate::infra::types_registry::TypesRegistrySchemaSource;

// `credstore` joins `deps` at the credstore v2 switch (gears/credstore/v2/plan.md);
// until then the V2 client is looked up lazily and writes that bind a new
// `secret_ref` fail closed.
#[toolkit::consumes(contract = types_registry_sdk::TypesRegistryApi, from = "types-registry")]
#[toolkit::gear(
    name = "model-registry",
    deps = [authz_resolver, tenant_resolver, types_registry],
    capabilities = [rest, db]
)]
pub struct ModelRegistryGear {
    service: OnceLock<Arc<Service>>,
}

impl Default for ModelRegistryGear {
    fn default() -> Self {
        Self {
            service: OnceLock::new(),
        }
    }
}

impl toolkit::contracts::DatabaseCapability for ModelRegistryGear {
    fn migrations(&self) -> Vec<Box<dyn sea_orm_migration::MigrationTrait>> {
        use sea_orm_migration::MigratorTrait;
        crate::infra::storage::migrations::Migrator::migrations()
    }
}

#[async_trait]
impl Gear for ModelRegistryGear {
    async fn init(&self, ctx: &GearCtx) -> anyhow::Result<()> {
        let cfg: ModelRegistryConfig = ctx.config_or_default()?;
        cfg.validate()?;

        let db: Arc<DBProvider<DomainError>> = Arc::new(DBProvider::new(ctx.db_required()?.db()));

        let tenant_resolver = ctx
            .client_hub()
            .get::<dyn TenantResolverClient>()
            .map_err(|e| anyhow::anyhow!("failed to get TenantResolverClient: {e}"))?;
        let authz = ctx
            .client_hub()
            .get::<dyn AuthZResolverApi>()
            .map_err(|e| anyhow::anyhow!("failed to get AuthZResolverApi: {e}"))?;
        let types_registry = ctx
            .client_hub()
            .get::<dyn TypesRegistryApi>()
            .map_err(|e| anyhow::anyhow!("failed to get TypesRegistryApi: {e}"))?;

        let clock = Arc::new(SystemClock);
        let chain_cache = Arc::new(ChainCache::new(
            tenant_resolver,
            clock.clone(),
            cfg.chain_cache_ttl(),
            cfg.chain_cache_max_entries,
            cfg.tenant_resolver_deadline(),
        ));
        let guard = AccessGuard::new(
            PolicyEnforcer::new(authz).with_deadline(cfg.pdp_deadline()),
            cfg.max_pdp_constraint_values,
        );
        let schemas = Arc::new(TypesRegistrySchemaSource::new(
            types_registry,
            cfg.types_registry_deadline(),
        ));

        let secret_refs = Arc::new(CredStoreSecretRefSource::new(
            ctx.client_hub(),
            cfg.credstore_deadline(),
        ));

        let service = Arc::new(Service::new(
            db,
            chain_cache,
            guard,
            schemas,
            secret_refs,
            clock,
            &cfg,
        ));
        self.service
            .set(service.clone())
            .map_err(|_| anyhow::anyhow!("{} gear already initialized", Self::MODULE_NAME))?;

        let local_client: Arc<dyn ModelRegistryClientV1> = Arc::new(LocalClient::new(service));
        ctx.client_hub().register(local_client);

        info!(
            chain_cache_ttl_seconds = cfg.chain_cache_ttl_seconds,
            "model-registry gear initialized"
        );
        Ok(())
    }
}

#[async_trait]
impl toolkit::contracts::RestApiCapability for ModelRegistryGear {
    fn register_rest(
        &self,
        _ctx: &GearCtx,
        router: Router,
        openapi: &dyn OpenApiRegistry,
    ) -> anyhow::Result<Router> {
        let service = self
            .service
            .get()
            .ok_or_else(|| anyhow::anyhow!("model-registry service not initialized"))?
            .clone();
        Ok(routes::register_routes(router, openapi, service))
    }
}
