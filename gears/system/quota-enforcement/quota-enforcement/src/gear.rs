//! Gear declaration of quota-enforcement.
//!
//! `init` wires the PEP boundary, the domain service, the in-process manager
//! client, the quota resolver plugin, and the cluster coordination binding.
//! The plugin instance is published from the configuration before `init`,
//! which binds its scoped client. The lifecycle entry runs the
//! fail-closed bootstrap before the ready signal, which ends by starting
//! notification delivery, then hosts the leader-only sweepers under child
//! tokens, and stops delivery on shutdown. The REST surface mounts into
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
use quota_enforcement_sdk::{QuotaEnforcementApi, QuotaManagerApi, QuotaOperatorApi};
use quota_resolver_sdk::{QuotaResolverPluginClient, QuotaResolverPluginSpecV1};
use tokio_util::sync::CancellationToken;
use toolkit::api::OpenApiRegistry;
use toolkit::client_hub::ClientHub;
use toolkit::context::GearCtx;
use toolkit::gts::PluginV1;
use toolkit::gts_publication::GtsPublicationInputs;
use toolkit::lifecycle::ReadySignal;
use toolkit::plugins::bind_plugin_instances;
use toolkit::{Gear, Healthcheck, RestApiCapability};
use toolkit_contract::policy::PolicyStack;
use tracing::{info, warn};
use types_registry_sdk::{GtsId, PlatformTypesRegistryApi};

use crate::api::healthcheck::ReadinessCheck;
use crate::api::in_process::{
    InProcessQuotaEnforcement, InProcessQuotaManager, InProcessQuotaOperator,
};
use crate::api::resolver_plugin::ResolverPlugin;
use crate::api::rest::routes;
use crate::config::QuotaEnforcementConfig;
use crate::domain::ports::{LifecycleGaugeSink, QeMetrics};
use crate::domain::{
    Admission, Bootstrap, BootstrapReporting, Bound, GaugeTiming, LifecycleGaugeRefresher,
    MetricCatalog, PluginBinding, Readiness, Service, SingletonScope,
};
use crate::infra::charge_backlog::ChargeBacklogCell;
use crate::infra::cluster_coordination::{ClusterCoordinationBinding, ElectionTiming};
use crate::infra::lease_backlog::LeaseBacklogCell;
use crate::infra::lifecycle_gauges::LifecycleGaugeCell;
use crate::infra::metrics;
use crate::infra::pdp_probe::PdpReachability;
use crate::infra::types_registry::PlatformTypeRegistry;

const LOG_TARGET: &str = "qe.lifecycle";
const BOOTSTRAP_RETRY_MIN: Duration = Duration::from_secs(1);
const BOOTSTRAP_RETRY_MAX: Duration = Duration::from_secs(30);

/// GTS segment of QE's quota resolver plugin instance.
pub const RESOLVER_PLUGIN_SEGMENT: &str = "cf.core.quota_enforcement.plugin.v1";

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
    lifecycle(entry = "serve", stop_timeout = "30s", await_ready),
    gts_publication(entities = Self::resolver_plugin_instance),
)]
#[toolkit::provides(
    contract = quota_enforcement_sdk::QuotaManagerApi,
    local = Self::build_manager,
    transports = [local, rest],
)]
#[toolkit::provides(
    contract = quota_enforcement_sdk::QuotaEnforcementApi,
    local = Self::build_enforcement,
    transports = [local, rest],
)]
#[toolkit::provides(
    contract = quota_enforcement_sdk::QuotaOperatorApi,
    local = Self::build_operator,
    transports = [local, rest],
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
    /// The sample cell of the held-allocation-charge gauges.
    charge_backlog: Arc<ChargeBacklogCell>,
    /// Timing of the lease sweeper.
    lease_sweep: crate::domain::operations::LeaseSweepTiming,
    /// Interval of the scheduled catalogue read.
    catalog_read_interval: Duration,
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
        let delivery = bootstrap.delivery();
        let bound = bootstrap_or_stop(bootstrap, &cancel, gauges.stop_timeout).await?;
        let refresher = LifecycleGaugeRefresher::new(
            bound.storage.clone(),
            Arc::clone(&bound.catalog),
            gauges.cell.clone() as Arc<dyn LifecycleGaugeSink>,
            gauges.timing,
        );
        let sweeper = Arc::new(crate::domain::operations::RetentionSweeper::new(
            bound.storage.clone(),
            Arc::clone(&gauges.metrics),
            Arc::clone(&bound.catalog),
            gauges.charge_backlog.clone() as Arc<dyn crate::domain::ports::ChargeBacklogSink>,
            gauges.retention,
        ));
        let lease_sweeper = Arc::new(crate::domain::operations::LeaseSweeper::new(
            bound.storage.clone(),
            Arc::clone(&bound.catalog),
            gauges.lease_backlog.clone() as Arc<dyn crate::domain::ports::LeaseBacklogSink>,
            gauges.lease_sweep,
        ));
        let coordinator = bound.coordinator.clone();
        let catalog = Arc::clone(&bound.catalog);
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
        let interval = gauges.catalog_read_interval;
        let catalog_shutdown = cancel.child_token();
        let catalog_task = tokio::spawn(async move {
            catalog
                .run_scheduled_reads(interval, catalog_shutdown)
                .await;
            Ok(())
        });
        cancel.cancelled().await;
        info!(target: LOG_TARGET, "quota-enforcement is stopping");
        stop_background(
            [
                ("lifecycle gauge", gauge_task),
                ("retention sweeper", retention_task),
                ("lease sweeper", lease_task),
                ("catalogue read", catalog_task),
            ],
            &delivery,
            gauges.stop_timeout,
        )
        .await;
        Ok(())
    }
}

impl QuotaEnforcementGear {
    /// The publication's config-only document: the quota resolver plugin
    /// instance, admitted before `init` binds its client.
    fn resolver_plugin_instance(
        inputs: &GtsPublicationInputs<'_>,
    ) -> anyhow::Result<Vec<(GtsId, serde_json::Value)>> {
        let cfg: QuotaEnforcementConfig = inputs.config_or_default()?;
        cfg.resolver_plugin.validate()?;
        let (id, document) = PluginV1::<QuotaResolverPluginSpecV1>::build_registration(
            RESOLVER_PLUGIN_SEGMENT,
            cfg.resolver_plugin.vendor,
            cfg.resolver_plugin.priority,
        )?;
        Ok(vec![(GtsId::try_new(id.as_ref())?, document)])
    }

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
/// of the lifecycle entry, so the ready signal is never sent. A dependency
/// that does not answer is waited for: nothing restarts a gear whose entry
/// returned an error, so giving up would leave the replica not ready for good.
async fn bootstrap_or_shutdown(
    bootstrap: &Bootstrap,
    cancel: &CancellationToken,
) -> anyhow::Result<Bound> {
    const SHUTDOWN: &str = "quota-enforcement: shutdown requested during bootstrap";
    // ponytail: no jitter; add it when the replica count makes retries in step a load on a dependency.
    let mut pause = BOOTSTRAP_RETRY_MIN;
    loop {
        let outage = tokio::select! {
            biased;
            () = cancel.cancelled() => anyhow::bail!(SHUTDOWN),
            outcome = bootstrap.run() => match outcome {
                Ok(bound) => return Ok(bound),
                Err(error) if error.is_dependency_outage() => error,
                Err(error) => return Err(error).context("quota-enforcement bootstrap failed"),
            },
        };
        warn!(
            target: LOG_TARGET,
            error = %outage,
            retry_in_ms = u64::try_from(pause.as_millis()).unwrap_or(u64::MAX),
            "quota-enforcement bootstrap waits for a dependency"
        );
        tokio::select! {
            biased;
            () = cancel.cancelled() => anyhow::bail!(SHUTDOWN),
            () = tokio::time::sleep(pause) => {}
        }
        pause = (pause * 2).min(BOOTSTRAP_RETRY_MAX);
    }
}

/// [`bootstrap_or_shutdown`], stopping notification delivery when it fails: a
/// shutdown can interrupt bootstrap after delivery started.
async fn bootstrap_or_stop(
    bootstrap: &Bootstrap,
    cancel: &CancellationToken,
    stop_timeout: Duration,
) -> anyhow::Result<Bound> {
    let outcome = bootstrap_or_shutdown(bootstrap, cancel).await;
    if outcome.is_err() {
        stop_delivery(&bootstrap.delivery(), stop_timeout).await;
    }
    outcome
}

/// A background task: for a leader-only one, its election's outcome once it
/// stops.
type LeaderTask = tokio::task::JoinHandle<Result<(), crate::domain::DomainError>>;

/// Leader-only: the elected replica refreshes and publishes the gauge sample;
/// every other replica publishes nothing. Leadership loss cancels the child
/// token and the refresher withdraws its sample.
fn spawn_leader_task(
    coordinator: Arc<dyn crate::domain::SingletonCoordinator>,
    scope: SingletonScope,
    work: crate::domain::ports::coordination::LeaderWork,
    shutdown: CancellationToken,
) -> LeaderTask {
    tokio::spawn(async move { coordinator.run_while_leader(scope, shutdown, work).await })
}

/// Wait for a leader task to stop within `budget`, aborting it after that; a
/// slow or failed stop is logged, never an error of the lifecycle entry.
async fn join_leader_task(name: &str, mut task: LeaderTask, budget: Duration) {
    let failure = match tokio::time::timeout(budget, &mut task).await {
        Ok(Ok(Ok(()))) => return,
        Ok(Ok(Err(err))) => format!("{name} election ended with an error: {err}"),
        Ok(Err(join)) => format!("{name} task did not finish cleanly: {join}"),
        Err(_elapsed) => {
            task.abort();
            format!("{name} task did not stop within {budget:?} and was aborted")
        }
    };
    tracing::warn!(target: LOG_TARGET, "{failure}");
}

/// Stop the background tasks and notification delivery together, within
/// `budget`.
async fn stop_background(
    tasks: [(&str, LeaderTask); 4],
    delivery: &crate::domain::notifications::DeliveryLifecycle,
    budget: Duration,
) {
    let leaders = futures::future::join_all(
        tasks
            .into_iter()
            .map(|(name, task)| join_leader_task(name, task, budget)),
    );
    tokio::join!(leaders, stop_delivery(delivery, budget));
}

/// Stop notification delivery within `budget`; events it did not finish stay
/// queued for the next processor. A slow stop is logged, never an error of
/// the lifecycle entry.
async fn stop_delivery(
    delivery: &crate::domain::notifications::DeliveryLifecycle,
    budget: Duration,
) {
    if tokio::time::timeout(budget, delivery.stop()).await.is_err() {
        tracing::warn!(
            target: LOG_TARGET,
            "notification delivery did not stop within {budget:?}"
        );
    }
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
        // The SDK default is a hang guard, far above the admission latency budget.
        let enforcer = PolicyEnforcer::new(authz).with_deadline(cfg.pdp_deadline());

        let registry: Arc<dyn PlatformTypesRegistryApi> =
            hub.get::<dyn PlatformTypesRegistryApi>().with_context(|| {
                format!(
                    "{} requires the types-registry platform client",
                    Self::MODULE_NAME
                )
            })?;

        let gauge_cell = Arc::new(LifecycleGaugeCell::default());
        let lease_backlog = Arc::new(LeaseBacklogCell::default());
        let charge_backlog = Arc::new(ChargeBacklogCell::default());
        let metrics: Arc<dyn QeMetrics> = metrics::build_default_adapter(
            &cfg.metrics,
            gauge_cell.clone(),
            lease_backlog.clone(),
            charge_backlog.clone(),
        );
        let readiness = Arc::new(Readiness::new());
        let catalog = Arc::new(MetricCatalog::new(
            Arc::new(PlatformTypeRegistry::new(
                registry,
                cfg.catalog.read_timeout(),
            )),
            Arc::clone(&metrics),
            cfg.catalog.unknown_memory(),
        ));
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
                snapshot: cfg.snapshot.to_limits(),
            },
        ));
        // The local builders find the service here; the key is crate-private.
        hub.register::<LocalService>(Arc::new(LocalService(service.clone())));
        let resolver_plugin: Arc<dyn QuotaResolverPluginClient> = Arc::new(ResolverPlugin::new(
            service.clone(),
            cfg.snapshot.to_limits().page_size,
        ));
        bind_plugin_instances::<QuotaResolverPluginSpecV1, _>(
            &hub,
            ctx.gts_publication_manifest()?.documents(),
            &resolver_plugin,
        )?;
        self.wire_quota_manager_api(ctx).await?;
        self.wire_quota_enforcement_api(ctx).await?;
        self.wire_quota_operator_api(ctx).await?;

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
            BootstrapReporting {
                metrics: Arc::clone(&metrics),
                readiness,
            },
            policy_limits,
        )
        .with_dispatch(cfg.notifications.to_limits());

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
            charge_backlog,
            lease_sweep: cfg
                .leases
                .to_sweep_timing()
                .context("[quota-enforcement.leases] is not a valid sweeper timing")?,
            catalog_read_interval: cfg.catalog.read_interval(),
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

/// The shared service, keyed by a type no other crate can name.
pub(crate) struct LocalService(Arc<Service>);

impl LocalService {
    fn of(ctx: &GearCtx) -> anyhow::Result<Arc<Service>> {
        let local = ctx
            .client_hub()
            .get::<Self>()
            .context("quota-enforcement: the local service is registered before wiring")?;
        Ok(local.0.clone())
    }
}

impl QuotaEnforcementGear {
    fn build_manager(
        ctx: &GearCtx,
        _policies: Arc<PolicyStack>,
    ) -> anyhow::Result<Arc<dyn QuotaManagerApi>> {
        Ok(Arc::new(InProcessQuotaManager::new(LocalService::of(ctx)?)))
    }

    fn build_enforcement(
        ctx: &GearCtx,
        _policies: Arc<PolicyStack>,
    ) -> anyhow::Result<Arc<dyn QuotaEnforcementApi>> {
        Ok(Arc::new(InProcessQuotaEnforcement::new(LocalService::of(
            ctx,
        )?)))
    }

    fn build_operator(
        ctx: &GearCtx,
        _policies: Arc<PolicyStack>,
    ) -> anyhow::Result<Arc<dyn QuotaOperatorApi>> {
        Ok(Arc::new(InProcessQuotaOperator::new(LocalService::of(
            ctx,
        )?)))
    }
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
        let router = routes::register_routes(router, openapi, service.clone());
        // Served from the local service, never the hub: a REST-wired hub client would forward.
        let router = quota_enforcement_sdk::rest::register_quota_manager_api_rest_routes(
            router,
            openapi,
            Arc::new(InProcessQuotaManager::new(service.clone())) as Arc<dyn QuotaManagerApi>,
        );
        let router = quota_enforcement_sdk::rest::register_quota_enforcement_api_rest_routes(
            router,
            openapi,
            Arc::new(InProcessQuotaEnforcement::new(service.clone()))
                as Arc<dyn QuotaEnforcementApi>,
        );
        Ok(
            quota_enforcement_sdk::rest::register_quota_operator_api_rest_routes(
                router,
                openapi,
                Arc::new(InProcessQuotaOperator::new(service)) as Arc<dyn QuotaOperatorApi>,
            ),
        )
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
