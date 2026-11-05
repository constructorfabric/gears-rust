//! `llm-gateway` gear entry point.

use std::sync::{Arc, OnceLock};
use std::time::Duration;

use anyhow::Context;
use async_trait::async_trait;
use authn_resolver_sdk::AuthNResolverClient;
use authz_resolver_sdk::{AuthZResolverApi, PolicyEnforcer};
use llm_gateway_sdk::LlmGatewayClientV1;
use model_registry_sdk::ModelRegistryClientV1;
use quota_enforcement_sdk::QuotaEnforcementClientV1;
use tokio_util::sync::CancellationToken;
use toolkit::api::OpenApiRegistry;
use toolkit::{Gear, GearCtx, RestApiCapability};
use usage_collector_sdk::UsageCollectorClientV1;

use crate::api::rest::routes;
use crate::config::LlmGatewayConfig;
use crate::domain::ports::{ServiceIdentity, UsageProducers, UsageSink};
use crate::domain::provider_rules::ProviderRulesRegistry;
use crate::domain::quota_guard::QuotaGuard;
use crate::domain::shutdown::ShutdownSignal;
use crate::domain::{KeylessCredentialBroker, LlmGatewayLocalClient, Service};
use crate::infra::media_fetcher::NoRouteMediaFetcher;
use crate::infra::model_registry::ModelRegistryResolver;
use crate::infra::plugin_directory::GtsPluginDirectory;
use crate::infra::service_identity::AuthNServiceIdentity;
use crate::infra::tool_schemas::TypesRegistryToolSchemas;
use crate::infra::usage_collector::UsageCollectorReporter;

/// LLM Gateway gear: wires the domain service, registers
/// `Arc<dyn LlmGatewayClientV1>` in `ClientHub`, and mounts the REST routes.
///
/// The `LlmGatewayProviderPluginSpecV1` GTS schema reaches `types-registry`
/// through the `toolkit-gts` link-time inventory, so no per-init
/// registration call is needed.
///
/// The gear is also stateful: its lifecycle entry ([`Self::serve`]) runs the
/// Usage Reporter worker until the runtime cancels it, then drains the usage
/// buffer within `shutdown.usage_drain_reserve`. After the cancel it first
/// waits (bounded) until the request pipeline is quiescent (no request in
/// flight, every settle finished) so the usage of requests that end during the
/// stop is still delivered.
/// The gear stop timeout, fed to the gear macro through `stop_timeout_fn`
/// (one source of truth): shutdown budgets (usage quiescence wait and drain
/// reserve) are derived from it.
pub(crate) const GEAR_STOP_TIMEOUT: Duration = Duration::from_secs(30);

#[toolkit::gear(
    name = "llm-gateway",
    deps = [types_registry, authz_resolver, authn_resolver, quota_enforcement, usage_collector],
    capabilities = [rest, stateful],
    lifecycle(entry = "serve", stop_timeout_fn = "stop_timeout")
)]
#[derive(Default)]
pub struct LlmGatewayModule {
    service: OnceLock<Arc<Service>>,
    reporter: OnceLock<Arc<UsageCollectorReporter>>,
    shutdown: OnceLock<ShutdownSignal>,
}

#[async_trait]
impl Gear for LlmGatewayModule {
    #[tracing::instrument(skip_all, fields(module = "llm-gateway"))]
    async fn init(&self, ctx: &GearCtx) -> anyhow::Result<()> {
        let cfg: LlmGatewayConfig = ctx.config_or_default()?;
        cfg.validate().map_err(|message| {
            anyhow::anyhow!("{} invalid configuration: {message}", Self::MODULE_NAME)
        })?;
        let hub = ctx.client_hub();

        // PEP boundary: fail init without an authz-resolver client rather
        // than ever serving without authorization.
        let authz = hub
            .get::<dyn AuthZResolverApi>()
            .with_context(|| format!("{} requires an authz-resolver client", Self::MODULE_NAME))?;
        let enforcer = PolicyEnforcer::new(authz);

        // Usage is never silently skipped: fail init without a Usage
        // Collector client.
        let usage_client = hub
            .get::<dyn UsageCollectorClientV1>()
            .with_context(|| format!("{} requires a usage-collector client", Self::MODULE_NAME))?;

        let registry = hub
            .get::<dyn ModelRegistryClientV1>()
            .with_context(|| format!("{} requires a model-registry client", Self::MODULE_NAME))?;

        // Quota is never bypassed: fail init without a Quota Enforcement
        // client, an AuthN client, or the service identity settle runs under.
        let quota_client = hub.get::<dyn QuotaEnforcementClientV1>().with_context(|| {
            format!("{} requires a quota-enforcement client", Self::MODULE_NAME)
        })?;
        let authn = hub
            .get::<dyn AuthNResolverClient>()
            .with_context(|| format!("{} requires an authn-resolver client", Self::MODULE_NAME))?;
        let credentials = cfg.client_credentials.as_ref().with_context(|| {
            format!(
                "{} requires [client_credentials] (service identity for quota settle)",
                Self::MODULE_NAME
            )
        })?;
        // One identity (and one cached context) for settle and usage delivery.
        let identity: Arc<dyn ServiceIdentity> =
            Arc::new(AuthNServiceIdentity::new(authn, credentials));
        let reporter = Arc::new(
            UsageCollectorReporter::new(
                usage_client,
                Arc::clone(&identity),
                &cfg.usage,
                &cfg.shutdown,
            )
            .map_err(|err| {
                anyhow::anyhow!("{} invalid usage configuration: {err}", Self::MODULE_NAME)
            })?
            .with_call_timeout(cfg.timeouts.dependency.usage_collector),
        );
        let quota = QuotaGuard::new(
            quota_client,
            identity,
            Arc::clone(&reporter) as Arc<dyn UsageSink>,
            &cfg.quota,
            &cfg.timeouts,
        );

        let shutdown = ShutdownSignal::new(cfg.shutdown.cut_after());
        let service = Arc::new(
            Service::new(
                enforcer,
                Arc::new(
                    ModelRegistryResolver::new(registry)
                        .with_timeout(cfg.timeouts.dependency.model_registry),
                ),
                Arc::new(GtsPluginDirectory::new(hub.clone())),
                Arc::new(KeylessCredentialBroker),
                Arc::new(
                    TypesRegistryToolSchemas::new(hub.clone())
                        .with_timeout(cfg.timeouts.dependency.types_registry),
                ),
                Arc::new(ProviderRulesRegistry::permissive()),
                quota,
                &cfg.limits,
                &cfg.stream,
                &cfg.timeouts,
            )
            .with_media(Arc::new(NoRouteMediaFetcher), &cfg.media)
            .with_shutdown(shutdown.clone()),
        );

        self.reporter
            .set(reporter)
            .map_err(|_| anyhow::anyhow!("{} module already initialized", Self::MODULE_NAME))?;
        self.shutdown
            .set(shutdown)
            .map_err(|_| anyhow::anyhow!("{} module already initialized", Self::MODULE_NAME))?;
        self.service
            .set(Arc::clone(&service))
            .map_err(|_| anyhow::anyhow!("{} module already initialized", Self::MODULE_NAME))?;

        let client: Arc<dyn LlmGatewayClientV1> = Arc::new(LlmGatewayLocalClient::new(service));
        hub.register::<dyn LlmGatewayClientV1>(client);
        Ok(())
    }
}

impl LlmGatewayModule {
    /// Stop timeout of the gear lifecycle ([`GEAR_STOP_TIMEOUT`]).
    #[allow(clippy::unused_self)] // the gear macro calls `stop_timeout_fn` as a method
    pub(crate) fn stop_timeout(&self) -> Duration {
        GEAR_STOP_TIMEOUT
    }

    /// Background lifecycle entry (wired via `lifecycle(entry = "serve")` on
    /// the gear macro): run the Usage Reporter worker until the runtime
    /// cancels `cancel`, then drain the buffer within the drain reserve.
    pub(crate) async fn serve(&self, cancel: CancellationToken) -> anyhow::Result<()> {
        let reporter =
            self.reporter.get().cloned().ok_or_else(|| {
                anyhow::anyhow!("{}: serve invoked before init", Self::MODULE_NAME)
            })?;
        tracing::info!(module = Self::MODULE_NAME, "usage reporter started");
        let producers =
            self.service.get().cloned().ok_or_else(|| {
                anyhow::anyhow!("{}: serve invoked before init", Self::MODULE_NAME)
            })? as Arc<dyn UsageProducers>;
        let shutdown =
            self.shutdown.get().cloned().ok_or_else(|| {
                anyhow::anyhow!("{}: serve invoked before init", Self::MODULE_NAME)
            })?;
        // Both start from the same stop signal: the Service stops accepting
        // and starts its cut timer, the reporter starts its quiescence wait.
        tokio::join!(
            shutdown.begin_on(&cancel),
            reporter.run(cancel.clone(), producers)
        );
        tracing::info!(module = Self::MODULE_NAME, "usage reporter stopped");
        Ok(())
    }
}

impl RestApiCapability for LlmGatewayModule {
    fn register_rest(
        &self,
        _ctx: &GearCtx,
        router: axum::Router,
        openapi: &dyn OpenApiRegistry,
    ) -> anyhow::Result<axum::Router> {
        let service = self
            .service
            .get()
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("{} Service not initialized", Self::MODULE_NAME))?;
        Ok(routes::register_routes(router, openapi, service))
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "module_tests.rs"]
mod module_tests;
