// Created: 2026-08-12 by Constructor Tech
//! The gear scaffold and its initialization.
//!
//! No `@cpt-dod` marker for `dod-gear-foundation-gear-scaffold` yet: it
//! also requires the client traits to be registered into `ClientHub`, which
//! waits on an implementation to register. The marker lands with the tick.

use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use authz_resolver_sdk::{AuthZResolverApi, PolicyEnforcer};
use sea_orm_migration::MigrationTrait;
use toolkit::api::OpenApiRegistry;
use toolkit::{DatabaseCapability, Gear, GearCtx, RestApiCapability};
use toolkit_db::{DBProvider, DbError};
use tracing::info;

use crate::config::SettingsServiceConfig;

/// The Settings Service gear.
///
/// Holds what initialization resolves, so later phases can hang services off it
/// without changing the startup contract.
#[toolkit::gear(name = "settings-service", deps = [authz_resolver], capabilities = [db, rest])]
pub struct SettingsService {
    config: OnceLock<Arc<SettingsServiceConfig>>,
    db: OnceLock<Arc<DBProvider<DbError>>>,
    enforcer: OnceLock<Arc<PolicyEnforcer>>,
    categories: OnceLock<
        Arc<
            crate::domain::category::CategoryService<
                crate::infra::storage::category_repo::CategoryRepo,
            >,
        >,
    >,
}

impl Default for SettingsService {
    fn default() -> Self {
        Self {
            config: OnceLock::new(),
            db: OnceLock::new(),
            enforcer: OnceLock::new(),
            categories: OnceLock::new(),
        }
    }
}

impl SettingsService {
    /// The bootstrap configuration, once initialization has run.
    ///
    /// # Errors
    /// Returns an error when called before [`Gear::init`].
    pub fn config(&self) -> anyhow::Result<Arc<SettingsServiceConfig>> {
        self.config
            .get()
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("{} gear not initialized", Self::MODULE_NAME))
    }

    /// The database handle, once initialization has run.
    ///
    /// # Errors
    /// Returns an error when called before [`Gear::init`].
    pub fn db(&self) -> anyhow::Result<Arc<DBProvider<DbError>>> {
        self.db
            .get()
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("{} gear not initialized", Self::MODULE_NAME))
    }

    /// The authorization enforcement point, once initialization has run.
    ///
    /// Every handler obtains its `AccessScope` through this rather than
    /// consulting the decision point directly, so the fail-closed projection in
    /// [`crate::api::authz`] cannot be bypassed by a handler that forgets it.
    ///
    /// # Errors
    /// Returns an error when called before [`Gear::init`].
    pub fn enforcer(&self) -> anyhow::Result<Arc<PolicyEnforcer>> {
        self.enforcer
            .get()
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("{} gear not initialized", Self::MODULE_NAME))
    }
}

#[async_trait]
impl Gear for SettingsService {
    async fn init(&self, ctx: &GearCtx) -> anyhow::Result<()> {
        // @cpt-begin:cpt-cf-settings-service-algo-gear-foundation-gear-init:p1:inst-gf-init-1
        // @cpt-begin:cpt-cf-settings-service-algo-gear-foundation-gear-init:p1:inst-gf-init-2
        // `config`, not `config_or_default`. Bootstrap values are
        // deployment-owned and are never managed settings, so there is nothing
        // to fall back to: an absent required value fails startup here rather
        // than surfacing later as the service enforcing something nobody chose.
        let config: SettingsServiceConfig = ctx.config()?;
        // @cpt-end:cpt-cf-settings-service-algo-gear-foundation-gear-init:p1:inst-gf-init-2
        // @cpt-end:cpt-cf-settings-service-algo-gear-foundation-gear-init:p1:inst-gf-init-1

        self.config
            .set(Arc::new(config))
            .map_err(|_| anyhow::anyhow!("{} gear already initialized", Self::MODULE_NAME))?;

        // @cpt-begin:cpt-cf-settings-service-algo-gear-foundation-gear-init:p1:inst-gf-init-3
        let db: Arc<DBProvider<DbError>> = Arc::new(ctx.db_required()?);
        // @cpt-end:cpt-cf-settings-service-algo-gear-foundation-gear-init:p1:inst-gf-init-3

        self.db
            .set(db)
            .map_err(|_| anyhow::anyhow!("{} gear already initialized", Self::MODULE_NAME))?;

        // @cpt-begin:cpt-cf-settings-service-algo-gear-foundation-gear-init:p1:inst-gf-init-6
        // Resolved at init, not per request: a decision point that cannot be
        // resolved must stop the gear coming up, rather than surfacing later as
        // a request-time denial indistinguishable from a real policy decision.
        let authz = ctx
            .client_hub()
            .get::<dyn AuthZResolverApi>()
            .map_err(|e| anyhow::anyhow!("failed to resolve the AuthZ resolver: {e}"))?;
        // @cpt-end:cpt-cf-settings-service-algo-gear-foundation-gear-init:p1:inst-gf-init-6

        self.enforcer
            .set(Arc::new(PolicyEnforcer::new(authz)))
            .map_err(|_| anyhow::anyhow!("{} gear already initialized", Self::MODULE_NAME))?;

        self.categories
            .set(Arc::new(crate::domain::category::CategoryService::new(
                crate::infra::storage::category_repo::CategoryRepo,
                Arc::new(crate::infra::audit_emitter::TracingAuditEmitter),
            )))
            .map_err(|_| anyhow::anyhow!("{} gear already initialized", Self::MODULE_NAME))?;

        // @cpt-begin:cpt-cf-settings-service-algo-gear-foundation-gear-init:p1:inst-gf-init-9
        info!("Settings Service gear initialized");
        // @cpt-end:cpt-cf-settings-service-algo-gear-foundation-gear-init:p1:inst-gf-init-9

        Ok(())
    }
}

impl RestApiCapability for SettingsService {
    fn register_rest(
        &self,
        _ctx: &GearCtx,
        router: axum::Router,
        openapi: &dyn OpenApiRegistry,
    ) -> anyhow::Result<axum::Router> {
        let service = self
            .categories
            .get()
            .ok_or_else(|| anyhow::anyhow!("category service not initialized"))?
            .clone();
        Ok(crate::api::rest::routes::register_routes(
            router,
            openapi,
            service,
            self.db()?,
            self.enforcer()?,
        ))
    }
}

impl DatabaseCapability for SettingsService {
    // @cpt-begin:cpt-cf-settings-service-algo-gear-foundation-gear-init:p1:inst-gf-init-4
    // @cpt-begin:cpt-cf-settings-service-algo-gear-foundation-gear-init:p1:inst-gf-init-5
    /// The gear's migrations, run to completion before it serves.
    ///
    /// `ToolKit` runs whatever is outstanding here and aborts startup if one
    /// fails, so steps 4 and 5 of gear init are satisfied by handing over the
    /// list rather than by driving it here — and a partially migrated schema is
    /// unreachable because no request is accepted until every migration has
    /// succeeded.
    fn migrations(&self) -> Vec<Box<dyn MigrationTrait>> {
        use sea_orm_migration::MigratorTrait;
        crate::infra::storage::migrations::Migrator::migrations()
    }
    // @cpt-end:cpt-cf-settings-service-algo-gear-foundation-gear-init:p1:inst-gf-init-5
    // @cpt-end:cpt-cf-settings-service-algo-gear-foundation-gear-init:p1:inst-gf-init-4
}

#[cfg(test)]
#[path = "gear_tests.rs"]
mod gear_tests;
