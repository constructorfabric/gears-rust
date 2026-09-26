use std::sync::{Arc, OnceLock};
use std::time::Duration;

use async_trait::async_trait;
use axum::Router;
use toolkit::api::OpenApiRegistry;
use toolkit::client_hub::ClientHubError;
use toolkit::{Gear, GearCtx};
use toolkit_db::DBProvider;
use toolkit_db::DbError;
use tracing::{info, warn};

use authz_resolver_sdk::{AuthZResolverApi, PolicyEnforcer};

use simple_user_settings_sdk::{SettingsOwnerResolver, SimpleUserSettingsClientV1};

use crate::api::rest::routes;
use crate::config::SettingsConfig;
use crate::domain::local_client::LocalClient;
use crate::domain::service::{Service, ServiceConfig};
use crate::infra::storage::sea_orm_repo::SeaOrmSettingsRepository;

/// Type alias for the concrete service type with ORM repository.
type ConcreteService = Service<SeaOrmSettingsRepository>;

/// Say at startup which key settings are filed under.
///
/// Only a report: the service reads the hub on every request, so a resolver
/// registered after this point still takes effect. The line is there so an
/// operator can confirm the intended wiring without a request.
fn report_owner_key(ctx: &GearCtx) {
    match owner_key(ctx) {
        Ok(key) => info!("Settings gear: settings are keyed by {key}"),
        Err(e) => warn!(
            error = %e,
            "Settings gear: SettingsOwnerResolver lookup failed; settings requests will fail until it is fixed"
        ),
    }
}

/// What the user half of the settings key is, as the hub stands now.
fn owner_key(ctx: &GearCtx) -> Result<&'static str, ClientHubError> {
    match ctx.client_hub().get::<dyn SettingsOwnerResolver>() {
        Ok(_) => Ok("the deployment's owner resolver"),
        Err(ClientHubError::NotFound { .. }) => {
            Ok("the token subject (no SettingsOwnerResolver registered)")
        }
        Err(e) => Err(e),
    }
}

#[toolkit::gear(
    name = "simple-user-settings",
    deps = [authz_resolver],
    capabilities = [rest, db]
)]
pub struct SettingsGear {
    service: OnceLock<Arc<ConcreteService>>,
}

impl Default for SettingsGear {
    fn default() -> Self {
        Self {
            service: OnceLock::new(),
        }
    }
}

impl toolkit::contracts::DatabaseCapability for SettingsGear {
    fn migrations(&self) -> Vec<Box<dyn sea_orm_migration::MigrationTrait>> {
        use sea_orm_migration::MigratorTrait;
        info!("Providing settings database migrations");
        crate::infra::storage::migrations::Migrator::migrations()
    }
}

#[async_trait]
impl Gear for SettingsGear {
    async fn init(&self, ctx: &GearCtx) -> anyhow::Result<()> {
        let cfg: SettingsConfig = ctx.config_or_default()?;
        cfg.validate()?;

        let db: Arc<DBProvider<DbError>> = Arc::new(ctx.db_required()?);

        // Repository no longer stores connection - uses &impl DBRunner per-method
        let repo = Arc::new(SeaOrmSettingsRepository::new());

        // Fetch AuthZ resolver from ClientHub
        let authz = ctx
            .client_hub()
            .get::<dyn AuthZResolverApi>()
            .map_err(|e| anyhow::anyhow!("failed to get AuthZ resolver: {e}"))?;
        let policy_enforcer = PolicyEnforcer::new(authz);

        let service_config = ServiceConfig {
            max_field_length: cfg.max_field_length,
            owner_resolver_timeout: Duration::from_millis(cfg.owner_resolver_timeout_ms),
        };
        let service = Arc::new(Service::new(
            db,
            repo,
            policy_enforcer,
            service_config,
            ctx.client_hub(),
        ));
        self.service
            .set(service.clone())
            .map_err(|_| anyhow::anyhow!("{} gear already initialized", Self::MODULE_NAME))?;

        let local_client: Arc<dyn SimpleUserSettingsClientV1> = Arc::new(LocalClient::new(service));
        ctx.client_hub().register(local_client);

        Ok(())
    }
}

#[async_trait]
impl toolkit::contracts::RestApiCapability for SettingsGear {
    fn register_rest(
        &self,
        ctx: &GearCtx,
        router: Router,
        openapi: &dyn OpenApiRegistry,
    ) -> anyhow::Result<Router> {
        info!("Settings gear: register_rest called");
        let service = self
            .service
            .get()
            .ok_or_else(|| anyhow::anyhow!("Service not initialized"))?
            .clone();

        report_owner_key(ctx);

        let router = routes::register_routes(router, openapi, service);
        info!("Settings gear: REST routes registered successfully");
        Ok(router)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_settings_gear_default() {
        let gear = SettingsGear::default();
        assert!(gear.service.get().is_none());
    }

    #[test]
    fn test_settings_gear_multiple_defaults_empty_service() {
        let gear = SettingsGear::default();
        let other = SettingsGear::default();
        assert!(other.service.get().is_none());
        assert!(gear.service.get().is_none());
    }

    /// Hands `init` one gear section and nothing else.
    struct Section(serde_json::Value);

    impl toolkit::ConfigProvider for Section {
        fn get_gear_config(&self, gear_name: &str) -> Option<&serde_json::Value> {
            (gear_name == "simple-user-settings").then_some(&self.0)
        }
    }

    /// The startup wiring, not just the validator: `init` must refuse an
    /// out-of-range resolver timeout before it touches the database or the hub.
    /// The context has neither, so reaching past validation would fail with a
    /// different error.
    #[tokio::test]
    async fn init_refuses_an_out_of_range_resolver_timeout() {
        for bad in [0, crate::config::MAX_OWNER_RESOLVER_TIMEOUT_MS + 1] {
            let ctx = GearCtx::new(
                "simple-user-settings",
                uuid::Uuid::new_v4(),
                Arc::new(Section(serde_json::json!({
                    "config": { "owner_resolver_timeout_ms": bad }
                }))),
                Arc::new(toolkit::ClientHub::new()),
                tokio_util::sync::CancellationToken::new(),
            );

            let err = SettingsGear::default()
                .init(&ctx)
                .await
                .expect_err("init must refuse the config");
            assert!(
                err.to_string().contains("owner_resolver_timeout_ms"),
                "{bad}: refused for the wrong reason: {err}"
            );
        }
    }
}
