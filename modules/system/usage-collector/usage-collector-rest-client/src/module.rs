//! `usage-collector-rest-client` `ModKit` module.

use std::sync::Arc;

use async_trait::async_trait;
use authn_resolver_sdk::AuthNResolverClient;
use authz_resolver_sdk::AuthZResolverClient;
use modkit::contracts::DatabaseCapability;
use modkit::{Module, ModuleCtx};
use sea_orm_migration::MigrationTrait;
use tracing::info;
use usage_emitter::{UsageEmitterRuntime, UsageEmitterRuntimeV1};

use crate::config::UsageCollectorRestClientConfig;
use crate::infra::UsageCollectorRestClient;

/// Satisfies the `"usage-collector-client"` `ModKit` dependency for separate binaries
/// by sending usage records to a remote collector REST API with s2s bearer auth.
#[modkit::module(
    name = "usage-collector-rest-client",
    deps = ["authn-resolver", "authz-resolver"],
    capabilities = [db],
)]
#[derive(Default)]
struct UsageCollectorRestClientModule;

#[async_trait]
impl Module for UsageCollectorRestClientModule {
    // @cpt-algo:cpt-cf-usage-collector-algo-rest-ingest-module-init:p1
    async fn init(&self, ctx: &ModuleCtx) -> anyhow::Result<()> {
        // @cpt-begin:cpt-cf-usage-collector-algo-rest-ingest-module-init:p1:inst-init-1
        let cfg: UsageCollectorRestClientConfig = ctx.config_expanded()?;
        cfg.validate()?;
        info!(?cfg, "Loaded {} configuration", Self::MODULE_NAME,);
        // @cpt-end:cpt-cf-usage-collector-algo-rest-ingest-module-init:p1:inst-init-1

        // @cpt-begin:cpt-cf-usage-collector-algo-rest-ingest-module-init:p1:inst-init-2
        let db = ctx
            .db_required()
            .map_err(|e| anyhow::anyhow!("{}: db not available: {e}", Self::MODULE_NAME))?
            .db();
        // @cpt-end:cpt-cf-usage-collector-algo-rest-ingest-module-init:p1:inst-init-2

        // @cpt-begin:cpt-cf-usage-collector-algo-rest-ingest-module-init:p1:inst-init-3
        let authentication = ctx
            .client_hub()
            .get::<dyn AuthNResolverClient>()
            .map_err(|e| {
                anyhow::anyhow!(
                    "{}: AuthNResolverClient not registered: {e}",
                    Self::MODULE_NAME
                )
            })?;
        // @cpt-end:cpt-cf-usage-collector-algo-rest-ingest-module-init:p1:inst-init-3

        // @cpt-begin:cpt-cf-usage-collector-algo-rest-ingest-module-init:p1:inst-init-4
        let authorization = ctx
            .client_hub()
            .get::<dyn AuthZResolverClient>()
            .map_err(|e| {
                anyhow::anyhow!(
                    "{}: AuthZResolverClient not registered: {e}",
                    Self::MODULE_NAME
                )
            })?;
        // @cpt-end:cpt-cf-usage-collector-algo-rest-ingest-module-init:p1:inst-init-4

        // @cpt-begin:cpt-cf-usage-collector-algo-rest-ingest-module-init:p1:inst-init-5
        let collector = UsageCollectorRestClient::new(&cfg, authentication).map_err(|e| {
            anyhow::anyhow!("{}: failed to build HTTP client: {e}", Self::MODULE_NAME)
        })?;
        let collector = Arc::new(collector);
        // @cpt-end:cpt-cf-usage-collector-algo-rest-ingest-module-init:p1:inst-init-5

        // @cpt-begin:cpt-cf-usage-collector-algo-rest-ingest-module-init:p1:inst-init-6
        let runtime = UsageEmitterRuntime::build(cfg.emitter, db, authorization, collector)
            .await
            .map_err(|e| {
                anyhow::anyhow!(
                    "{}: failed to build usage emitter runtime: {e}",
                    Self::MODULE_NAME
                )
            })?;
        let runtime = Arc::new(runtime);
        // @cpt-end:cpt-cf-usage-collector-algo-rest-ingest-module-init:p1:inst-init-6
        // @cpt-begin:cpt-cf-usage-collector-algo-rest-ingest-module-init:p1:inst-init-7
        ctx.client_hub()
            .register::<dyn UsageEmitterRuntimeV1>(runtime);
        // @cpt-end:cpt-cf-usage-collector-algo-rest-ingest-module-init:p1:inst-init-7

        Ok(())
    }
}

// @cpt-dod:cpt-cf-usage-collector-dod-rest-ingest-rest-client-crate:p1
impl DatabaseCapability for UsageCollectorRestClientModule {
    fn migrations(&self) -> Vec<Box<dyn MigrationTrait>> {
        info!("Providing {} database migrations", Self::MODULE_NAME);
        modkit_db::outbox::outbox_migrations()
    }
}
