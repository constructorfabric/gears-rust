// @cpt-dod:cpt-cf-resource-group-dod-e2e-test-suite:p1
use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use authz_resolver_sdk::{AuthZResolverClient, PolicyEnforcer};
use modkit::api::OpenApiRegistry;
use modkit::{DatabaseCapability, Module, ModuleCtx, RestApiCapability};
use modkit_db::DBProvider;
use modkit_db::DbError;
use resource_group_sdk::{ResourceGroupClient, ResourceGroupReadHierarchy};
use sea_orm_migration::MigrationTrait;
use tracing::info;

use crate::api::rest::routes;
use crate::domain::group_service::{GroupService, QueryProfile};
use crate::domain::membership_service::MembershipService;
use crate::domain::read_service::RgReadService;
use crate::domain::rg_service::RgService;
use crate::domain::type_service::TypeService;
use crate::infra::storage::group_repo::GroupRepository;
use crate::infra::storage::membership_repo::MembershipRepository;
use crate::infra::storage::type_repo::TypeRepository;

pub type ConcreteTypeService = TypeService<TypeRepository>;
pub type ConcreteGroupService = GroupService<GroupRepository, TypeRepository>;
pub type ConcreteMembershipService =
    MembershipService<GroupRepository, TypeRepository, MembershipRepository>;
pub type ConcreteRgService = RgService<GroupRepository, TypeRepository, MembershipRepository>;

// @cpt-dod:cpt-cf-resource-group-dod-sdk-foundation-module-scaffold:p1
/// Main module struct for the resource-group module.
#[modkit::module(
    name = "resource-group",
    deps = [],
    capabilities = [db, rest]
)]
#[allow(clippy::struct_field_names)]
pub struct ResourceGroup {
    type_service: OnceLock<Arc<ConcreteTypeService>>,
    group_service: OnceLock<Arc<ConcreteGroupService>>,
    membership_service: OnceLock<Arc<ConcreteMembershipService>>,
}

impl Default for ResourceGroup {
    fn default() -> Self {
        Self {
            type_service: OnceLock::new(),
            group_service: OnceLock::new(),
            membership_service: OnceLock::new(),
        }
    }
}

#[async_trait]
impl Module for ResourceGroup {
    async fn init(&self, ctx: &ModuleCtx) -> anyhow::Result<()> {
        // Acquire DB capability (secure wrapper)
        let db: Arc<DBProvider<DbError>> = Arc::new(ctx.db_required()?);

        // Resolve AuthZ client from ClientHub and create PolicyEnforcer
        let authz = ctx
            .client_hub()
            .get::<dyn AuthZResolverClient>()
            .map_err(|e| anyhow::anyhow!("failed to get AuthZ resolver: {e}"))?;
        let enforcer = PolicyEnforcer::new(authz);

        // Create repo instances
        let group_repo = Arc::new(GroupRepository);
        let type_repo = Arc::new(TypeRepository);
        let membership_repo = Arc::new(MembershipRepository);

        // Create TypeService
        let type_service = Arc::new(TypeService::new(db.clone(), type_repo.clone()));

        self.type_service
            .set(type_service)
            .map_err(|_| anyhow::anyhow!("{} module already initialized", Self::MODULE_NAME))?;

        // Create GroupService with default query profile and PolicyEnforcer
        let profile = QueryProfile::default();
        let group_service = Arc::new(GroupService::new(
            db.clone(),
            profile,
            enforcer.clone(),
            group_repo.clone(),
            type_repo.clone(),
        ));

        self.group_service
            .set(group_service)
            .map_err(|_| anyhow::anyhow!("{} module already initialized", Self::MODULE_NAME))?;

        // Create MembershipService with PolicyEnforcer for AuthZ enforcement
        let membership_service = Arc::new(MembershipService::new(
            db,
            enforcer,
            group_repo,
            type_repo,
            membership_repo,
        ));
        self.membership_service
            .set(membership_service.clone())
            .map_err(|_| anyhow::anyhow!("{} module already initialized", Self::MODULE_NAME))?;

        // Phase 1 (SystemCapability): register SDK clients in ClientHub
        let type_svc = self
            .type_service
            .get()
            .ok_or_else(|| anyhow::anyhow!("{} type_service not initialized", Self::MODULE_NAME))?
            .clone();
        let group_svc = self
            .group_service
            .get()
            .ok_or_else(|| anyhow::anyhow!("{} group_service not initialized", Self::MODULE_NAME))?
            .clone();

        let rg_client: Arc<dyn ResourceGroupClient> = Arc::new(RgService::new(
            type_svc,
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
            "Resource Group module initialized (ClientHub: ResourceGroupClient + ResourceGroupReadHierarchy)"
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
        _ctx: &ModuleCtx,
        router: axum::Router,
        openapi: &dyn OpenApiRegistry,
    ) -> anyhow::Result<axum::Router> {
        info!("Registering resource_group REST routes");

        let type_service = self
            .type_service
            .get()
            .ok_or_else(|| anyhow::anyhow!("TypeService not initialized"))?
            .clone();

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

        let router = routes::register_routes(
            router,
            openapi,
            type_service,
            group_service,
            membership_service,
        );

        info!("Resource Group REST routes registered successfully");
        Ok(router)
    }
}
