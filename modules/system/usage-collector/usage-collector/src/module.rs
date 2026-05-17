//! Usage-collector gateway `ModKit` module.

use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use authz_resolver_sdk::AuthZResolverClient;
use axum::Router;
use modkit::api::OpenApiRegistry;
use modkit::contracts::{DatabaseCapability, RestApiCapability};
use modkit::{Module, ModuleCtx};
use sea_orm_migration::MigrationTrait;
use tracing::info;
use types_registry_sdk::{RegisterResult, TypesRegistryClient};
use usage_collector_sdk::UsageCollectorPluginSpecV1;
use usage_emitter::{UsageEmitterRuntime, UsageEmitterRuntimeV1};

use crate::api::rest::routes;
use crate::config::UsageCollectorConfig;
use crate::domain::{Service, UsageCollectorLocalClient};

/// Usage collector gateway: registers plugin schema, resolves plugins via GTS,
/// exposes `dyn UsageCollectorClientV1` for outbox delivery, and wires REST endpoints
/// via `DatabaseCapability` and `RestApiCapability`.
#[modkit::module(
    name = "usage-collector",
    deps = ["authz-resolver", "types-registry"],
    capabilities = [db, rest],
)]
#[derive(Default)]
pub struct UsageCollectorModule {
    /// Gateway service stored during `init()` for use in `register_rest()`.
    service: OnceLock<Arc<Service>>,
}

#[async_trait]
impl Module for UsageCollectorModule {
    #[tracing::instrument(skip_all, fields(vendor))]
    async fn init(&self, ctx: &ModuleCtx) -> anyhow::Result<()> {
        let cfg: UsageCollectorConfig = ctx.config_or_default()?;
        cfg.validate()?;
        tracing::Span::current().record("vendor", &cfg.vendor);
        info!(
            cfg.vendor,
            ?cfg.plugin_timeout,
            "Loaded {} configuration",
            Self::MODULE_NAME,
        );

        let registry = ctx.client_hub().get::<dyn TypesRegistryClient>()?;
        let schema_str = UsageCollectorPluginSpecV1::gts_schema_with_refs_as_string();
        let schema_json: serde_json::Value = serde_json::from_str(&schema_str)?;
        let results = registry.register(vec![schema_json]).await?;
        RegisterResult::ensure_all_ok(&results)?;
        info!(
            schema_id = %UsageCollectorPluginSpecV1::gts_schema_id(),
            "Registered {} plugin schema in types-registry",
            Self::MODULE_NAME,
        );

        let db = ctx
            .db_required()
            .map_err(|e| anyhow::anyhow!("{}: db not available: {e}", Self::MODULE_NAME))?
            .db();

        let authz = ctx
            .client_hub()
            .get::<dyn AuthZResolverClient>()
            .map_err(|e| {
                anyhow::anyhow!(
                    "{}: AuthZResolverClient not registered: {e}",
                    Self::MODULE_NAME
                )
            })?;

        let service = Service::new(cfg.clone(), ctx.client_hub(), Arc::clone(&authz));
        let service = Arc::new(service);
        self.service
            .set(Arc::clone(&service))
            .map_err(|_| anyhow::anyhow!("{} module already initialized", Self::MODULE_NAME))?;

        let collector = UsageCollectorLocalClient::new(service);
        let collector = Arc::new(collector);

        let runtime = UsageEmitterRuntime::build(cfg.emitter, db, authz, collector).await?;
        let runtime = Arc::new(runtime);
        ctx.client_hub()
            .register::<dyn UsageEmitterRuntimeV1>(runtime);

        Ok(())
    }
}

impl DatabaseCapability for UsageCollectorModule {
    fn migrations(&self) -> Vec<Box<dyn MigrationTrait>> {
        info!("Providing {} database migrations", Self::MODULE_NAME);
        modkit_db::outbox::outbox_migrations()
    }
}

impl RestApiCapability for UsageCollectorModule {
    fn register_rest(
        &self,
        ctx: &ModuleCtx,
        router: Router,
        openapi: &dyn OpenApiRegistry,
    ) -> anyhow::Result<Router> {
        tracing::info!("Registering {} REST routes", Self::MODULE_NAME);

        let runtime = ctx
            .client_hub()
            .get::<dyn UsageEmitterRuntimeV1>()
            .map_err(|e| {
                anyhow::anyhow!(
                    "{}: UsageEmitterRuntimeV1 not registered: {e}",
                    Self::MODULE_NAME
                )
            })?;

        let service = self
            .service
            .get()
            .ok_or_else(|| anyhow::anyhow!("{}: service not initialized", Self::MODULE_NAME))?;
        let service = Arc::clone(service);

        let router = routes::register_routes(router, openapi, runtime, service);
        tracing::info!("{} REST routes registered", Self::MODULE_NAME);

        Ok(router)
    }
}
