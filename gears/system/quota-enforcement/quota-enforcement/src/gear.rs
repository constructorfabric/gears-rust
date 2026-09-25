//! Gear declaration of quota-enforcement.
//!
//! `init` wires the PEP boundary, the domain service, the in-process manager
//! client, and the cluster coordination binding. The lifecycle entry runs the
//! fail-closed bootstrap before the ready signal, then hosts the leader-only
//! lifecycle-gauge refresh under a child token. The REST surface mounts into
//! the platform `api-gateway`; the readiness check reports the bootstrap state
//! and the cluster requirements verdict.
//!
//! The gear declares no `deps = [cluster]` edge (cluster DESIGN section
//! 3.17.7): a deployed consumer links no cluster gear. Start ordering comes
//! from the cluster gear's `system` tier, readiness gating from the
//! SDK-submitted consumer registration.

use std::sync::{Arc, OnceLock};
use std::time::Duration;

use anyhow::Context as _;
use async_trait::async_trait;
use authz_resolver_sdk::{AuthZResolverApi, PolicyEnforcer};
use quota_enforcement_sdk::QuotaManagerClientV1;
use tokio_util::sync::CancellationToken;
use toolkit::api::OpenApiRegistry;
use toolkit::client_hub::ClientHub;
use toolkit::context::GearCtx;
use toolkit::lifecycle::ReadySignal;
use toolkit::{Gear, Healthcheck, RestApiCapability};
use tracing::info;
use types_registry_sdk::TypesRegistryClient;

use crate::api::healthcheck::ReadinessCheck;
use crate::api::in_process::InProcessQuotaManager;
use crate::api::rest::routes;
use crate::config::QuotaEnforcementConfig;
use crate::domain::ports::{LifecycleGaugeSink, MetricRegistry, QeMetrics};
use crate::domain::{
    Admission, Bootstrap, BootstrapReporting, Bound, CatalogBinding, GaugeTiming,
    LifecycleGaugeRefresher, PluginBinding, Readiness, Service, SingletonScope,
};
use crate::infra::cluster_coordination::{ClusterCoordinationBinding, ElectionTiming};
use crate::infra::lease_backlog::LeaseBacklogCell;
use crate::infra::lifecycle_gauges::LifecycleGaugeCell;
use crate::infra::metric_registry::CachedMetricRegistry;
use crate::infra::metrics;
use crate::infra::pdp_probe::PdpReachability;
use crate::infra::types_registry::TypesRegistryContracts;

const LOG_TARGET: &str = "qe.lifecycle";

/// Quota Enforcement gear.
///
/// The gear owns no database: persistence lives behind the storage plugin.
/// It is stateful because the bootstrap runs in the lifecycle entry and later
/// features host their sweepers there under child cancellation tokens.
// @cpt-dod:cpt-cf-quota-enforcement-dod-workspace-crates:p1
#[toolkit::gear(
    name = "quota-enforcement",
    deps = [authz_resolver, types_registry],
    capabilities = [rest, stateful],
    lifecycle(entry = "serve", stop_timeout = "30s", await_ready)
)]
pub struct QuotaEnforcementGear {
    service: OnceLock<Arc<Service>>,
    bootstrap: OnceLock<Bootstrap>,
    hub: OnceLock<Arc<ClientHub>>,
    gauges: OnceLock<GaugeWiring>,
}

/// What the lifecycle entry needs to host its leader-only tasks.
struct GaugeWiring {
    cell: Arc<LifecycleGaugeCell>,
    timing: GaugeTiming,
    stop_timeout: Duration,
    /// Telemetry sink of the retention sweeper.
    metrics: Arc<dyn QeMetrics>,
    /// Timing of the retention sweeper.
    retention: crate::domain::operations::RetentionTiming,
    /// The sample cell of `lease_unreclaimed_expired`.
    lease_backlog: Arc<LeaseBacklogCell>,
    /// Timing of the lease sweeper.
    lease_sweep: crate::domain::operations::LeaseSweepTiming,
}

impl Default for QuotaEnforcementGear {
    fn default() -> Self {
        Self {
            service: OnceLock::new(),
            bootstrap: OnceLock::new(),
            hub: OnceLock::new(),
            gauges: OnceLock::new(),
        }
    }
}

impl QuotaEnforcementGear {
    /// The domain service, once `init` ran.
    #[must_use]
    pub fn service(&self) -> Option<Arc<Service>> {
        self.service.get().cloned()
    }

    /// The lifecycle gauge sample cell, once `init` ran. Holds a sample only
    /// while this replica leads and its last refresh succeeded.
    #[must_use]
    pub fn lifecycle_gauges(&self) -> Option<Arc<LifecycleGaugeCell>> {
        self.gauges.get().map(|wiring| wiring.cell.clone())
    }

    /// The lease backlog sample cell, once `init` ran. Holds a sample only
    /// while this replica leads the lease sweeper.
    #[must_use]
    pub fn lease_backlog(&self) -> Option<Arc<LeaseBacklogCell>> {
        self.gauges.get().map(|wiring| wiring.lease_backlog.clone())
    }

    /// Lifecycle entry: bootstrap, signal ready, host the leader-only gauge
    /// refresh, then stop on shutdown.
    ///
    /// Bootstrap resolves the cluster leader election here, in `start`, after
    /// the cluster gear started. The gauge refresh runs under a child token of
    /// `cancel` after the ready signal; later features spawn their sweepers the
    /// same way.
    ///
    /// # Errors
    ///
    /// Returns an error when bootstrap fails or shutdown interrupts it. The
    /// ready signal is never sent in that case, so the gear is never marked
    /// running.
    pub(crate) async fn serve(
        self: Arc<Self>,
        cancel: CancellationToken,
        ready: ReadySignal,
    ) -> anyhow::Result<()> {
        let (service, bootstrap, gauges) = self.initialised()?;
        let bound = bootstrap_or_shutdown(bootstrap, &cancel).await?;
        let refresher = LifecycleGaugeRefresher::new(
            bound.storage.clone(),
            bound.metric_registry.clone(),
            gauges.cell.clone() as Arc<dyn LifecycleGaugeSink>,
            gauges.timing,
        );
        let sweeper = Arc::new(crate::domain::operations::RetentionSweeper::new(
            bound.storage.clone(),
            Arc::clone(&gauges.metrics),
            gauges.retention,
        ));
        let lease_sweeper = Arc::new(crate::domain::operations::LeaseSweeper::new(
            bound.storage.clone(),
            Arc::clone(&bound.classifications),
            gauges.lease_backlog.clone() as Arc<dyn crate::domain::ports::LeaseBacklogSink>,
            gauges.lease_sweep,
        ));
        let coordinator = bound.coordinator.clone();
        service
            .bind(bound)
            .context("quota-enforcement: publish bootstrapped dependencies")?;

        ready.notify();
        info!(target: LOG_TARGET, "quota-enforcement is ready");

        let gauge_task = spawn_leader_task(
            coordinator.clone(),
            SingletonScope::LifecycleGauges,
            refresher.leader_work(),
            cancel.child_token(),
        );
        // @cpt-begin:cpt-cf-quota-enforcement-algo-retention-sweep:p1:inst-ret-elect
        let retention_task = spawn_leader_task(
            coordinator.clone(),
            SingletonScope::RetentionSweeper,
            sweeper.leader_work(),
            cancel.child_token(),
        );
        // @cpt-end:cpt-cf-quota-enforcement-algo-retention-sweep:p1:inst-ret-elect
        // @cpt-begin:cpt-cf-quota-enforcement-algo-lease-sweep:p1:inst-swp-elect
        let lease_task = spawn_leader_task(
            coordinator,
            SingletonScope::LeaseSweeper,
            lease_sweeper.leader_work(),
            cancel.child_token(),
        );
        // @cpt-end:cpt-cf-quota-enforcement-algo-lease-sweep:p1:inst-swp-elect
        cancel.cancelled().await;
        info!(target: LOG_TARGET, "quota-enforcement is stopping");
        join_leader_task("lifecycle gauge", gauge_task, gauges.stop_timeout).await;
        join_leader_task("retention sweeper", retention_task, gauges.stop_timeout).await;
        join_leader_task("lease sweeper", lease_task, gauges.stop_timeout).await;
        Ok(())
    }
}

impl QuotaEnforcementGear {
    /// The init-time cells, all filled or none.
    fn initialised(&self) -> anyhow::Result<(Arc<Service>, &Bootstrap, &GaugeWiring)> {
        const BEFORE_INIT: &str = "quota-enforcement: serve invoked before init";
        let service = self.service.get().cloned().context(BEFORE_INIT)?;
        let bootstrap = self.bootstrap.get().context(BEFORE_INIT)?;
        let gauges = self.gauges.get().context(BEFORE_INIT)?;
        Ok((service, bootstrap, gauges))
    }
}

/// Bootstrap raced against shutdown: a shutdown during bootstrap is an error
/// of the lifecycle entry, so the ready signal is never sent.
async fn bootstrap_or_shutdown(
    bootstrap: &Bootstrap,
    cancel: &CancellationToken,
) -> anyhow::Result<Bound> {
    tokio::select! {
        biased;
        () = cancel.cancelled() => {
            anyhow::bail!("quota-enforcement: shutdown requested during bootstrap");
        }
        outcome = bootstrap.run() => outcome.context("quota-enforcement bootstrap failed"),
    }
}

/// Leader-only: the elected replica refreshes and publishes the gauge sample;
/// every other replica publishes nothing. Leadership loss cancels the child
/// token and the refresher withdraws its sample.
fn spawn_leader_task(
    coordinator: Arc<dyn crate::domain::SingletonCoordinator>,
    scope: SingletonScope,
    work: crate::domain::ports::coordination::LeaderWork,
    shutdown: CancellationToken,
) -> tokio::task::JoinHandle<Result<(), crate::domain::DomainError>> {
    tokio::spawn(async move { coordinator.run_while_leader(scope, shutdown, work).await })
}

/// Wait for a leader task to stop within `budget`; a slow or failed stop is
/// logged, never an error of the lifecycle entry.
async fn join_leader_task(
    name: &str,
    task: tokio::task::JoinHandle<Result<(), crate::domain::DomainError>>,
    budget: Duration,
) {
    let failure = match tokio::time::timeout(budget, task).await {
        Ok(Ok(Ok(()))) => return,
        Ok(Ok(Err(err))) => format!("{name} election ended with an error: {err}"),
        Ok(Err(join)) => format!("{name} task did not finish cleanly: {join}"),
        Err(_elapsed) => format!("{name} task did not stop within {budget:?}"),
    };
    tracing::warn!(target: LOG_TARGET, "{failure}");
}

#[async_trait]
impl Gear for QuotaEnforcementGear {
    #[tracing::instrument(skip_all, fields(storage_vendor))]
    async fn init(&self, ctx: &GearCtx) -> anyhow::Result<()> {
        if self.service.get().is_some() {
            anyhow::bail!("{} gear already initialized", Self::MODULE_NAME);
        }
        let cfg: QuotaEnforcementConfig = ctx.config_or_default()?;
        cfg.validate()?;
        tracing::Span::current().record("storage_vendor", cfg.storage_vendor.as_str());

        // The PDP client is mandatory; bootstrap separately probes reachability.
        let hub = ctx.client_hub();
        let authz: Arc<dyn AuthZResolverApi> = hub
            .get::<dyn AuthZResolverApi>()
            .with_context(|| format!("{} requires an authz-resolver client", Self::MODULE_NAME))?;
        let pdp_probe = Arc::new(PdpReachability::new(authz.clone()));
        let enforcer = PolicyEnforcer::new(authz);

        // Bootstrap builds the projection catalogue from this registry client.
        let registry: Arc<dyn TypesRegistryClient> = hub
            .get::<dyn TypesRegistryClient>()
            .with_context(|| format!("{} requires a types-registry client", Self::MODULE_NAME))?;
        let catalog = CatalogBinding {
            registry: Arc::new(TypesRegistryContracts::new(registry.clone())),
            config: cfg
                .catalog
                .to_domain()
                .context("[quota-enforcement.catalog] is not a valid catalogue")?,
        };
        let metric_registry: Arc<dyn MetricRegistry> = Arc::new(CachedMetricRegistry::new(
            registry,
            cfg.quotas.metric_cache_entries,
            cfg.quotas.metric_cache_ttl(),
            cfg.quotas.metric_cache_stale_grace(),
        ));

        let gauge_cell = Arc::new(LifecycleGaugeCell::default());
        let lease_backlog = Arc::new(LeaseBacklogCell::default());
        let metrics: Arc<dyn QeMetrics> =
            metrics::build_default_adapter(&cfg.metrics, gauge_cell.clone(), lease_backlog.clone());
        let readiness = Arc::new(Readiness::new());
        let admission = Admission::new(enforcer, metrics.clone());
        let policy_limits = cfg
            .policies
            .to_limits()
            .context("[quota-enforcement.policies] is not a valid policy bound set")?;
        let service = Arc::new(Service::new(
            admission,
            readiness.clone(),
            cfg.quotas.to_limits(),
            policy_limits,
            crate::domain::service::OperationsRuntime {
                cache_entries: cfg.operations.idempotency_cache_entries,
                cache_ttl: cfg.operations.idempotency_cache_ttl(),
                preparation_max_attempts: policy_limits.preparation_max_attempts,
                leases: cfg.leases.to_limits(),
                batch: cfg.operations.to_batch_limits(),
            },
        ));
        // In-process clients share the REST domain boundary.
        hub.register::<dyn quota_enforcement_sdk::QuotaOperatorClientV1>(Arc::new(
            crate::api::in_process::InProcessQuotaOperator::new(service.clone()),
        ));
        hub.register::<dyn quota_enforcement_sdk::QuotaEnforcementClientV1>(Arc::new(
            crate::api::in_process::InProcessQuotaEnforcement::new(service.clone()),
        ));
        hub.register::<dyn QuotaManagerClientV1>(Arc::new(InProcessQuotaManager::new(
            service.clone(),
        )));

        let timing = ElectionTiming::new(
            cfg.election.ttl(),
            cfg.election.max_missed_renewals,
            cfg.sweeper_stop_timeout(),
        )
        .context("[quota-enforcement.election] is not a valid election timing")?;
        let coordinator = Arc::new(ClusterCoordinationBinding::new(hub.clone(), timing));
        let binding = PluginBinding::new(hub.clone(), cfg.storage_vendor.clone());
        let bootstrap = Bootstrap::new(
            binding,
            coordinator,
            pdp_probe,
            catalog,
            metric_registry,
            BootstrapReporting {
                metrics: Arc::clone(&metrics),
                readiness,
            },
            policy_limits,
        );

        let gauges = GaugeWiring {
            cell: gauge_cell,
            timing: cfg.gauges.to_timing(),
            stop_timeout: cfg.sweeper_stop_timeout(),
            metrics: Arc::clone(&metrics),
            retention: cfg
                .retention
                .to_timing()
                .context("[quota-enforcement.retention] is not a valid sweeper timing")?,
            lease_backlog,
            lease_sweep: cfg
                .leases
                .to_sweep_timing()
                .context("[quota-enforcement.leases] is not a valid sweeper timing")?,
        };
        set_once(&self.gauges, gauges)?;
        set_once(&self.hub, hub)?;
        set_once(&self.bootstrap, bootstrap)?;
        set_once(&self.service, service)?;

        info!(target: LOG_TARGET, "quota-enforcement initialised; bootstrap runs in the lifecycle entry");
        Ok(())
    }
}

/// Fill one of the gear's init-time cells exactly once.
fn set_once<T>(cell: &OnceLock<T>, value: T) -> anyhow::Result<()> {
    cell.set(value).map_err(|_| {
        anyhow::anyhow!(
            "{} gear already initialized",
            QuotaEnforcementGear::MODULE_NAME
        )
    })
}

impl RestApiCapability for QuotaEnforcementGear {
    // @cpt-flow:cpt-cf-quota-enforcement-flow-gear-bootstrap:p1
    fn register_rest(
        &self,
        _ctx: &GearCtx,
        router: axum::Router,
        openapi: &dyn OpenApiRegistry,
    ) -> anyhow::Result<axum::Router> {
        // @cpt-begin:cpt-cf-quota-enforcement-flow-gear-bootstrap:p1:inst-boot-rest
        let service = self
            .service
            .get()
            .cloned()
            .context("quota-enforcement: register_rest invoked before init")?;
        Ok(routes::register_routes(router, openapi, service))
        // @cpt-end:cpt-cf-quota-enforcement-flow-gear-bootstrap:p1:inst-boot-rest
    }

    fn healthcheck(&self, _ctx: &GearCtx) -> Option<Arc<dyn Healthcheck>> {
        let service = self.service.get()?;
        // The cluster contributor reports deferred profile or wiring failures.
        let cluster = self
            .hub
            .get()
            .map(|hub| cluster_sdk::cluster_readiness(hub.clone()));
        Some(Arc::new(ReadinessCheck::new(
            service.readiness().clone(),
            cluster,
        )))
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "gear_tests.rs"]
mod gear_tests;
