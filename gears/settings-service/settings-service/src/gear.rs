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

use crate::domain::platform_scope::PlatformScope;
use crate::domain::validation::TypeValidator;
use settings_service_sdk::api::{SettingsContributionClient, SettingsReaderClient};

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
/// The resolver over the concrete repositories, as the gear and its REST
/// surface share it.
pub type ConcreteResolver = crate::domain::resolution::ValueResolver<
    crate::infra::storage::declaration_repo::DeclarationRepo,
    crate::infra::storage::value_repo::ValueRepo,
    crate::infra::storage::access_repo::AccessRepo,
>;

#[toolkit::consumes(contract = authz_resolver_sdk::AuthZResolverApi, from = "authz-resolver")]
#[toolkit::gear(name = "settings-service", deps = [types_registry, credstore], capabilities = [db, rest])]
pub struct SettingsService {
    config: OnceLock<Arc<SettingsServiceConfig>>,
    db: OnceLock<Arc<DBProvider<DbError>>>,
    enforcer: OnceLock<Arc<PolicyEnforcer>>,
    types: OnceLock<Arc<dyn TypesRegistryClient>>,
    validator: OnceLock<Arc<dyn TypeValidator>>,
    categories: OnceLock<
        Arc<
            crate::domain::category::CategoryService<
                crate::infra::storage::category_repo::CategoryRepo,
                crate::infra::storage::audit_store::AuditStore,
            >,
        >,
    >,
    resolver: OnceLock<Arc<ConcreteResolver>>,
    writes: OnceLock<Arc<crate::infra::value_writes::WriteCoordinator>>,
    access: OnceLock<Arc<crate::api::rest::access_handlers::ConcreteAccessService>>,
    hierarchy: OnceLock<Arc<dyn crate::domain::resolution::TenantHierarchy>>,
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
            validator: OnceLock::new(),
            categories: OnceLock::new(),
            resolver: OnceLock::new(),
            writes: OnceLock::new(),
            access: OnceLock::new(),
            hierarchy: OnceLock::new(),
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
    /// The Type Validator, once initialization has run.
    ///
    /// Generic over any GTS type id; for a setting the id passed is the
    /// declaration's `value_type_id`. Every rule it enforces is hard, and a type
    /// it cannot resolve is a rejection rather than a vacuous pass.
    ///
    /// # Errors
    /// Returns an error when called before [`Gear::init`].
    pub fn validator(&self) -> anyhow::Result<Arc<dyn TypeValidator>> {
        self.validator
            .get()
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("{} gear not initialized", Self::MODULE_NAME))
    }

    /// The Value Resolver, once initialized.
    ///
    /// # Errors
    /// If called before `init` completed.
    pub fn resolver(&self) -> anyhow::Result<Arc<ConcreteResolver>> {
        self.resolver
            .get()
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("{} resolver not initialized", Self::MODULE_NAME))
    }

    /// The write coordinator, once initialized.
    ///
    /// # Errors
    /// If called before `init` completed.
    pub fn writes(&self) -> anyhow::Result<Arc<crate::infra::value_writes::WriteCoordinator>> {
        self.writes
            .get()
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("{} writes not initialized", Self::MODULE_NAME))
    }

    /// The tenant access service, once initialized.
    ///
    /// # Errors
    /// If called before `init` completed.
    pub fn access(
        &self,
    ) -> anyhow::Result<Arc<crate::api::rest::access_handlers::ConcreteAccessService>> {
        self.access
            .get()
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("{} access not initialized", Self::MODULE_NAME))
    }

    /// The tenant hierarchy port, once initialized.
    ///
    /// # Errors
    /// If called before `init` completed.
    pub fn hierarchy(&self) -> anyhow::Result<Arc<dyn crate::domain::resolution::TenantHierarchy>> {
        self.hierarchy
            .get()
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("{} hierarchy not initialized", Self::MODULE_NAME))
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
        // Built over the registry client above: validation of a value against
        // its type is the one path that consults the registry per call, and it
        // fails closed on a type the registry does not know.
        self.validator
            .set(Arc::new(
                crate::infra::type_validator::GtsTypeValidator::new(self.types()?),
            ))
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
        let platform_scope: Arc<dyn PlatformScope> =
            Arc::new(crate::infra::platform_scope::HubPlatformScope::new(hub));
        // @cpt-end:cpt-cf-settings-service-algo-gear-foundation-gear-init:p1:inst-gf-init-6

        // The audit sink: the gear's own table, written in each mutation's
        // transaction. The retention default is validated here because a store
        // configured below twelve months would prune what the platform must keep.
        let config = self.config()?;
        if config.audit_retention_days < crate::audit::MIN_RETENTION_DAYS {
            anyhow::bail!(
                "{}: audit_retention_days is {} but must not be below {}",
                Self::MODULE_NAME,
                config.audit_retention_days,
                crate::audit::MIN_RETENTION_DAYS
            );
        }
        let audit = crate::infra::storage::audit_store::AuditStore;
        self.categories
            .set(Arc::new(crate::domain::category::CategoryService::new(
                crate::infra::storage::category_repo::CategoryRepo,
                audit,
                Arc::clone(&platform_scope),
            )))
            .map_err(|_| anyhow::anyhow!("{} gear already initialized", Self::MODULE_NAME))?;

        // The contribution door: gears register their declarations through this
        // trait from their own init, so it is bound into the hub here and each
        // caller names `settings-service` in its `deps` to initialize after us.
        // The read path: the local effective-value cache — this gear's
        // `cache_ttl_seconds` is its knob — the tenant hierarchy port over the
        // tenant resolver, the resolver over both repositories, and the
        // in-process reader bound into the hub for every consuming gear.
        let cache = Arc::new(crate::domain::resolution::EffectiveCache::new(
            std::time::Duration::from_secs(self.config()?.cache_ttl_seconds),
        ));
        let hierarchy: Arc<dyn crate::domain::resolution::TenantHierarchy> = Arc::new(
            crate::infra::tenant_hierarchy::HubTenantHierarchy::new(ctx.client_hub()),
        );
        self.hierarchy
            .set(Arc::clone(&hierarchy))
            .map_err(|_| anyhow::anyhow!("{} gear already initialized", Self::MODULE_NAME))?;
        let hierarchy_for_access = Arc::clone(&hierarchy);
        let resolver = Arc::new(crate::domain::resolution::ValueResolver::new(
            crate::infra::storage::declaration_repo::DeclarationRepo,
            crate::infra::storage::value_repo::ValueRepo,
            crate::infra::storage::access_repo::AccessRepo,
            hierarchy,
            Arc::clone(&platform_scope),
            self.validator()?,
            Arc::clone(&cache),
        ));
        self.resolver
            .set(Arc::clone(&resolver))
            .map_err(|_| anyhow::anyhow!("{} gear already initialized", Self::MODULE_NAME))?;
        // Tenant access restrictions: set, clear, read and list, each mutation in
        // its own transaction with its record, evicting the restricted subtree.
        self.access
            .set(Arc::new(crate::domain::access::AccessService::new(
                crate::infra::storage::declaration_repo::DeclarationRepo,
                crate::infra::storage::access_repo::AccessRepo,
                crate::infra::storage::audit_store::AuditStore,
                Arc::clone(&hierarchy_for_access),
                Arc::clone(&platform_scope),
                Arc::clone(&cache),
            )))
            .map_err(|_| anyhow::anyhow!("{} gear already initialized", Self::MODULE_NAME))?;

        // The write path: the step-up verifier from configuration — the OIDC/JWKS
        // binding when a section is present, otherwise the binding that refuses
        // every write needing step-up while reads keep serving — over the same
        // resolver, audit store, and the ports whose real bindings come later.
        let step_up: Arc<dyn crate::domain::stepup::StepUpVerifier> = match &config.step_up {
            Some(section) => Arc::new(crate::infra::step_up::OidcStepUpVerifier::from_config(
                section,
            )?),
            None => Arc::new(crate::domain::stepup::NoStepUpVerifier::default()),
        };
        // The Secret Manager over the Credential Store: `credstore` is a system
        // gear, so its client is in the hub before this init runs.
        let credstore = ctx
            .client_hub()
            .get::<dyn credstore_sdk::CredStoreClientV1>()
            .map_err(|e| anyhow::anyhow!("{}: credstore client: {e}", Self::MODULE_NAME))?;
        let secrets: Arc<dyn crate::domain::ports::SecretManager> = Arc::new(
            crate::infra::secret_manager::CredStoreSecretManager::new(credstore),
        );
        let writer = Arc::new(crate::domain::writes::ValueWriter::new(
            crate::infra::storage::value_repo::ValueRepo,
            Arc::clone(&resolver),
            self.validator()?,
            crate::infra::storage::audit_store::AuditStore,
            step_up,
            Arc::clone(&secrets),
            Arc::new(crate::infra::write_metrics::LoggingPublisher),
            Arc::new(crate::infra::write_metrics::OtelWriteMetrics::new()),
        ));
        self.writes
            .set(Arc::new(crate::infra::value_writes::WriteCoordinator::new(
                self.db()?,
                writer,
            )))
            .map_err(|_| anyhow::anyhow!("{} gear already initialized", Self::MODULE_NAME))?;

        // @cpt-begin:cpt-cf-settings-service-algo-gear-foundation-gear-init:p1:inst-gf-init-7
        // The machine-only plaintext path behind the reader: per-setting PEP
        // gate, the Secret Manager, and the audit store for `secret_use`.
        let secret_resolver = Arc::new(crate::domain::secrets::SecretResolver::new(
            Arc::clone(&resolver),
            Arc::clone(&secrets),
            Arc::new(crate::infra::secret_manager::PepSecretGate::new(
                self.enforcer()?,
            )),
            crate::infra::storage::audit_store::AuditStore,
        ));
        let reader: Arc<dyn SettingsReaderClient> =
            Arc::new(crate::infra::reader_client::ReaderClient::new(
                self.db()?,
                Arc::clone(&resolver),
                secret_resolver,
            ));
        ctx.client_hub()
            .register::<dyn SettingsReaderClient>(reader);

        let contributions = Arc::new(crate::domain::contribution::ContributionService::new(
            crate::infra::storage::declaration_repo::DeclarationRepo,
            crate::infra::storage::category_repo::CategoryRepo,
            crate::infra::storage::value_repo::ValueRepo,
            self.validator()?,
            Arc::new(
                crate::infra::setting_type_registrar::TypesRegistryRegistrar::new(self.types()?),
            ),
            audit,
            platform_scope,
        ));
        let contribution_client: Arc<dyn SettingsContributionClient> =
            Arc::new(crate::infra::contribution_client::ContributionClient::new(
                self.db()?,
                contributions,
                cache,
            ));
        ctx.client_hub()
            .register::<dyn SettingsContributionClient>(contribution_client);
        // @cpt-end:cpt-cf-settings-service-algo-gear-foundation-gear-init:p1:inst-gf-init-7

        self.declarations
            .set(Arc::new(
                crate::domain::declaration::DeclarationService::new(
                    crate::infra::storage::declaration_repo::DeclarationRepo,
                    self.types()?,
                ),
            ))
            .map_err(|_| anyhow::anyhow!("{} gear already initialized", Self::MODULE_NAME))?;

        // @cpt-begin:cpt-cf-settings-service-algo-gear-foundation-gear-init:p1:inst-gf-init-12
        info!("Settings Service gear initialized");
        // @cpt-end:cpt-cf-settings-service-algo-gear-foundation-gear-init:p1:inst-gf-init-12

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
        let router = crate::api::rest::declaration_routes::register_routes(
            router,
            openapi,
            declarations,
            self.db()?,
            self.enforcer()?,
        );
        let router = crate::api::rest::setting_routes::register_routes(
            router,
            openapi,
            self.resolver()?,
            self.db()?,
            self.enforcer()?,
        );
        let router = crate::api::rest::value_routes::register_routes(
            router,
            openapi,
            self.writes()?,
            self.enforcer()?,
        );
        Ok(crate::api::rest::access_routes::register_routes(
            router,
            openapi,
            self.access()?,
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
