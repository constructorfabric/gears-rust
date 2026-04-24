// Created: 2026-04-16 by Constructor Tech
// @cpt-dod:cpt-cf-resource-group-dod-e2e-test-suite:p1
use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use authz_resolver_sdk::{AuthZResolverClient, PolicyEnforcer};
use modkit::{DatabaseCapability, Module, ModuleCtx};
use modkit_db::DBProvider;
use modkit_db::DbError;
use resource_group_sdk::{ResourceGroupClient, ResourceGroupReadHierarchy};
use sea_orm_migration::MigrationTrait;
use tracing::info;

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
    deps = ["authz-resolver", "types-registry"],
    capabilities = [db]
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

        // Resolve TypesRegistryClient for GTS metadata validation
        let types_registry: Arc<dyn types_registry_sdk::TypesRegistryClient> = ctx
            .client_hub()
            .get::<dyn types_registry_sdk::TypesRegistryClient>()
            .map_err(|e| anyhow::anyhow!("failed to get TypesRegistryClient: {e}"))?;

        // Create GroupService with default query profile and PolicyEnforcer
        let profile = QueryProfile::default();
        let group_service = Arc::new(GroupService::new(
            db.clone(),
            profile,
            enforcer.clone(),
            group_repo.clone(),
            type_repo.clone(),
            types_registry,
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

// REST capability and route registration are added in a follow-up PR.
