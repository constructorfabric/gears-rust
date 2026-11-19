// Created: 2026-04-16 by Constructor Tech
// Updated: 2026-04-28 by Constructor Tech
// @cpt-dod:cpt-cf-resource-group-dod-e2e-test-suite:p1
use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use authz_resolver_sdk::PolicyEnforcer;
use resource_group_sdk::{ResourceGroupClient, ResourceGroupReadHierarchy};
use sea_orm_migration::MigrationTrait;
use toolkit::api::OpenApiRegistry;
use toolkit::{DatabaseCapability, Gear, GearCtx, RestApiCapability};
use toolkit_db::DBProvider;
use toolkit_db::DbError;
use tracing::info;

use crate::api::rest::routes;
use crate::config::ResourceGroupConfig;
use crate::domain::group_service::{GroupService, QueryProfile, TenantTypes};
use crate::domain::membership_service::MembershipService;
use crate::domain::read_service::RgReadService;
use crate::domain::rg_service::RgService;
use crate::domain::type_catalog::RgTypeCatalog;
use crate::infra::storage::group_repo::GroupRepository;
use crate::infra::storage::membership_repo::MembershipRepository;
use crate::infra::types_registry::RegistryTypeCatalog;

pub type ConcreteGroupService = GroupService<GroupRepository>;
pub type ConcreteMembershipService = MembershipService<GroupRepository, MembershipRepository>;
pub type ConcreteRgService = RgService<GroupRepository, MembershipRepository>;

// @cpt-dod:cpt-cf-resource-group-dod-sdk-foundation-gear-scaffold:p1
// @cpt-dod:cpt-cf-resource-group-dod-type-mgmt-managed-publication:p1
/// Resource-group gear.
///
/// Publication (`gts_publication(...)`, run by the runtime before any application
/// `init`): the SDK's group and membership base schemas.
///
/// RG type definitions live only in the types registry: `init` takes its
/// tenant API (`TypesRegistryApi`, consumed and wired before the application
/// group, local or remote) and reads definitions under each caller's context.
#[toolkit::consumes(contract = authz_resolver_sdk::AuthZResolverApi, from = "authz-resolver")]
#[toolkit::consumes(contract = types_registry_sdk::TypesRegistryApi, from = "types-registry")]
#[toolkit::gear(
    name = "resource-group",
    capabilities = [db, rest],
    gts_publication(crates = [resource_group_sdk]),
)]
#[allow(clippy::struct_field_names)]
pub struct ResourceGroup {
    group_service: OnceLock<Arc<ConcreteGroupService>>,
    membership_service: OnceLock<Arc<ConcreteMembershipService>>,
}

impl Default for ResourceGroup {
    fn default() -> Self {
        Self {
            group_service: OnceLock::new(),
            membership_service: OnceLock::new(),
        }
    }
}

#[async_trait]
impl Gear for ResourceGroup {
    async fn init(&self, ctx: &GearCtx) -> anyhow::Result<()> {
        let cfg: ResourceGroupConfig = ctx.config_or_default()?;
        cfg.validate()?;
        info!(
            tenant_type_paths = ?cfg.tenant_type_paths,
            "Loaded resource-group configuration"
        );

        // Acquire DB capability (secure wrapper)
        let db: Arc<DBProvider<DbError>> = Arc::new(ctx.db_required()?);

        // The AuthZ resolver is consumed, wired after `init` (locally or
        // remotely); the enforcer fetches it from the hub per call.
        // RG owns the canonical membership and closure tables, but its group
        // resources are nested through `parent_id`/`resource_group_closure`,
        // not represented by membership rows. Their descriptors do not opt in
        // to native group predicates, and RG does not advertise the service-
        // level capabilities either; the PDP must expand group scopes to
        // explicit `in` predicates, or deny.
        let enforcer = PolicyEnforcer::from_hub(ctx.client_hub());

        // Create repo instances
        let group_repo = Arc::new(GroupRepository);
        let membership_repo = Arc::new(MembershipRepository);

        // RG type definitions: the types registry's tenant API, read under
        // each caller's `SecurityContext`. No local definition catalogue or cache (`gts_type` is only an identity dictionary).
        let registry: Arc<dyn types_registry_sdk::TypesRegistryApi> = ctx
            .client_hub()
            .get::<dyn types_registry_sdk::TypesRegistryApi>()
            .map_err(|e| anyhow::anyhow!("failed to get TypesRegistryApi: {e}"))?;
        let types: Arc<dyn RgTypeCatalog> = Arc::new(RegistryTypeCatalog::new(registry));

        // Create GroupService with default query profile and PolicyEnforcer
        let profile = QueryProfile::default();
        // The composition root is where the metrics recorder is chosen. The
        // service itself defaults to the no-op one, so nothing that builds a
        // `GroupService` outside this path -- tests, mostly -- records or
        // pays for anything.
        let group_service = Arc::new(
            GroupService::new(
                db.clone(),
                profile,
                enforcer.clone(),
                group_repo.clone(),
                types.clone(),
            )
            .with_metrics(Arc::new(
                crate::infra::metrics::RgMetricsMeter::from_global(),
            ))
            .with_tenant_types(TenantTypes::new(cfg.tenant_type_paths)),
        );

        self.group_service
            .set(group_service)
            .map_err(|_| anyhow::anyhow!("{} gear already initialized", Self::MODULE_NAME))?;

        // Create MembershipService with PolicyEnforcer for AuthZ enforcement
        let membership_service = Arc::new(MembershipService::new(
            db,
            enforcer,
            group_repo,
            types,
            membership_repo,
        ));
        self.membership_service
            .set(membership_service.clone())
            .map_err(|_| anyhow::anyhow!("{} gear already initialized", Self::MODULE_NAME))?;

        // Application init: register SDK clients before client wiring and route composition.
        let group_svc = self
            .group_service
            .get()
            .ok_or_else(|| anyhow::anyhow!("{} group_service not initialized", Self::MODULE_NAME))?
            .clone();

        let rg_client: Arc<dyn ResourceGroupClient> = Arc::new(RgService::new(
            group_svc.clone(),
            membership_service.clone(),
        ));
        ctx.client_hub()
            .register::<dyn ResourceGroupClient>(rg_client);

        let read_client: Arc<dyn ResourceGroupReadHierarchy> =
            Arc::new(RgReadService::new(group_svc, membership_service));
        ctx.client_hub()
            .register::<dyn ResourceGroupReadHierarchy>(read_client);

        info!(
            "Resource Group gear initialized (ClientHub: ResourceGroupClient + ResourceGroupReadHierarchy)"
        );
        Ok(())
    }
}

impl DatabaseCapability for ResourceGroup {
    fn migrations(&self) -> Vec<Box<dyn MigrationTrait>> {
        use sea_orm_migration::MigratorTrait;
        info!("Providing resource_group database migrations");
        crate::infra::storage::migrations::Migrator::migrations()
    }
}

impl RestApiCapability for ResourceGroup {
    fn register_rest(
        &self,
        _ctx: &GearCtx,
        router: axum::Router,
        openapi: &dyn OpenApiRegistry,
    ) -> anyhow::Result<axum::Router> {
        info!("Registering resource_group REST routes");

        let group_service = self
            .group_service
            .get()
            .ok_or_else(|| anyhow::anyhow!("GroupService not initialized"))?
            .clone();

        let membership_service = self
            .membership_service
            .get()
            .ok_or_else(|| anyhow::anyhow!("MembershipService not initialized"))?
            .clone();

        let router = routes::register_routes(router, openapi, group_service, membership_service);

        info!("Resource Group REST routes registered successfully");
        Ok(router)
    }
}

#[cfg(test)]
#[path = "gear_tests.rs"]
mod gear_tests;
