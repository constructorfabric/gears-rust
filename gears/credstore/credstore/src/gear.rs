use std::sync::{Arc, OnceLock};
use std::time::Duration;

use async_trait::async_trait;
use authz_resolver_sdk::{AuthZResolverClient, PolicyEnforcer, models::Capability};
use tenant_resolver_sdk::TenantResolverClient;
use tokio_util::sync::CancellationToken;
use toolkit::api::OpenApiRegistry;
use toolkit::contracts::{DatabaseCapability, SystemCapability};
use toolkit::lifecycle::ReadySignal;
use toolkit::{Gear, GearCtx, RestApiCapability};
use toolkit_db::DBProvider;
use tracing::info;

use types_registry_sdk::TypesRegistryClient;

use crate::client::CredStoreLocalClient;
use crate::config::{CredStoreConfig, HierarchyCfg};
use crate::domain::ports::metrics::CredStoreMetricsPort;
use crate::domain::secret::service::{ReaperSettings, Service};
use crate::infra::metrics::CredStoreMetricsMeter;
use crate::infra::plugin_select::GtsCredStorePluginSelector;
use crate::infra::storage::repo_impl::SecretRepoImpl;
use crate::infra::tenant_resolver::TenantResolverDir;
use crate::infra::types_registry::GtsSecretTypeResolver;

// `system` capability is required in this platform: consumers like `oagw` are
// system modules and resolve `CredStoreClientV1` from the ClientHub during their
// `init`. System modules initialize before non-system ones
// (`modules_by_system_priority`), so credstore must also be a system module to
// register its client before those consumers init.
#[toolkit::gear(
    name = "credstore",
    deps = [authz_resolver, tenant_resolver, types_registry],
    capabilities = [system, db, rest, stateful],
    lifecycle(entry = "serve", stop_timeout = "30s", await_ready)
)]
pub struct CredStoreGear {
    service: OnceLock<Arc<Service>>,
}

impl Default for CredStoreGear {
    fn default() -> Self {
        Self {
            service: OnceLock::new(),
        }
    }
}

/// PDP capabilities to advertise, given the hierarchy config. When the shared
/// tenant-closure is co-located in this module's database we advertise
/// `TenantHierarchy` (structured subtree predicate, resolved by a closure
/// subquery); otherwise we omit it and the PDP pre-expands the subtree into a
/// flat membership list this module enforces without any local closure.
fn pep_capabilities(hierarchy: &HierarchyCfg) -> Vec<Capability> {
    if hierarchy.tenant_closure_colocated {
        vec![Capability::TenantHierarchy]
    } else {
        Vec::new()
    }
}

impl CredStoreGear {
    #[allow(
        clippy::redundant_pub_crate,
        reason = "module-private serve entry-point invoked by the toolkit runtime"
    )]
    pub(crate) async fn serve(
        self: Arc<Self>,
        cancel: CancellationToken,
        ready: ReadySignal,
    ) -> anyhow::Result<()> {
        let Some(svc) = self.service.get().cloned() else {
            anyhow::bail!("credstore: serve invoked before init");
        };

        let tick = Duration::from_secs(svc.reaper_tick_secs());
        let mut interval = tokio::time::interval(tick);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

        ready.notify();
        info!(
            target: "credstore.lifecycle",
            reaper_tick_secs = tick.as_secs(),
            "credstore reaper tick started"
        );

        loop {
            tokio::select! {
                biased;
                () = cancel.cancelled() => break,
                _ = interval.tick() => {
                    svc.reap_and_refresh().await;
                }
            }
        }

        info!(target: "credstore.lifecycle", "credstore reaper tick cancelled");
        Ok(())
    }
}

#[async_trait]
impl Gear for CredStoreGear {
    #[tracing::instrument(skip_all, fields(module = "credstore"))]
    async fn init(&self, ctx: &GearCtx) -> anyhow::Result<()> {
        let cfg: CredStoreConfig = ctx.config_or_default()?;
        cfg.validate()
            .map_err(|err| anyhow::anyhow!("credstore config invalid: {err}"))?;
        info!(vendor = %cfg.vendor, "initializing credstore module");

        let db_raw = ctx.db_required()?;
        let db: Arc<DBProvider<crate::domain::error::DomainError>> =
            Arc::new(DBProvider::new(db_raw.db()));

        let repo = Arc::new(SecretRepoImpl::new(Arc::clone(&db)));

        let authz_client = ctx
            .client_hub()
            .get::<dyn AuthZResolverClient>()
            .map_err(|e| anyhow::anyhow!("failed to get AuthZResolverClient: {e}"))?;
        let enforcer =
            PolicyEnforcer::new(authz_client).with_capabilities(pep_capabilities(&cfg.hierarchy));
        info!("authz-resolver client resolved from client hub; PolicyEnforcer wired");

        let tr_client = ctx
            .client_hub()
            .get::<dyn TenantResolverClient>()
            .map_err(|e| anyhow::anyhow!("failed to get TenantResolverClient: {e}"))?;

        let metrics: Arc<dyn CredStoreMetricsPort> = Arc::new(CredStoreMetricsMeter::from_global());

        let dir = Arc::new(TenantResolverDir::new(
            tr_client,
            Arc::clone(&metrics),
            cfg.hierarchy.ancestor_cache_ttl_secs,
        ));

        let plugins = Arc::new(GtsCredStorePluginSelector::new(
            ctx.client_hub(),
            cfg.vendor.clone(),
        ));

        // Fail-closed: without the types-registry client no secret type can
        // be resolved, so credstore must not come up (types-registry is a
        // hard `deps` and initializes first).
        let registry = ctx
            .client_hub()
            .get::<dyn TypesRegistryClient>()
            .map_err(|e| anyhow::anyhow!("failed to get TypesRegistryClient: {e}"))?;
        let types = Arc::new(GtsSecretTypeResolver::new(registry, Arc::clone(&metrics)));
        info!("types-registry client resolved from client hub; secret-type resolver wired");

        let svc = Arc::new(Service::new(
            repo,
            dir,
            enforcer,
            plugins,
            types,
            metrics,
            ReaperSettings {
                tick_secs: cfg.reaper.tick_secs,
                provisioning_timeout_secs: cfg.reaper.provisioning_timeout_secs,
                deprovisioning_timeout_secs: cfg.reaper.deprovisioning_timeout_secs,
            },
        ));

        self.service
            .set(Arc::clone(&svc))
            .map_err(|_| anyhow::anyhow!("{} module already initialized", Self::MODULE_NAME))?;

        let client: Arc<dyn credstore_sdk::CredStoreClientV1> =
            Arc::new(CredStoreLocalClient::new(svc));
        ctx.client_hub()
            .register::<dyn credstore_sdk::CredStoreClientV1>(client);

        info!("credstore module initialized");
        Ok(())
    }
}

// Empty system capability: credstore needs no pre_init/post_init work, only the
// system-priority init ordering (see the module attribute above).
impl SystemCapability for CredStoreGear {}

impl DatabaseCapability for CredStoreGear {
    fn migrations(&self) -> Vec<Box<dyn sea_orm_migration::MigrationTrait>> {
        use sea_orm_migration::MigratorTrait;
        info!("providing credstore database migrations");
        crate::infra::storage::migrations::Migrator::migrations()
    }
}

impl RestApiCapability for CredStoreGear {
    fn register_rest(
        &self,
        _ctx: &GearCtx,
        router: axum::Router,
        openapi: &dyn OpenApiRegistry,
    ) -> anyhow::Result<axum::Router> {
        info!("registering credstore REST routes");
        let svc = self
            .service
            .get()
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("credstore Service not initialized"))?;
        let router = crate::api::rest::register_routes(router, openapi, svc);
        info!("credstore REST routes registered");
        Ok(router)
    }
}

#[cfg(test)]
mod tests {
    use super::{Capability, pep_capabilities};
    use crate::config::HierarchyCfg;

    #[test]
    fn advertises_tenant_hierarchy_when_closure_colocated() {
        let cfg = HierarchyCfg {
            tenant_closure_colocated: true,
            ..Default::default()
        };
        assert_eq!(pep_capabilities(&cfg), vec![Capability::TenantHierarchy]);
    }

    #[test]
    fn advertises_no_capabilities_when_closure_not_colocated() {
        let cfg = HierarchyCfg {
            tenant_closure_colocated: false,
            ..Default::default()
        };
        assert!(pep_capabilities(&cfg).is_empty());
    }
}
