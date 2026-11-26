// Updated: 2026-10-06 by Constructor Tech
//! `ToolKit` gear declaration, dependency wiring, and managed lifecycle.
//!
//! The runtime publishes the GTS declarations of this gear and of its SDK (the
//! credential types, the permissions, the audit topic and event type) before
//! any application `init`, so `init` neither waits for the registry nor
//! registers anything in it. Initialization builds the domain service and
//! registers its SDK client and REST routes. The gear runs no background work
//! at all (ADR-0006): no outbox, no workers, no timers, no database polling, no work at startup.
//! Every store side effect is executed by the request that caused it, and the
//! leftovers of an interrupted request are healed by a later request that
//! touches the same record. The lifecycle entry only reports ready and waits
//! for cancellation.

use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use authz_resolver_sdk::PolicyEnforcer;
use tenant_resolver_sdk::TenantResolverClient;
use tokio_util::sync::CancellationToken;
use toolkit::api::OpenApiRegistry;
use toolkit::contracts::DatabaseCapability;
use toolkit::lifecycle::ReadySignal;
use toolkit::{Gear, GearCtx, RestApiCapability};
use toolkit_db::DBProvider;
use tracing::info;
use types_registry_sdk::PlatformTypesRegistryApi;

use crate::client::CredStoreLocalClient;
use crate::config::CredStoreConfig;
use crate::domain::ports::audit::AuditSink;
use crate::domain::ports::metrics::CredStoreMetricsPort;
use crate::domain::ports::plugin::PluginSelector;
use crate::domain::secret::service::{ListSettings, Service, WriteSettings};
use crate::infra::audit::{BrokerResolver, DEFAULT_PUBLISH_TIMEOUT, EventBrokerAuditSink};
use crate::infra::metrics::CredStoreMetricsMeter;
use crate::infra::plugin_select::GtsCredStorePluginSelector;
use crate::infra::storage::repo_impl::SecretRepoImpl;
use crate::infra::tenant_resolver::TenantResolverDir;
use crate::infra::types_registry::GtsSecretTypeResolver;

// `deps = [credstore-v2]` ensures consumers such as `oagw` find its client during `init`.
#[toolkit::consumes(contract = authz_resolver_sdk::AuthZResolverApi, from = "authz-resolver")]
#[toolkit::consumes(contract = types_registry_sdk::PlatformTypesRegistryApi, from = "types-registry")]
#[toolkit::gear(
    name = "credstore-v2",
    deps = [tenant_resolver],
    gts_publication(crates = [crate, credstore_sdk]),
    capabilities = [db, rest, stateful],
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

impl CredStoreGear {
    #[allow(
        clippy::redundant_pub_crate,
        reason = "module-private serve entry-point invoked by the toolkit runtime"
    )]
    /// Lifecycle entry point: requires `init` to have populated the service, signals readiness, then waits until `cancel` fires; it runs no background work.
    pub(crate) async fn serve(
        self: Arc<Self>,
        cancel: CancellationToken,
        ready: ReadySignal,
    ) -> anyhow::Result<()> {
        if self.service.get().is_none() {
            anyhow::bail!("credstore: serve invoked before init");
        }

        ready.notify();
        info!(
            target: "credstore.lifecycle",
            "credstore gear serving; no background work: store cleanup runs inside requests"
        );

        cancel.cancelled().await;

        info!(target: "credstore.lifecycle", "credstore lifecycle cancelled");
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

        // No PDP capabilities: credstore projects no authorization tables
        // (no `tenant_closure`), so the PDP hands it pre-expanded, flat
        // tenant predicates (AUTHZ_USAGE_SCENARIOS S09–S11). The resolver is
        // consumed, wired after `init` (locally or remotely), so the enforcer
        // fetches it from the hub per call.
        let enforcer = PolicyEnforcer::from_hub(ctx.client_hub());
        info!("PolicyEnforcer wired to the consumed authz-resolver");

        let tr_client = ctx
            .client_hub()
            .get::<dyn TenantResolverClient>()
            .map_err(|e| anyhow::anyhow!("failed to get TenantResolverClient: {e}"))?;

        let metrics: Arc<dyn CredStoreMetricsPort> = Arc::new(CredStoreMetricsMeter::from_global());

        let dir = Arc::new(TenantResolverDir::new(tr_client, Arc::clone(&metrics)));

        let plugins = Arc::new(GtsCredStorePluginSelector::new(
            ctx.client_hub(),
            cfg.vendor.clone(),
        ));

        // Fail-closed: without the platform registry API no secret type can
        // be resolved, so credstore must not come up. The registry is
        // consumed, not linked: its local client where it runs in this
        // process, its resolving REST client otherwise.
        let registry = ctx
            .client_hub()
            .get::<dyn PlatformTypesRegistryApi>()
            .map_err(|e| anyhow::anyhow!("failed to get PlatformTypesRegistryApi: {e}"))?;
        let types = Arc::new(GtsSecretTypeResolver::new(registry, Arc::clone(&metrics)));
        info!("types-registry client resolved from client hub; secret-type resolver wired");

        // Audit (`cpt-cf-credstore-nfr-audit`): the event broker is a
        // non-blocking dependency. It is deliberately NOT in `deps` and its
        // client is resolved per event, so credstore starts and works with
        // the broker absent; every dropped event is counted instead. The
        // audit topic and event type are published with the other
        // declarations before `init`.
        let hub = ctx.client_hub();
        let resolve: BrokerResolver =
            Arc::new(move || hub.try_get::<dyn event_broker_sdk::EventBrokerApi>());
        let audit_sink: Arc<dyn AuditSink> = Arc::new(EventBrokerAuditSink::new(
            resolve,
            Arc::clone(&metrics),
            DEFAULT_PUBLISH_TIMEOUT,
        ));

        let svc = Arc::new(
            Service::new(
                repo,
                dir,
                enforcer,
                Arc::clone(&plugins) as Arc<dyn PluginSelector>,
                types,
                Arc::clone(&metrics),
                ListSettings {
                    max_limit: cfg.list.max_limit,
                },
            )
            .with_audit(audit_sink)
            .with_write_settings(WriteSettings {
                intent_lease: std::time::Duration::from_secs(cfg.write.intent_lease_secs),
            }),
        );

        self.service
            .set(Arc::clone(&svc))
            .map_err(|_| anyhow::anyhow!("{} module already initialized", Self::MODULE_NAME))?;

        let client: Arc<dyn credstore_sdk::CredStoreClientV2> =
            Arc::new(CredStoreLocalClient::new(svc));
        ctx.client_hub()
            .register::<dyn credstore_sdk::CredStoreClientV2>(client);

        info!("credstore module initialized");
        Ok(())
    }
}

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
#[path = "gear_tests.rs"]
mod gear_tests;
