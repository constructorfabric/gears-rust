//! Initialization, dependency checks, readiness and cooperative shutdown.
use crate::config::OrdersConfig;
use crate::infra::broker::ProducerReadiness;
use crate::infra::events::EventSink;
use anyhow::Context;
use arc_swap::{ArcSwap, ArcSwapOption};
use async_trait::async_trait;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;
use toolkit::contracts::{DatabaseCapability, RestApiCapability};
use toolkit::{Gear, GearCtx};
use toolkit::{Healthcheck, HealthcheckResult};

/// Retained real dependencies for subsequent service/engine implementation.
struct OrdersRuntime {
    db: toolkit_db::Db,
    /// The one shared PEP for REST, SDK, engine pre-guard and read wrapper (08 §3.5).
    pep: crate::authz::Pep,
    /// Independently configured worker capability; unaffected by PDP outages.
    maintenance: Option<Arc<crate::infra::maintenance::MaintenanceAuthority>>,
    /// The attested restricted class connections of the workers (S2-11), opened at init.
    workers: Option<Arc<crate::infra::workers::WorkerConnections>>,
    /// Validated worker cadences and batch bounds.
    worker_settings: crate::infra::workers::WorkerSettings,
    /// Worker observability on the process meter.
    worker_metrics: Arc<dyn crate::infra::workers::WorkerMetrics>,
    /// Configured identities the audit actor class derives from (D-115); grants nothing.
    identities: crate::domain::audit::ActorIdentities,
    /// Keyed administrative-text minimizer for audit values (D-204).
    admin_text_key: Arc<crate::domain::audit::AdminTextKey>,
    /// The frozen date-basis preparer (S2-10) for Preview, submit and amendment; none of those
    /// operations is delivered yet.
    _dates: crate::infra::dates::DatePreparer,
    /// The explicit positive finite registry lease (Foundation §3.6).
    lease: crate::domain::idempotency::LeaseDuration,
    /// State table, guard registry and field classes compiled and validated at init (§3.4).
    registries: crate::infra::engine::Registries,
    /// Event Broker and the tenant resolver publish their clients after Orders' init (the broker
    /// in its REST phase), so the producer is bound from `serve` through this hub.
    hub: Arc<toolkit::ClientHub>,
    producer: crate::infra::broker::ProducerSettings,
    dependency_timeout: Duration,
    _config: OrdersConfig,
}

/// Upper bound of the producer-binding retry backoff.
const BIND_BACKOFF_MAX: Duration = Duration::from_secs(30);

/// Feature-gated scaffold: no business traffic can be admitted yet.
#[toolkit::gear(name = "bss-orders-lifecycle", deps = [authz_resolver, types_registry, tenant_resolver, event_broker], capabilities = [db, rest, stateful], lifecycle(entry = "serve", stop_timeout = "30s"))]
pub struct BssOrdersLifecycleGear {
    runtime: ArcSwapOption<OrdersRuntime>,
    running: Arc<AtomicBool>,
    /// Managed producer readiness (D-200); never `Ready` without a bound worker handle.
    producer: Arc<ArcSwap<ProducerReadiness>>,
    /// The bound transaction sink the S2-04 engine enqueues through; `None` while not ready.
    events: Arc<ArcSwapOption<EventSink>>,
    /// The transition engine (S2-04), built over the bound producer; `None` while not ready.
    engine: Arc<ArcSwapOption<crate::infra::engine::Engine>>,
    /// Draft authoring (S2-09), shared by REST and the local SDK; set at init, unavailable until
    /// the engine is bound.
    capture: ArcSwapOption<crate::infra::capture::CaptureService>,
    /// Authorized draft reads with access logging (early S6-01/S6-04), shared by REST and the
    /// local SDK; set at init, unavailable until `serve` binds its parts alongside the engine.
    reads: ArcSwapOption<crate::infra::read::ReadService>,
    /// The read dependencies bound by `serve`; `None` while not ready (no read before readiness).
    read_parts: Arc<ArcSwapOption<crate::infra::read::ReadParts>>,
    /// Last pass result per Orders-owned worker (S2-11).
    worker_status: Arc<crate::infra::workers::WorkerStatus>,
    /// The workers `serve` scheduled; `None` before `serve` and after shutdown.
    scheduled_workers: ArcSwapOption<Vec<crate::infra::workers::WorkerKind>>,
    /// The gear-local per-(caller, order) edge limiter (S2-12, D-185); built at init from the
    /// validated settings, mounted with the authoring routes.
    limiter: ArcSwapOption<crate::api::rest::throttle::PerOrderLimiter>,
    /// The store the readiness probe reads (DESIGN §3.8: store unavailability makes the instance
    /// not ready, never restarted); `None` before init and after shutdown.
    store: Arc<ArcSwapOption<toolkit_db::Db>>,
}
impl Default for BssOrdersLifecycleGear {
    fn default() -> Self {
        Self {
            runtime: ArcSwapOption::from(None),
            running: Arc::new(AtomicBool::new(false)),
            producer: Arc::new(ArcSwap::from_pointee(ProducerReadiness::Starting)),
            events: Arc::new(ArcSwapOption::from(None)),
            engine: Arc::new(ArcSwapOption::from(None)),
            capture: ArcSwapOption::from(None),
            reads: ArcSwapOption::from(None),
            read_parts: Arc::new(ArcSwapOption::from(None)),
            worker_status: Arc::new(crate::infra::workers::WorkerStatus::default()),
            scheduled_workers: ArcSwapOption::from(None),
            limiter: ArcSwapOption::from(None),
            store: Arc::new(ArcSwapOption::from(None)),
        }
    }
}
impl BssOrdersLifecycleGear {
    pub(crate) async fn serve(
        self: Arc<Self>,
        cancel: tokio_util::sync::CancellationToken,
    ) -> anyhow::Result<()> {
        let runtime = self
            .runtime
            .load_full()
            .ok_or_else(|| anyhow::anyhow!("bss-orders-lifecycle: not initialized"))?;
        self.running.store(true, Ordering::Release);
        // The five Orders-owned workers start with the lifecycle: cleanup, purge and audit need
        // no producer; the expiry/auto-void slots report their undelivered bodies. Each pass
        // takes its advisory key on the host connection and stops with this token.
        let workers = self.start_workers(&runtime, &cancel);
        if let Some(bound) = Box::pin(self.bind_until_ready(&runtime, &cancel)).await {
            self.events.store(Some(Arc::new(bound.sink().clone())));
            self.engine
                .store(Some(Arc::new(crate::infra::engine::Engine::new(
                    crate::infra::engine::EngineParts {
                        db: runtime.db.clone(),
                        pep: runtime.pep.clone(),
                        sink: bound.sink().clone(),
                        identities: runtime.identities.clone(),
                        admin_key: Arc::clone(&runtime.admin_text_key),
                        lease: runtime.lease,
                    },
                    runtime.registries.clone(),
                ))));
            self.read_parts
                .store(Some(Arc::new(crate::infra::read::ReadParts {
                    db: runtime.db.clone(),
                    pep: runtime.pep.clone(),
                    identities: runtime.identities.clone(),
                })));
            self.producer.store(Arc::new(ProducerReadiness::Ready));
            tracing::info!(
                queue = crate::infra::events::QUEUE,
                "bss-orders-lifecycle: event producer bound"
            );
            cancel.cancelled().await;
            self.read_parts.store(None);
            self.engine.store(None);
            self.events.store(None);
            self.producer.store(Arc::new(ProducerReadiness::Stopped));
            // Library-managed shutdown; committed queue rows stay for the next owner.
            bound.stop().await;
        }
        if let Some(workers) = workers {
            workers.stop().await;
        }
        self.scheduled_workers.store(None);
        self.producer.store(Arc::new(ProducerReadiness::Stopped));
        self.running.store(false, Ordering::Release);
        self.store.store(None);
        self.runtime.store(None);
        Ok(())
    }

    /// Bind the managed producer, retrying with bounded backoff until bound or cancelled.
    /// A failed attempt is visible as readiness and a warning, never a restart loop.
    async fn bind_until_ready(
        &self,
        runtime: &OrdersRuntime,
        cancel: &tokio_util::sync::CancellationToken,
    ) -> Option<crate::infra::broker::BoundProducer> {
        let mut backoff = Duration::from_millis(250);
        loop {
            let attempt = Box::pin(crate::infra::broker::bind(
                runtime
                    .hub
                    .get::<dyn event_broker_sdk::EventBrokerApi>()
                    .ok(),
                runtime
                    .hub
                    .get::<dyn tenant_resolver_sdk::TenantResolverClient>()
                    .ok(),
                runtime.db.clone(),
                &runtime.producer,
                runtime.dependency_timeout,
                None,
            ));
            tokio::select! {
                () = cancel.cancelled() => return None,
                result = attempt => match result {
                    Ok(bound) => return Some(bound),
                    Err(error) => {
                        tracing::warn!(
                            failure = error.failure.code(),
                            error = %error,
                            "bss-orders-lifecycle: event producer not ready"
                        );
                        self.producer
                            .store(Arc::new(ProducerReadiness::Unavailable(error.failure)));
                    }
                },
            }
            tokio::select! {
                () = cancel.cancelled() => return None,
                () = tokio::time::sleep(backoff) => {}
            }
            backoff = (backoff * 2).min(BIND_BACKOFF_MAX);
        }
    }

    /// Start the Orders-owned workers under the configured authority and class connections.
    /// Without a maintenance block nothing is scheduled: every task fails closed.
    fn start_workers(
        &self,
        runtime: &OrdersRuntime,
        cancel: &tokio_util::sync::CancellationToken,
    ) -> Option<crate::infra::workers::WorkerHandles> {
        use crate::infra::workers::{self, WorkerParts, expiry::SweepUnavailable};
        let (Some(authority), Some(connections)) = (&runtime.maintenance, &runtime.workers) else {
            tracing::info!(
                "bss-orders-lifecycle: no maintenance authority configured; no worker scheduled"
            );
            return None;
        };
        let handles = workers::start(
            WorkerParts {
                host: runtime.db.clone(),
                connections: Arc::clone(connections),
                authority: Arc::clone(authority),
                settings: runtime.worker_settings,
                metrics: Arc::clone(&runtime.worker_metrics),
                // Stage 5 supplies the expiry/auto-void bodies; S3/S5 the recovery bodies.
                expiry: Arc::new(SweepUnavailable),
                auto_void: Arc::new(SweepUnavailable),
                recovery: Arc::new(workers::RecoveryUnavailable),
                capture_hooks: workers::audit::CaptureHooks::default(),
                status: Arc::clone(&self.worker_status),
            },
            cancel,
        );
        self.scheduled_workers
            .store(Some(Arc::new(handles.scheduled().to_vec())));
        Some(handles)
    }

    /// Last pass result per worker (readiness detail and tests).
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn worker_status(&self) -> &crate::infra::workers::WorkerStatus {
        &self.worker_status
    }

    /// The workers scheduled by `serve`.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn scheduled_workers(&self) -> Vec<crate::infra::workers::WorkerKind> {
        self.scheduled_workers
            .load_full()
            .map(|v| v.as_ref().clone())
            .unwrap_or_default()
    }

    /// Current managed-producer readiness (S2-12 readiness and tests).
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn producer_readiness(&self) -> ProducerReadiness {
        **self.producer.load()
    }

    /// The bound event sink, once the producer is ready (consumed by the S2-04 engine).
    #[allow(dead_code)]
    pub(crate) fn event_sink(&self) -> Option<Arc<EventSink>> {
        self.events.load_full()
    }

    /// The transition engine, once the producer is bound. Business operations (S2-09 onward)
    /// enter it; none is mounted yet.
    #[allow(dead_code)]
    pub(crate) fn engine(&self) -> Option<Arc<crate::infra::engine::Engine>> {
        self.engine.load_full()
    }
}
#[async_trait]
impl Gear for BssOrdersLifecycleGear {
    async fn init(&self, ctx: &GearCtx) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.runtime.load().is_none(),
            "bss-orders-lifecycle: already initialized"
        );
        let config: OrdersConfig = ctx
            .config_expanded()
            .context("bss-orders-lifecycle: explicit config required")?;
        config.validate()?;
        let resolver = ctx
            .client_hub()
            .get::<dyn authz_resolver_sdk::AuthZResolverApi>()
            .context("bss-orders-lifecycle: AuthZResolverApi required")?;
        let registry = ctx
            .client_hub()
            .get::<dyn types_registry_sdk::TypesRegistryClient>()
            .context("bss-orders-lifecycle: TypesRegistryClient required")?;
        let db = ctx
            .db_required()
            .context("bss-orders-lifecycle: database required")?
            .db();
        anyhow::ensure!(
            db.backend() == sea_orm::DbBackend::Postgres,
            "bss-orders-lifecycle: PostgreSQL is required; other backends are unproven"
        );
        // DESIGN 02 §3.7/§3.8: the policy channel promotes the configured date-policy rows with
        // the deployment, and startup refuses a missing or invalid platform default.
        crate::infra::dates::install(&db, config.date_policy_plan()?.as_ref()).await?;
        crate::gts::validate_catalog()?;
        crate::authz::validate_census()?;
        // S2-12: only delivered, caller-facing operations are mounted; a delivered route bound to
        // a service-only or break-glass action refuses startup.
        crate::api::rest::validate_delivered()?;
        // Foundation §3.4 step 1: duplicate expanded keys, unknown guard rows/reasons, unowned
        // reasons and unclassified authored fields refuse startup.
        let registries = crate::infra::engine::Registries::compile()?;
        tokio::time::timeout(
            std::time::Duration::from_millis(u64::from(config.dependency_timeout_ms)),
            crate::gts::register(registry.as_ref()),
        )
        .await
        .context("bss-orders-lifecycle: registry initialization timed out")??;
        let enforcer = authz_resolver_sdk::PolicyEnforcer::new(resolver).with_deadline(
            std::time::Duration::from_millis(u64::from(config.dependency_timeout_ms)),
        );
        let producer = config.producer_settings()?;
        // S2-11: open and attest the workers' restricted class connections before anything is
        // scheduled. A missing, unresolved, misprovisioned or over-privileged connection refuses
        // startup; there is no fallback to the host connection.
        let dependency_timeout = Duration::from_millis(u64::from(config.dependency_timeout_ms));
        let workers = if config.maintenance.is_some() {
            let dsns = config.maintenance_connections()?;
            let opts = toolkit_db::ConnectOpts {
                max_conns: Some(4),
                min_conns: Some(0),
                acquire_timeout: Some(dependency_timeout),
                ..toolkit_db::ConnectOpts::default()
            };
            let connections = tokio::time::timeout(
                dependency_timeout.saturating_mul(2),
                crate::infra::workers::WorkerConnections::connect(&dsns, opts),
            )
            .await
            .context("bss-orders-lifecycle: maintenance connections timed out")?
            .map_err(|e| anyhow::anyhow!("bss-orders-lifecycle: {e}"))?;
            Some(Arc::new(connections))
        } else {
            None
        };
        let runtime = Arc::new(OrdersRuntime {
            db: db.clone(),
            pep: crate::authz::Pep::new(Arc::new(enforcer)),
            maintenance: config.maintenance_authority().map(Arc::new),
            workers,
            worker_settings: config.worker_settings()?,
            worker_metrics: Arc::new(crate::infra::workers::metrics::OtelWorkerMetrics::new()),
            identities: config.actor_identities()?,
            admin_text_key: Arc::new(config.admin_text_key()?),
            _dates: crate::infra::dates::DatePreparer::new(
                db,
                crate::infra::dates::PreparationClock::Database,
            ),
            lease: config.idempotency_lease()?,
            registries,
            hub: ctx.client_hub(),
            producer,
            dependency_timeout,
            _config: config,
        });
        let capture = Arc::new(crate::infra::capture::CaptureService::new(
            Arc::clone(&self.engine),
            runtime._config.capture_settings()?,
        ));
        let reads = Arc::new(crate::infra::read::ReadService::new(
            Arc::clone(&self.read_parts),
            Arc::new(crate::infra::read::OtelReadSignals::new()),
        ));
        // Registration is last: failed prerequisites cannot leave a discoverable local client.
        ctx.client_hub()
            .register::<dyn bss_orders_lifecycle_sdk::OrdersLifecycleV1>(Arc::new(
                crate::infra::capture::LocalOrdersClient::new(
                    Arc::clone(&capture),
                    Arc::clone(&reads),
                ),
            ));
        self.limiter
            .store(Some(crate::api::rest::throttle::PerOrderLimiter::new(
                runtime._config.throttle_settings()?,
            )));
        self.store.store(Some(Arc::new(runtime.db.clone())));
        self.capture.store(Some(capture));
        self.reads.store(Some(reads));
        self.runtime.store(Some(runtime));
        Ok(())
    }
}
impl DatabaseCapability for BssOrdersLifecycleGear {
    fn migrations(&self) -> Vec<Box<dyn sea_orm_migration::MigrationTrait>> {
        crate::infra::storage::migrations()
    }
}
impl RestApiCapability for BssOrdersLifecycleGear {
    fn register_rest(
        &self,
        _ctx: &GearCtx,
        router: axum::Router,
        openapi: &dyn toolkit::api::OpenApiRegistry,
    ) -> anyhow::Result<axum::Router> {
        // Only delivered, authorized handlers are mounted (the S2-12 census,
        // `crate::api::rest::DELIVERED_OPERATIONS`): the S2-09 draft authoring routes, bound to
        // the gateway caller zone and the edge limiter, and the early S6-01/S6-04 draft read
        // routes. They answer unavailable until `serve` binds the engine and the read parts (no
        // write or read before readiness).
        // A gear whose init failed publishes no route, as it publishes no SDK client.
        let (Some(capture), Some(reads), Some(limiter)) = (
            self.capture.load_full(),
            self.reads.load_full(),
            self.limiter.load_full(),
        ) else {
            return Ok(router);
        };
        Ok(router
            .merge(crate::api::rest::capture::router(capture, limiter, openapi))
            .merge(crate::api::rest::read::router(reads, openapi)))
    }
    fn healthcheck(&self, _ctx: &GearCtx) -> Option<Arc<dyn Healthcheck>> {
        Some(Arc::new(OrdersReadiness {
            running: Arc::clone(&self.running),
            producer: Arc::clone(&self.producer),
            engine: Arc::clone(&self.engine),
            read_parts: Arc::clone(&self.read_parts),
            store: Arc::clone(&self.store),
        }))
    }
}

/// Readiness, not liveness (DESIGN §3.8, D-200): the instance is ready only while its lifecycle
/// task runs, the managed producer is bound, the engine and the read parts are mounted and the
/// store answers a real scoped read (the mandatory platform date-policy default). Any loss makes
/// the instance **not ready**, so it stops receiving traffic while it stays alive: the lifecycle
/// task keeps running, the bound producer keeps retrying, and nothing restarts the process into
/// the same unavailable dependency. `/healthz` stays the host's shallow liveness probe.
struct OrdersReadiness {
    running: Arc<AtomicBool>,
    producer: Arc<ArcSwap<ProducerReadiness>>,
    engine: Arc<ArcSwapOption<crate::infra::engine::Engine>>,
    read_parts: Arc<ArcSwapOption<crate::infra::read::ReadParts>>,
    store: Arc<ArcSwapOption<toolkit_db::Db>>,
}

/// Bound on the readiness store probe; the gateway's own per-check timeout still applies.
const STORE_PROBE_TIMEOUT: Duration = Duration::from_secs(2);

#[async_trait]
impl Healthcheck for OrdersReadiness {
    fn name(&self) -> &'static str {
        "bss-orders-lifecycle"
    }
    async fn check(&self) -> HealthcheckResult {
        if !self.running.load(Ordering::Acquire) {
            return HealthcheckResult::unhealthy("Orders Lifecycle is not running")
                .with_code("orders-not-ready");
        }
        let producer = **self.producer.load();
        if producer != ProducerReadiness::Ready {
            return HealthcheckResult::unhealthy(format!(
                "Orders event producer is not bound: {}",
                producer.describe()
            ))
            .with_code(producer.describe());
        }
        if self.engine.load().is_none() || self.read_parts.load().is_none() {
            return HealthcheckResult::unhealthy("Orders engine or reads are not bound")
                .with_code("orders-not-ready");
        }
        let Some(store) = self.store.load_full() else {
            return HealthcheckResult::unhealthy("Orders store is not bound")
                .with_code("orders-store-unavailable");
        };
        let probe = tokio::time::timeout(
            STORE_PROBE_TIMEOUT,
            crate::infra::dates::verify_platform_default(&store),
        )
        .await;
        match probe {
            Ok(Ok(_)) => HealthcheckResult {
                status: toolkit::HealthcheckStatus::Healthy,
                message: Some(
                    "draft capture and draft reads are ready; later packages answer unavailable"
                        .to_owned(),
                ),
                code: None,
            },
            Ok(Err(error)) => {
                tracing::warn!(target: "orders.readiness", error = %error, "Orders store probe failed");
                HealthcheckResult::unhealthy(
                    "Orders store is unavailable or its policy default is missing",
                )
                .with_code("orders-store-unavailable")
            }
            Err(_) => HealthcheckResult::unhealthy("Orders store probe timed out")
                .with_code("orders-store-unavailable"),
        }
    }
}

#[cfg(test)]
mod tests;
