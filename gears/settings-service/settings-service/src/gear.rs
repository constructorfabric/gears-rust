// Created: 2026-08-12 by Constructor Tech
//! The gear scaffold and its initialization.
//!
//! No `@cpt-dod` marker for `dod-gear-foundation-gear-scaffold` yet: it
//! also requires the client traits to be registered into `ClientHub`, which
//! waits on an implementation to register. The marker lands with the tick.

use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use authz_resolver_sdk::PolicyEnforcer;
use sea_orm_migration::MigrationTrait;
use toolkit::api::OpenApiRegistry;
use toolkit::{DatabaseCapability, Gear, GearCtx, RestApiCapability};
use toolkit_db::{DBProvider, DbError};
use tracing::info;
use types_registry_sdk::TypesRegistryClient;

use crate::config::SettingsServiceConfig;

/// The Settings Service gear.
///
/// Holds what initialization resolves, so later phases can hang services off it
/// without changing the startup contract.
///
/// `deps` names the one gear this service calls during its **own** init — the
/// types registry — because a `deps` entry is an ordering claim that a gear
/// reading settings during *its* init could turn into an unsortable cycle
/// (DESIGN.md §4.9). Everything else is consumed: the authorization resolver is
/// declared below and wired by the runtime's proxy-wiring phase after init, so
/// the enforcer fetches it from the hub on first use; the tenant resolver is
/// fetched the same way (see [`crate::infra::platform_scope`]) — its SDK has no
/// REST projection yet, so it cannot carry a `consumes` declaration until it
/// does, and in the Embedded profile R1 is limited to the two paths coincide.
#[toolkit::consumes(contract = authz_resolver_sdk::AuthZResolverApi, from = "authz-resolver")]
#[toolkit::gear(name = "settings-service", deps = [types_registry], capabilities = [db, rest])]
pub struct SettingsService {
    config: OnceLock<Arc<SettingsServiceConfig>>,
    db: OnceLock<Arc<DBProvider<DbError>>>,
    enforcer: OnceLock<Arc<PolicyEnforcer>>,
    types: OnceLock<Arc<dyn TypesRegistryClient>>,
    categories: OnceLock<
        Arc<
            crate::domain::category::CategoryService<
                crate::infra::storage::category_repo::CategoryRepo,
            >,
        >,
    >,
    declarations: OnceLock<
        Arc<
            crate::domain::declaration::DeclarationService<
                crate::infra::storage::declaration_repo::DeclarationRepo,
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
            types: OnceLock::new(),
            categories: OnceLock::new(),
            declarations: OnceLock::new(),
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
    /// The enforcer itself resolves the decision point lazily from the hub: the
    /// resolver is a consumed client, wired after init (DESIGN.md §4.9), and a
    /// decision it cannot obtain is a denial, not an allow.
    ///
    /// # Errors
    /// Returns an error when called before [`Gear::init`].
    pub fn enforcer(&self) -> anyhow::Result<Arc<PolicyEnforcer>> {
        self.enforcer
            .get()
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("{} gear not initialized", Self::MODULE_NAME))
    }

    /// The GTS types registry, once initialization has run.
    ///
    /// A declaration's value type lives in the registry, not here: the read
    /// surface resolves its trait set for rendering, and declaration creation
    /// checks the type is a real catalogue entry. `has_secret_trait` is
    /// denormalised onto the row for masking precisely so that hot path does
    /// *not* come back through this client.
    ///
    /// # Errors
    /// Returns an error when called before [`Gear::init`].
    pub fn types(&self) -> anyhow::Result<Arc<dyn TypesRegistryClient>> {
        self.types
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
        // The one client called during our own init, and therefore the one
        // `deps` entry: registering the settings GTS schemas is a real call into
        // the registry, so it must already be up. A registry that is absent must
        // not first be discovered by a read that has already passed
        // authorization and reached the database.
        let types = ctx
            .client_hub()
            .get::<dyn TypesRegistryClient>()
            .map_err(|e| anyhow::anyhow!("failed to resolve the types registry: {e}"))?;
        self.types
            .set(types)
            .map_err(|_| anyhow::anyhow!("{} gear already initialized", Self::MODULE_NAME))?;

        // Consumed, not depended on. The authorization resolver is declared with
        // `#[toolkit::consumes]` on the struct and wired by the proxy-wiring
        // phase *after* init, so resolving it here would fail by construction;
        // the enforcer fetches it from the hub per call and denies when it
        // cannot. The tenant resolver is fetched the same way, on the first
        // platform-scoped mutation that needs the root tenant's id. Neither is
        // an ordering claim on the rest of the platform (DESIGN.md §4.9).
        let hub = ctx.client_hub();
        self.enforcer
            .set(Arc::new(PolicyEnforcer::from_hub(Arc::clone(&hub))))
            .map_err(|_| anyhow::anyhow!("{} gear already initialized", Self::MODULE_NAME))?;
        let platform_scope = Arc::new(crate::infra::platform_scope::HubPlatformScope::new(hub));
        // @cpt-end:cpt-cf-settings-service-algo-gear-foundation-gear-init:p1:inst-gf-init-6

        self.categories
            .set(Arc::new(crate::domain::category::CategoryService::new(
                crate::infra::storage::category_repo::CategoryRepo,
                Arc::new(crate::infra::audit_emitter::TracingAuditEmitter),
                platform_scope,
            )))
            .map_err(|_| anyhow::anyhow!("{} gear already initialized", Self::MODULE_NAME))?;

        self.declarations
            .set(Arc::new(
                crate::domain::declaration::DeclarationService::new(
                    crate::infra::storage::declaration_repo::DeclarationRepo,
                    self.types()?,
                ),
            ))
            .map_err(|_| anyhow::anyhow!("{} gear already initialized", Self::MODULE_NAME))?;

        // @cpt-begin:cpt-cf-settings-service-algo-gear-foundation-gear-init:p1:inst-gf-init-11
        info!("Settings Service gear initialized");
        // @cpt-end:cpt-cf-settings-service-algo-gear-foundation-gear-init:p1:inst-gf-init-11

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
        let declarations = self
            .declarations
            .get()
            .ok_or_else(|| anyhow::anyhow!("declaration service not initialized"))?
            .clone();
        let router = crate::api::rest::routes::register_routes(
            router,
            openapi,
            service,
            self.db()?,
            self.enforcer()?,
        );
        Ok(crate::api::rest::declaration_routes::register_routes(
            router,
            openapi,
            declarations,
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
