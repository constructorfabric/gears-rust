use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use axum::Router;
use toolkit::api::OpenApiRegistry;
use toolkit::{Gear, GearCtx, RestApiCapability};
use tracing::info;

use authz_resolver_sdk::{AuthZResolverClient, PolicyEnforcer};
use github_mirror_sdk::GithubMirrorClientV1;

use crate::api::rest::routes;
use crate::config::GithubMirrorConfig;
use crate::domain::local_client::LocalClient;
use crate::domain::service::{Service, ServiceConfig};
use crate::infra::storage::sea_orm_repo::{
    SeaOrmCommitRepository, SeaOrmIssueRepository, SeaOrmPullRequestRepository,
    SeaOrmRepoRepository,
};

type ConcreteService = Service<
    SeaOrmRepoRepository,
    SeaOrmIssueRepository,
    SeaOrmPullRequestRepository,
    SeaOrmCommitRepository,
>;

#[toolkit::gear(
    name = "github-mirror",
    deps = [authz_resolver],
    capabilities = [rest, db]
)]
pub struct GithubMirrorGear {
    service: OnceLock<Arc<ConcreteService>>,
}

impl Default for GithubMirrorGear {
    fn default() -> Self {
        Self {
            service: OnceLock::new(),
        }
    }
}

impl toolkit::contracts::DatabaseCapability for GithubMirrorGear {
    fn migrations(&self) -> Vec<Box<dyn sea_orm_migration::MigrationTrait>> {
        use sea_orm_migration::MigratorTrait;
        crate::infra::storage::migrations::Migrator::migrations()
    }
}

#[async_trait]
impl Gear for GithubMirrorGear {
    async fn init(&self, ctx: &GearCtx) -> anyhow::Result<()> {
        let cfg: GithubMirrorConfig = ctx.config_or_default()?;
        info!(api_base_url = %cfg.api_base_url, "Initializing github-mirror gear");

        let db = Arc::new(ctx.db_required()?);
        let repo = Arc::new(SeaOrmRepoRepository::new());
        let issues = Arc::new(SeaOrmIssueRepository::new());
        let pull_requests = Arc::new(SeaOrmPullRequestRepository::new());
        let commits = Arc::new(SeaOrmCommitRepository::new());

        let authz = ctx
            .client_hub()
            .get::<dyn AuthZResolverClient>()
            .map_err(|e| anyhow::anyhow!("failed to get AuthZ resolver: {e}"))?;
        let policy_enforcer = PolicyEnforcer::new(authz);

        let service = Arc::new(Service::new(
            db,
            repo,
            issues,
            pull_requests,
            commits,
            policy_enforcer,
            ServiceConfig {
                api_base_url: cfg.api_base_url,
            },
        ));

        self.service
            .set(service.clone())
            .map_err(|_| anyhow::anyhow!("{} gear already initialized", Self::MODULE_NAME))?;

        let client: Arc<dyn GithubMirrorClientV1> = Arc::new(LocalClient::new(service));
        ctx.client_hub()
            .register::<dyn GithubMirrorClientV1>(client);

        Ok(())
    }
}

impl RestApiCapability for GithubMirrorGear {
    fn register_rest(
        &self,
        _ctx: &GearCtx,
        router: Router,
        openapi: &dyn OpenApiRegistry,
    ) -> anyhow::Result<Router> {
        let service = self
            .service
            .get()
            .ok_or_else(|| anyhow::anyhow!("Service not initialized"))?
            .clone();

        let router = routes::register_routes(router, openapi, service);
        info!("github-mirror REST routes registered");
        Ok(router)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_gear_has_no_service_until_init() {
        let gear = GithubMirrorGear::default();
        assert!(gear.service.get().is_none());
    }

    #[test]
    fn gear_provides_all_migrations() {
        use toolkit::contracts::DatabaseCapability;
        let gear = GithubMirrorGear::default();
        assert_eq!(gear.migrations().len(), 4);
    }
}
