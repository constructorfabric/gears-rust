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
    /// operations is delivered in production (Preview's development delivery holds a clone).
    _dates: crate::infra::dates::DatePreparer,
    /// The explicit positive finite registry lease (Foundation §3.6).
    lease: crate::domain::idempotency::LeaseDuration,
    /// State table, guard registry and field classes compiled and validated at init (§3.4).
    registries: crate::infra::engine::Registries,
    /// The gate ports (S3-14): real clients or fail-closed adapters, never a double. The
    /// authority is validated at init; the catalog clients are bound from `serve`. No route, SDK
    /// method or engine path reads them yet.
    gate: Arc<crate::infra::gate::GatePorts>,
    /// Event Broker and the tenant resolver publish their clients after Orders' init (the broker
    /// in its REST phase), so the producer is bound from `serve` through this hub; the gate's
    /// Pricing and Products clients (no dependency edge) are bound from `serve` the same way.
    hub: Arc<toolkit::ClientHub>,
    producer: crate::infra::broker::ProducerSettings,
    dependency_timeout: Duration,
    _config: OrdersConfig,
}

/// Upper bound of the producer-binding retry backoff.
const BIND_BACKOFF_MAX: Duration = Duration::from_secs(30);

/// The Orders gear: mounts the delivered draft authoring and read routes, registers the local SDK
/// client, binds the managed event producer and starts the configured maintenance workers. No
/// operation is admitted before the producer is bound (readiness), and none after stop.
///
/// `authn_resolver` is a dependency (D-194): a configured commercial identity must be able to
/// authenticate at init, and the resolver publishes `AuthNResolverClient` in its own `init`, so
/// without the edge the init order of the two gears would be arbitrary.
#[toolkit::gear(name = "bss-orders", deps = [authz_resolver, authn_resolver, types_registry, tenant_resolver, event_broker], capabilities = [db, rest, stateful], lifecycle(entry = "serve", stop_timeout = "30s"))]
pub struct BssOrdersGear {
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
    limiter: ArcSwapOption<crate::infra::throttle::PerOrderLimiter>,
    /// The store the readiness probe reads (DESIGN §3.8: store unavailability makes the instance
    /// not ready, never restarted); `None` before init and after shutdown.
    store: Arc<ArcSwapOption<toolkit_db::Db>>,
    /// The gate ports (S3-14), whose live manifest the readiness message reports; never a
    /// readiness condition. `None` before init and after shutdown.
    gate: Arc<ArcSwapOption<crate::infra::gate::GatePorts>>,
    /// The Preview service (S3-11), set at init only in the development delivery (M2-1:
    /// [`gate_dev_delivery`]); the REST route and the local SDK delegate to it, and in every
    /// other build it stays `None` (route unmounted, SDK `NotDelivered`).
    preview: ArcSwapOption<crate::infra::gate::preview::PreviewService>,
    /// The submit service (S3-12), under the same development-delivery condition as `preview`;
    /// the REST route (M3b, D-234) and the local SDK delegate to it, `None` in every other build.
    submit: ArcSwapOption<crate::infra::gate::submit::SubmitService>,
}

/// Submit's development acceptance double (M2-6): with the `gate-dev-stubs` feature and the stubs
/// bound, a [`SellabilityStub`](crate::infra::gate::stubs::SellabilityStub) over the fixture's
/// acceptance policy, accepting on Pricing's own `resolve` read from the hub. `None` otherwise:
/// phase B then reads Pricing's published `SellabilityV1`, or answers unavailable.
#[cfg(feature = "gate-dev-stubs")]
fn dev_acceptance(
    gate: &crate::infra::gate::GatePorts,
    hub: Arc<toolkit::ClientHub>,
) -> Option<Arc<dyn bss_pricing_sdk::acceptance::SellabilityV1>> {
    use crate::infra::gate::stubs::{HubResolveBindings, SellabilityStub};
    let fixture = gate.dev_fixture()?;
    tracing::warn!(
        target: "orders.gate.dev_stubs",
        "bss-orders: a development stub issues submit's Pricing acceptance receipts; never a \
         production binding"
    );
    Some(Arc::new(SellabilityStub::new(
        fixture,
        Arc::new(HubResolveBindings(hub)),
    )))
}
/// Without the feature there is no development acceptance double.
#[cfg(not(feature = "gate-dev-stubs"))]
fn dev_acceptance(
    _gate: &crate::infra::gate::GatePorts,
    _hub: Arc<toolkit::ClientHub>,
) -> Option<Arc<dyn bss_pricing_sdk::acceptance::SellabilityV1>> {
    None
}

/// M2-1: the gate's development delivery switch. Both the `gate-dev-stubs` feature and
/// `gate.dev_stubs = true` are required (validation already refuses the flag without the
/// feature); only then do Preview and submit (each one's REST route and local SDK method) run
/// the real service over the development stubs.
fn gate_dev_delivery(config: &crate::config::GateConfig) -> bool {
    config.dev_delivery_fixture().is_some()
}

/// The development delivery, built once: the orchestrator both services share and each flow's
/// caller limit. Present only for the development delivery, so no flow can be half-built.
struct DevDelivery {
    orchestrator: crate::infra::gate::orchestrate::Orchestrator,
    preview_limit: crate::infra::throttle::CallerLimitSettings,
    submit_limit: crate::infra::throttle::CallerLimitSettings,
}
impl Default for BssOrdersGear {
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
            gate: Arc::new(ArcSwapOption::from(None)),
            preview: ArcSwapOption::from(None),
            submit: ArcSwapOption::from(None),
        }
    }
}
impl BssOrdersGear {
    /// Run until `cancel`: start the workers, bind the event producer, publish the engine and
    /// read parts while bound, then withdraw them and stop.
    pub(crate) async fn serve(
        self: Arc<Self>,
        cancel: tokio_util::sync::CancellationToken,
    ) -> anyhow::Result<()> {
        let runtime = self
            .runtime
            .load_full()
            .ok_or_else(|| anyhow::anyhow!("bss-orders: not initialized"))?;
        self.running.store(true, Ordering::Release);
        // The five Orders-owned workers start with the lifecycle: cleanup, purge and audit need
        // no producer; the expiry/auto-void slots report their undelivered bodies. Each pass
        // takes its advisory key on the host connection and stops with this token.
        let workers = self.start_workers(&runtime, &cancel);
        Self::start_gate(&runtime);
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
                "bss-orders: event producer bound"
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
            runtime.worker_metrics.scheduled(0);
        }
        self.scheduled_workers.store(None);
        self.producer.store(Arc::new(ProducerReadiness::Stopped));
        self.running.store(false, Ordering::Release);
        self.store.store(None);
        self.gate.store(None);
        self.runtime.store(None);
        Ok(())
    }

    /// S3-14: bind the gate's catalog clients here, not at init: every gear's `init` has
    /// completed before any `serve`, so the binding does not depend on boot order (Pricing and
    /// Products have no dependency edge to Orders). [`Self::bind_until_ready`] retries it with
    /// each producer bind attempt. Logs the manifest once; binding never calls a port.
    fn start_gate(runtime: &OrdersRuntime) {
        Self::bind_gate_catalog(runtime);
        let gate = runtime.gate.readiness();
        tracing::info!(
            target: "orders.gate",
            manifest = %serde_json::to_string(&gate).unwrap_or_default(),
            "bss-orders: {}",
            gate.summary()
        );
    }

    /// Bind the gate's catalog port to the Pricing and Products clients when both are published
    /// in the hub (a no-op once bound); returns whether it is bound. Logs the manifest when this
    /// call binds it. No port is called.
    fn bind_gate_catalog(runtime: &OrdersRuntime) -> bool {
        let catalog = &runtime.gate.catalog;
        let was_bound = catalog.is_bound();
        let bound = catalog.bind_from_hub(&runtime.hub);
        if bound && !was_bound {
            let gate = runtime.gate.readiness();
            tracing::info!(
                target: "orders.gate",
                manifest = %serde_json::to_string(&gate).unwrap_or_default(),
                "bss-orders: gate catalog bound; {}",
                gate.summary()
            );
        }
        bound
    }

    /// Bind the managed producer, retrying with bounded, fully jittered exponential backoff
    /// until bound or cancelled. A failed attempt is visible as readiness and a warning, never a
    /// restart loop.
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
                            error = &error as &dyn std::error::Error,
                            "bss-orders: event producer not ready"
                        );
                        self.producer
                            .store(Arc::new(ProducerReadiness::Unavailable(error.failure)));
                        // The gate's catalog clients join the producer's retry (S3-14).
                        Self::bind_gate_catalog(runtime);
                    }
                },
            }
            tokio::select! {
                () = cancel.cancelled() => return None,
                () = tokio::time::sleep(crate::infra::jitter::full_jitter(backoff)) => {}
            }
            backoff = (backoff * 2).min(BIND_BACKOFF_MAX);
        }
    }

    /// Start the Orders-owned workers under the configured authority and class connections.
    /// Without a maintenance block nothing is scheduled: every task fails closed, which is
    /// reported as a warning and an `orders_workers_scheduled` gauge of zero (review finding
    /// 185). Readiness is deliberately not affected: serving traffic does not depend on the
    /// workers, and a not-ready instance would not schedule them either.
    fn start_workers(
        &self,
        runtime: &OrdersRuntime,
        cancel: &tokio_util::sync::CancellationToken,
    ) -> Option<crate::infra::workers::WorkerHandles> {
        use crate::infra::workers::{self, WorkerParts, expiry::SweepUnavailable};
        let (Some(authority), Some(connections)) = (&runtime.maintenance, &runtime.workers) else {
            tracing::warn!(
                "bss-orders: no maintenance authority configured; no worker scheduled: idempotency \
                 cleanup, retention purge and audit verification/checkpointing are disabled"
            );
            runtime.worker_metrics.scheduled(0);
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
                status: Arc::clone(&self.worker_status),
            },
            cancel,
        );
        runtime
            .worker_metrics
            .scheduled(u64::try_from(handles.scheduled().len()).unwrap_or(u64::MAX));
        self.scheduled_workers
            .store(Some(Arc::new(handles.scheduled().to_vec())));
        Some(handles)
    }

    /// Last pass result per worker (the lifecycle tests). Readiness does not report it: the
    /// workers are observed through their pass metrics and the `orders_workers_scheduled` gauge.
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "readiness does not depend on the workers (their metrics report them); the \
                      lifecycle tests read the per-worker results"
        )
    )]
    pub(crate) fn worker_status(&self) -> &crate::infra::workers::WorkerStatus {
        &self.worker_status
    }

    /// The live gate manifest (the gear-boot tests; the readiness check holds its own handle to
    /// the slot and reports the summary).
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "the readiness check holds its own handle to the gate slot; only the \
                      gear-boot tests read the full manifest through the gear"
        )
    )]
    pub(crate) fn gate_readiness(&self) -> Option<crate::infra::gate::readiness::GateReadiness> {
        self.gate.load().as_ref().map(|gate| gate.readiness())
    }

    /// The workers scheduled by `serve`.
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "no runtime path lists the scheduled workers; the lifecycle tests assert the set"
        )
    )]
    pub(crate) fn scheduled_workers(&self) -> Vec<crate::infra::workers::WorkerKind> {
        self.scheduled_workers
            .load_full()
            .map(|v| v.as_ref().clone())
            .unwrap_or_default()
    }

    /// Current managed-producer readiness (the lifecycle tests; the readiness check reads the
    /// shared slot directly).
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "the readiness check holds its own handle to the producer slot; only the \
                      lifecycle tests read it through the gear"
        )
    )]
    pub(crate) fn producer_readiness(&self) -> ProducerReadiness {
        **self.producer.load()
    }

    /// The bound event sink, once the producer is ready (readiness tests; the engine holds its
    /// own handle to the same sink).
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "the engine is built with its own sink handle; only the lifecycle tests read \
                      the gear slot"
        )
    )]
    pub(crate) fn event_sink(&self) -> Option<Arc<EventSink>> {
        self.events.load_full()
    }

    /// The transition engine, once the producer is bound (readiness tests). The mounted draft
    /// authoring routes and the local SDK enter it through the capture service, which shares
    /// this slot.
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "the capture service and readiness hold their own handles to the engine slot; \
                      only the lifecycle tests read it through the gear"
        )
    )]
    pub(crate) fn engine(&self) -> Option<Arc<crate::infra::engine::Engine>> {
        self.engine.load_full()
    }
}
#[async_trait]
impl Gear for BssOrdersGear {
    async fn init(&self, ctx: &GearCtx) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.runtime.load().is_none(),
            "bss-orders: already initialized"
        );
        let config: OrdersConfig = ctx
            .config_expanded()
            .context("bss-orders: explicit config required")?;
        config.validate()?;
        let resolver = ctx
            .client_hub()
            .get::<dyn authz_resolver_sdk::AuthZResolverApi>()
            .context("bss-orders: AuthZResolverApi required")?;
        let registry = ctx
            .client_hub()
            .get::<dyn types_registry_sdk::TypesRegistryClient>()
            .context("bss-orders: TypesRegistryClient required")?;
        let db = ctx
            .db_required()
            .context("bss-orders: database required")?
            .db();
        anyhow::ensure!(
            db.backend() == sea_orm::DbBackend::Postgres,
            "bss-orders: PostgreSQL is required; other backends are unproven"
        );
        let date_policy = config.date_policy_plan()?;
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
        .context("bss-orders: registry initialization timed out")??;
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
            .context("bss-orders: maintenance connections timed out")?
            .map_err(|e| anyhow::anyhow!("bss-orders: {e}"))?;
            Some(Arc::new(connections))
        } else {
            None
        };
        // One identities value backs the PEP's decision class and the engine's audit class.
        let identities = config.actor_identities()?;
        // S3-14: every gate port bound to its fail-closed adapter; configured commercial
        // identities without a published AuthN resolver refuse startup. Pricing and Products are
        // optional (missing providers are the designed state) and bound from `serve`.
        let budget = Arc::new(crate::infra::gate::budget::Budget::from_config(
            &config.gate,
            Arc::new(crate::infra::gate::signals::OtelGateSignals::new()),
        ));
        let gate = crate::infra::gate::GatePorts::from_hub(
            &ctx.client_hub(),
            &config.gate,
            Arc::clone(&budget),
        )
        .map(Arc::new)?;
        let dates = crate::infra::dates::DatePreparer::new(
            db.clone(),
            crate::infra::dates::PreparationClock::Database,
        );
        let pep = crate::authz::Pep::new(Arc::new(enforcer), identities.clone());
        // S3-11/S3-12 under M2-1: the Preview and submit services over the same PEP, ports,
        // budget and date preparer, built only for the development delivery.
        let dev: Option<DevDelivery> = gate_dev_delivery(&config.gate)
            .then(|| -> anyhow::Result<_> {
                Ok(DevDelivery {
                    orchestrator: crate::infra::gate::orchestrate::Orchestrator::new(
                        Arc::clone(&gate),
                        budget,
                        Arc::new(crate::infra::gate::orchestrate::StorageInflightClaims::new(
                            db.clone(),
                        )),
                    ),
                    preview_limit: config.gate.preview_limit()?,
                    submit_limit: config.gate.submit_limit()?,
                })
            })
            .transpose()?;
        let preview = dev.as_ref().map(|dev| {
            Arc::new(crate::infra::gate::preview::PreviewService::new(
                pep.clone(),
                dev.orchestrator.clone(),
                Arc::new(dates.clone()),
                Arc::new(crate::infra::gate::preview::StoragePreviewRuns::new(
                    db.clone(),
                )),
                crate::infra::throttle::PerCallerLimiter::new(
                    crate::infra::throttle::CallerLimiter::Preview,
                    dev.preview_limit,
                ),
            ))
        });
        let submit_dates = dates.clone();
        let runtime = Arc::new(OrdersRuntime {
            db: db.clone(),
            pep,
            maintenance: config.maintenance_authority().map(Arc::new),
            workers,
            worker_settings: config.worker_settings()?,
            worker_metrics: Arc::new(crate::infra::workers::metrics::OtelWorkerMetrics::new()),
            identities,
            admin_text_key: Arc::new(config.admin_text_key()?),
            _dates: dates,
            lease: config.idempotency_lease()?,
            registries,
            gate: Arc::clone(&gate),
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
        let submit = dev
            .map(|dev| -> anyhow::Result<_> {
                Ok(Arc::new(
                    crate::infra::gate::submit::SubmitService::new(
                        Arc::clone(&self.engine),
                        Arc::clone(&reads),
                        dev.orchestrator,
                        Arc::new(submit_dates),
                        gate.catalog.authority(),
                        crate::infra::throttle::PerCallerLimiter::new(
                            crate::infra::throttle::CallerLimiter::Submit,
                            dev.submit_limit,
                        ),
                        runtime._config.capture_settings()?.line_cap,
                    )
                    .with_acceptance_budget(
                        crate::infra::gate::budget::AcceptanceBudget::from_config(
                            &runtime._config.gate,
                        ),
                    )
                    .with_acceptance_hub(ctx.client_hub())
                    .with_acceptance(dev_acceptance(&gate, ctx.client_hub())),
                ))
            })
            .transpose()?;
        let throttle = runtime._config.throttle_settings()?;
        // DESIGN 02 §3.7/§3.8: the policy channel promotes the configured date-policy rows with
        // the deployment, and startup refuses a missing or invalid platform default. It is the
        // only init step with a persistent effect, so it runs after every fallible step (the
        // registry registration, the worker connection attestation, the settings): a start that
        // fails earlier writes nothing (review finding 186). It is bounded like the registry
        // step (finding 45). The promotion is one transaction under the namespace lock: an
        // elapse before its commit rolls it back; one during the commit leaves it either applied
        // or not, and the next start's compare-and-apply converges on the same plan.
        tokio::time::timeout(
            dependency_timeout,
            crate::infra::dates::install(&runtime.db, date_policy.as_ref()),
        )
        .await
        .context("bss-orders: date policy installation timed out")??;
        // Registration is last: failed prerequisites cannot leave a discoverable local client.
        ctx.client_hub()
            .register::<dyn bss_orders_sdk::OrdersV1>(Arc::new(
                crate::api::local_client::LocalOrdersClient::new(
                    Arc::clone(&capture),
                    Arc::clone(&reads),
                )
                .with_preview(preview.clone())
                .with_submit(submit.clone()),
            ));
        self.limiter
            .store(Some(crate::infra::throttle::PerOrderLimiter::new(throttle)));
        self.store.store(Some(Arc::new(runtime.db.clone())));
        self.gate.store(Some(gate));
        self.preview.store(preview);
        self.submit.store(submit);
        self.capture.store(Some(capture));
        self.reads.store(Some(reads));
        self.runtime.store(Some(runtime));
        Ok(())
    }
}
impl DatabaseCapability for BssOrdersGear {
    fn migrations(&self) -> Vec<Box<dyn sea_orm_migration::MigrationTrait>> {
        crate::infra::storage::migrations()
    }
}
impl RestApiCapability for BssOrdersGear {
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
        let mut router = router
            .merge(crate::api::rest::capture::router(
                capture,
                Arc::clone(&limiter),
                openapi,
            ))
            .merge(crate::api::rest::read::router(reads, openapi));
        // M2-1: the Preview and submit routes exist only in the development delivery (the
        // services are set only then); the production surface stays the census. Submit (M3b,
        // D-234) is an existing-order write and shares the authoring routes' edge limiter.
        if let Some(preview) = self.preview.load_full() {
            router = router.merge(crate::api::rest::gate::router(preview, openapi));
        }
        if let Some(submit) = self.submit.load_full() {
            router = router.merge(crate::api::rest::gate::submit_router(
                submit, limiter, openapi,
            ));
        }
        Ok(router)
    }
    fn healthcheck(&self, _ctx: &GearCtx) -> Option<Arc<dyn Healthcheck>> {
        Some(Arc::new(OrdersReadiness {
            running: Arc::clone(&self.running),
            producer: Arc::clone(&self.producer),
            engine: Arc::clone(&self.engine),
            read_parts: Arc::clone(&self.read_parts),
            store: Arc::clone(&self.store),
            gate: Arc::clone(&self.gate),
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
    /// The gate ports; their live manifest's summary rides the ready message and never decides
    /// readiness.
    gate: Arc<ArcSwapOption<crate::infra::gate::GatePorts>>,
}

/// Bound on the readiness store probe; the gateway's own per-check timeout still applies.
const STORE_PROBE_TIMEOUT: Duration = Duration::from_secs(2);

#[async_trait]
impl Healthcheck for OrdersReadiness {
    fn name(&self) -> &'static str {
        "bss-orders"
    }
    async fn check(&self) -> HealthcheckResult {
        if !self.running.load(Ordering::Acquire) {
            return HealthcheckResult::unhealthy("Orders is not running")
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
            Ok(Ok(_)) => {
                let mut message =
                    "draft capture and draft reads are ready; later packages answer unavailable"
                        .to_owned();
                // S3-14: the gate section. Missing providers are the designed state, so the
                // manifest is reported, never a readiness condition.
                if let Some(gate) = self.gate.load_full() {
                    message.push_str("; ");
                    message.push_str(&gate.readiness().summary());
                }
                HealthcheckResult {
                    status: toolkit::HealthcheckStatus::Healthy,
                    message: Some(message),
                    code: None,
                }
            }
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
