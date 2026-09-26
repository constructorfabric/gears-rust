//! `EventBrokerModule`: the `ModKit` gear that wires Ingest/Delivery/
//! Dispatcher/Reaper per deployment mode (`DESIGN.md:2224`'s Deployment
//! Modes table; `docs/ADR/0007-service-decomposition.md`).
//!
//! `init` resolves and stores the configured [`DeploymentMode`], whose
//! `*_active()` predicates report which services/routes *should* exist for
//! that mode. Dispatcher forwarding routes register when
//! `dispatcher_active()` (`eb-dispatcher-routing`); ingest/delivery service
//! construction happens in `register_rest()` against the real `Storage`
//! (eb-single-process-implementation D2 risk mitigation - `InMemoryDomainRepo`
//! is gone). `serve()` self-registers with `DirectoryService` in
//! `cluster_ingest`/`cluster_delivery` mode (design.md D4/D5), starts the
//! ingest outbox pipeline (design.md D5) whenever `ingest_active()`, and
//! otherwise starts no background work.

use std::net::SocketAddr;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use anyhow::Context;
use async_trait::async_trait;
use authz_resolver_sdk::{AuthZResolverApi, PolicyEnforcer};
use axum::Extension;
use event_broker_sdk::{EventBrokerApi, EventBrokerBackendProvider};
use tokio_util::sync::CancellationToken;
use toolkit::api::OpenApiRegistry;
use toolkit::client_hub::ClientHub;
use toolkit::directory::{DirectoryClient, RegisterInstanceInfo, ServiceEndpoint};
use toolkit::{DatabaseCapability, Gear, GearCtx, Healthcheck, RestApiCapability};
use toolkit_db::outbox::{Outbox, Partitions};
use uuid::Uuid;

use crate::api::rest::routes;
use crate::config::{DeploymentMode, EventBrokerConfig, RegistrationConfig, StreamingConfig};
use crate::domain::cluster::EventBrokerCluster;
use crate::domain::local_broker::LocalBroker;
use crate::domain::outbox::INGEST_OUTBOX_PARTITIONS;
use crate::infra::cluster::{AdvertiseAddressResolver, ConfigAdvertiseAddress};
use crate::infra::dispatcher::DispatcherState;
use crate::infra::health::EventBrokerReadiness;
use crate::infra::storage::Storage;
use crate::infra::workers::IngestOutboxHandler;

/// How often a registered ingest/delivery instance sends a heartbeat to
/// `DirectoryService` to stay routable (design.md D4). A placeholder
/// interval - `docs/DESIGN.md`'s "Open — broker team" note flags that
/// `DirectoryService`'s actual heartbeat-loss timing still needs to be
/// confirmed against the broker's failover budget.
const DIRECTORY_HEARTBEAT_INTERVAL: Duration = Duration::from_secs(15);

/// The storage backend provider assembled from the backend plugins linked into
/// this build; a second backend adds a line here and nowhere else.
///
/// Mirrors the cluster gear's own `provider_registry()`: the plugin implements
/// the SDK's provider trait, the gear names it once at wiring, and nothing else
/// in the gear knows which backend it got. No database handle is passed: a
/// backend opens the storage its own options name, which is what lets a topic's
/// events live somewhere this gear's metadata does not.
fn backend_provider() -> impl EventBrokerBackendProvider {
    sqlite_event_broker_plugin::SqliteBackendProvider
}

/// Expressed as predicates on [`DeploymentMode`] itself so the activation
/// set can never disagree with the mode it's derived from
/// (`docs/ADR/0007-service-decomposition.md` D6).
impl DeploymentMode {
    /// `DESIGN.md:2224`'s Deployment Modes table, column by column.
    #[must_use]
    pub fn ingest_active(self) -> bool {
        matches!(self, Self::Standalone | Self::ClusterIngest)
    }

    #[must_use]
    pub fn delivery_active(self) -> bool {
        matches!(self, Self::Standalone | Self::ClusterDelivery)
    }

    #[must_use]
    pub fn dispatcher_active(self) -> bool {
        matches!(self, Self::ClusterDispatcher)
    }

    /// The `reaper` worker runs in every mode except `cluster_dispatcher`
    /// (a stateless HTTP gateway has nothing for it to reap).
    #[must_use]
    pub fn reaper_active(self) -> bool {
        !matches!(self, Self::ClusterDispatcher)
    }
}

#[toolkit::gear(
    name = "event-broker",
    deps = [cluster, authz_resolver, types_registry],
    capabilities = [db, rest, stateful],
    lifecycle(entry = "serve", stop_timeout = "30s")
)]
#[derive(Default)]
pub struct EventBrokerModule {
    mode: OnceLock<DeploymentMode>,
    client_hub: OnceLock<Arc<ClientHub>>,
    registration: OnceLock<RegistrationConfig>,
    batch: OnceLock<crate::config::BatchConfig>,
    streaming: OnceLock<StreamingConfig>,
    loader: OnceLock<crate::config::LoaderConfig>,
    /// Resolved once in `init()` (`ctx.db_required()`, after migrations have
    /// already run) - shared by `SpecificationManager`'s cache, `Storage`, and
    /// the ingest outbox. Not by the storage backend: the event log lives in a
    /// database that backend opens for itself, so a topic's events can be
    /// somewhere this gear's metadata is not.
    db: OnceLock<Arc<toolkit_db::DBProvider<toolkit_db::DbError>>>,
    /// Built in `init()` (`register_rest()` is sync, so it cannot construct
    /// this itself) and read by `register_rest()`. The actual
    /// cache-table population (`infra::specification::bulk_load`, a free
    /// function - not a method on this trait, since it never touches the
    /// object itself, only the `TypesRegistryClient`/`db` `init()` also has
    /// in hand) happens later, from `serve()` - see that method's own doc
    /// comment for why.
    spec_manager: OnceLock<Arc<dyn crate::domain::specification::SpecificationManager>>,
    /// The partition caches this instance serves from. Built in `init()` because
    /// two callers need the *same* one: `register_rest()` hands it to the
    /// delivery service so a session can attach readers, and `serve()` hands it
    /// to the loader so those readers get filled.
    topics: OnceLock<Arc<crate::infra::loader::topics::TopicManager>>,
    /// This instance's consumer groups and the subscriptions in them. Built in
    /// `init` for the same reason as `topics`: `register_rest` hands it to the
    /// delivery service and `serve` hands it to the sweeper, and two would be
    /// two disjoint sets of members.
    groups: OnceLock<Arc<crate::domain::consumer_group_coordinator::ConsumerGroupCoordinator>>,
    /// The whole operator config, kept because the retention worker needs two
    /// parts of it that no other collaborator does: the per-topic settings map
    /// and the deployment's default backend name, which together decide what
    /// bounds each topic is held to.
    config: OnceLock<EventBrokerConfig>,
    /// The storage backend a topic's events are stored by and read back
    /// from, wrapped in the trivial `SingleBackendResolver` - built in
    /// `init()` alongside `spec_manager` from the backend plugin's provider,
    /// read by `register_rest()`.
    backend_resolver: OnceLock<Arc<dyn crate::domain::backend::BackendResolver>>,
    /// The real `Storage` (`ConsumerGroupRepo`/`CursorRepo`/`RoutingMarkers`/
    /// `IdempotencyGuard`/`ProducerRegistry`/`ActiveStreamMarker`) -
    /// `InMemoryDomainRepo`'s permanent replacement (eb-single-process-
    /// implementation D2 risk mitigation). Built in `init()`, read by
    /// `register_rest()`; its ingest outbox is wired in later, by `serve()`.
    storage: OnceLock<Arc<Storage>>,
}

#[async_trait]
impl Gear for EventBrokerModule {
    async fn init(&self, ctx: &GearCtx) -> anyhow::Result<()> {
        let cfg: EventBrokerConfig = ctx.config()?;
        // Resolved before anything is wired, so a configuration that names two
        // different backends for one instance fails at startup rather than
        // after half the gear is built.
        let backend_selection = cfg.backend_selection()?;
        self.config
            .set(cfg.clone())
            .map_err(|_| anyhow::anyhow!("{} module already initialized", Self::MODULE_NAME))?;
        self.mode
            .set(cfg.mode)
            .map_err(|_| anyhow::anyhow!("{} module already initialized", Self::MODULE_NAME))?;
        self.client_hub
            .set(ctx.client_hub())
            .map_err(|_| anyhow::anyhow!("{} module already initialized", Self::MODULE_NAME))?;
        if matches!(
            cfg.mode,
            DeploymentMode::ClusterIngest | DeploymentMode::ClusterDelivery
        ) {
            let bound_addr: SocketAddr =
                cfg.registration.listen_addr.parse().with_context(|| {
                    format!(
                        "invalid registration.listen_addr '{}'",
                        cfg.registration.listen_addr
                    )
                })?;
            // Fail fast at startup rather than only once `serve()` actually
            // registers (design.md D5) - the resolved address itself is
            // discarded; `register_self()` recomputes it (a pure, cheap
            // string operation, not worth caching across the two calls).
            ConfigAdvertiseAddress {
                config: &cfg.registration,
            }
            .resolve(bound_addr)?;
        }
        self.registration
            .set(cfg.registration)
            .map_err(|_| anyhow::anyhow!("{} module already initialized", Self::MODULE_NAME))?;
        self.batch
            .set(cfg.batch)
            .map_err(|_| anyhow::anyhow!("{} module already initialized", Self::MODULE_NAME))?;
        self.streaming
            .set(cfg.streaming)
            .map_err(|_| anyhow::anyhow!("{} module already initialized", Self::MODULE_NAME))?;

        // Built here, not in `register_rest()` or `serve()`, because both of
        // them need this exact instance - see the field comment.
        self.topics
            .set(crate::infra::wiring::build_topic_manager(&cfg.loader))
            .map_err(|_| anyhow::anyhow!("{} module already initialized", Self::MODULE_NAME))?;
        self.loader
            .set(cfg.loader)
            .map_err(|_| anyhow::anyhow!("{} module already initialized", Self::MODULE_NAME))?;
        self.groups
            .set(Arc::new(
                crate::domain::consumer_group_coordinator::ConsumerGroupCoordinator::new(
                    std::time::Duration::from_secs(u64::from(cfg.subscription.join_timeout_secs)),
                ),
            ))
            .map_err(|_| anyhow::anyhow!("{} module already initialized", Self::MODULE_NAME))?;

        // `DatabaseCapability::migrations()` has already run by this point
        // (`libs/toolkit/src/runtime/host_runtime.rs`'s `run_db_phase()`
        // precedes `init()`), so `event_broker_spec_cache` and friends
        // already exist.
        let db = Arc::new(ctx.db_required()?);
        self.db
            .set(Arc::clone(&db))
            .map_err(|_| anyhow::anyhow!("{} module already initialized", Self::MODULE_NAME))?;

        // eb-single-process-implementation D1: the actual cache-table
        // populate (`infra::specification::bulk_load`) happens later, in
        // `serve()` - see that method's own doc comment for why. The
        // manager is usable immediately either way: `get_topic`/etc. query
        // the SQLite cache table directly on every call, so handing
        // services this not-yet-loaded manager now is safe as long as the
        // table is populated before any real request can reach them
        // (guaranteed: REST traffic doesn't arrive until the start phase,
        // `serve()`'s own phase, completes).
        let spec_manager: Arc<dyn crate::domain::specification::SpecificationManager> = Arc::new(
            crate::infra::specification::TypesRegistrySpecificationManager::new(Arc::clone(&db)),
        );
        self.spec_manager
            .set(Arc::clone(&spec_manager))
            .map_err(|_| anyhow::anyhow!("{} module already initialized", Self::MODULE_NAME))?;

        // The storage backend comes from a plugin, built through its provider
        // the way the cluster gear builds a cache backend from a cache
        // provider. The gear links the plugin but knows only the
        // `EventBrokerBackend` trait behind it - which is what lets a second
        // backend bring its own storage and its own retention rather than
        // inheriting this one's.
        //
        // Every topic resolves to it regardless of `topic`, so it is built from
        // the selection every configured topic agrees on; binding a topic to the
        // backend its own settings name is the next step, and needs the
        // per-topic settings to be the source of truth first.
        //
        // The database it opens is its own. The one resolved above keeps ingest
        // and delivery metadata - cursors, consumer groups, producers, the
        // specification cache, the ingest outbox - and the event log is not
        // among them.
        let provider = backend_provider();
        // The configured type has to be one this build can serve. Checked here
        // rather than trusted, because a type nothing implements would otherwise
        // reach the one linked backend and be stored by it silently, under a
        // name the operator did not choose.
        if backend_selection.r#type.as_ref() != provider.backend_type() {
            anyhow::bail!(
                "configuration names backend type '{}', which this build does not serve; it links '{}'",
                backend_selection.r#type.as_ref(),
                provider.backend_type(),
            );
        }
        let backend = provider
            .build_backend(&backend_selection.settings)
            .await
            .map_err(|e| anyhow::anyhow!("could not build the storage backend: {e}"))?;
        self.backend_resolver
            .set(Arc::new(
                crate::domain::backend::SingleBackendResolver::new(backend),
            ))
            .map_err(|_| anyhow::anyhow!("{} module already initialized", Self::MODULE_NAME))?;

        // eb-single-process-implementation D2 risk mitigation: the real
        // `Storage`, wired against the same `db` and `spec_manager`. Its
        // `ClusterCacheV1` (backing the routing markers)
        // is deliberately NOT resolved here - `ClusterGear` only registers
        // its backends into the `ClientHub` during the platform's *start*
        // phase, which runs after every gear's `init()`
        // (`host_runtime.rs`'s phase order). `serve()` resolves and wires it
        // in once that phase has begun (`Storage::set_cache`'s own doc
        // comment has the full story - found by actually booting the
        // standalone binary, since every test wires the cluster cache
        // directly, bypassing this ordering constraint entirely).
        self.storage
            .set(Arc::new(Storage::new(
                db,
                Arc::clone(&spec_manager),
                ctx.instance_id(),
            )))
            .map_err(|_| anyhow::anyhow!("{} module already initialized", Self::MODULE_NAME))?;

        tracing::info!(
            module = Self::MODULE_NAME,
            mode = ?cfg.mode,
            "event-broker deployment-mode wiring resolved"
        );
        Ok(())
    }
}

/// Every table this gear owns (`SpecificationManager`'s cache, `Storage`'s
/// durable namespaces, the ingest outbox) is registered here as one
/// idempotent migration - gears must not receive
/// a raw DB connection (`libs/toolkit/src/contracts.rs`'s `DatabaseCapability`
/// rule). Runs automatically before `init()` on every `Run`
/// (`eb-single-process-implementation` design.md D7's "No versioned
/// migration chain" note) - not a separate `Migrate` step.
impl DatabaseCapability for EventBrokerModule {
    fn migrations(&self) -> Vec<Box<dyn sea_orm_migration::MigrationTrait>> {
        use sea_orm_migration::MigratorTrait;
        crate::infra::storage::migrations::Migrator::migrations()
    }
}

impl RestApiCapability for EventBrokerModule {
    /// Dispatcher forwarding routes register when `dispatcher_active()`
    /// (`eb-dispatcher-routing`). Ingest/delivery routes register when
    /// `ingest_active()`/`delivery_active()` - both share one `HandlerState`,
    /// built the same way `test_support::harness::EventBrokerHarness` does
    /// (`infra::wiring::build_handler_state`, over the same real `Storage`
    /// both construct their services against).
    ///
    /// Resolves `EventBrokerCluster` once and attaches it (plus a shared
    /// Pingora connector) via `Extension` after registration, matching the
    /// "attach service once after all routes are registered" convention
    /// (`docs/toolkit_unified_system/04_rest_operation_builder.md`).
    fn register_rest(
        &self,
        ctx: &GearCtx,
        router: axum::Router,
        openapi: &dyn OpenApiRegistry,
    ) -> anyhow::Result<axum::Router> {
        let mode = *self
            .mode
            .get()
            .ok_or_else(|| anyhow::anyhow!("init must run before register_rest()"))?;

        let mut router = router;
        if mode.ingest_active() {
            router = routes::register_ingest_routes(router, openapi);
        }
        if mode.delivery_active() {
            router = routes::register_delivery_routes(router, openapi);
        }
        if mode.ingest_active() || mode.delivery_active() {
            // The whole struct, not one field: every knob in it is read
            // downstream now - batch bounds and progress cadence by the session,
            // the heartbeat by its schedule.
            let streaming = self
                .streaming
                .get()
                .ok_or_else(|| anyhow::anyhow!("init must run before register_rest()"))?
                .clone();
            let batch = self
                .batch
                .get()
                .ok_or_else(|| anyhow::anyhow!("init must run before register_rest()"))?
                .clone();
            // Real clients, not the test harness's permissive-by-default
            // doubles - `infra::wiring::build_handler_state` is this gear's
            // only production `HandlerState` construction path today
            // (`eb-authz-enforcement`'s design.md Context), so this is the
            // call that actually turns enforcement on in production.
            let authz: Arc<dyn AuthZResolverApi> =
                ctx.client_hub().get::<dyn AuthZResolverApi>()?;
            let spec_manager = self
                .spec_manager
                .get()
                .ok_or_else(|| anyhow::anyhow!("init must run before register_rest()"))?
                .clone();
            let backend_resolver = self
                .backend_resolver
                .get()
                .ok_or_else(|| anyhow::anyhow!("init must run before register_rest()"))?
                .clone();
            let storage = self
                .storage
                .get()
                .ok_or_else(|| anyhow::anyhow!("init must run before register_rest()"))?
                .clone();
            // The same `TopicManager` the loader fills, not a second one -
            // see the field comment on `topics`.
            let topics = self
                .topics
                .get()
                .ok_or_else(|| anyhow::anyhow!("init must run before register_rest()"))?
                .clone();
            let groups = self
                .groups
                .get()
                .ok_or_else(|| anyhow::anyhow!("init must run before register_rest()"))?
                .clone();
            // The production heartbeat is the configured whole-second knob; the
            // `Duration` arg exists only so a test harness can run it faster.
            let heartbeat =
                std::time::Duration::from_secs(u64::from(streaming.heartbeat_interval_secs));
            let state = crate::infra::wiring::build_handler_state(
                storage,
                PolicyEnforcer::new(authz),
                spec_manager,
                backend_resolver,
                topics,
                groups,
                // Process-local: a group is owned by one delivery instance, so
                // stream exclusion needs no storage and no coordination.
                Arc::new(crate::domain::streaming::lease::InProcessStreamLeases::new()),
                batch,
                streaming,
                heartbeat,
            );
            // Publish this process's in-process client (design.md: the gear's
            // own `EventBrokerApi` implementation) into the ClientHub, so an
            // embedded consumer resolves `dyn EventBrokerApi` and reaches the
            // real ingest/delivery services with no socket in the path - the
            // local variant beside the SDK's REST client, the same way oagw
            // publishes `ServiceGatewayClientV1Facade`. The two services are
            // shared with the REST layer (`state`), not a second copy.
            let broker: Arc<dyn EventBrokerApi> = Arc::new(LocalBroker::new(
                state.ingest.clone(),
                state.delivery.clone(),
            ));
            ctx.client_hub().register::<dyn EventBrokerApi>(broker);
            router = router.layer(Extension(state));
        }

        if !mode.dispatcher_active() {
            return Ok(router);
        }
        let router = routes::register_dispatcher_routes(router, openapi);
        let directory: Arc<dyn DirectoryClient> = ctx.client_hub().get::<dyn DirectoryClient>()?;
        let state = Arc::new(DispatcherState::new(directory));
        Ok(router.layer(Extension(state)))
    }

    /// Holds `/readyz` at `503` until `serve()` has wired the handles this
    /// instance's roles need.
    ///
    /// Without it the platform advertises the pod the instant the start phase
    /// *spawns* `serve()`, while `start_workers` is still resolving the
    /// cluster cache and starting the outbox - so a publish can land on a pod
    /// that reports itself ready and get a `503` from the storage facade.
    ///
    /// Captures the `Storage` rather than the handles themselves: it is
    /// collected in the REST phase, a phase before either exists, and reads
    /// them through `Storage`'s own `OnceLock`s at check time.
    ///
    /// Returning `None` would opt the gear out of readiness altogether, so the
    /// unreachable no-`init` arm still yields a verdict - `Starting`, the
    /// fail-safe direction - rather than reporting a pod with no storage ready.
    fn healthcheck(&self, _ctx: &GearCtx) -> Option<Arc<dyn Healthcheck>> {
        let (Some(storage), Some(mode)) = (self.storage.get(), self.mode.get()) else {
            tracing::error!(
                module = Self::MODULE_NAME,
                "healthcheck collected before init - reporting Starting until it runs"
            );
            return Some(Arc::new(NotInitialised));
        };
        Some(Arc::new(EventBrokerReadiness::new(
            Arc::clone(storage),
            *mode,
        )))
    }
}

/// The verdict for a gear whose `init()` has not run: `init` precedes the REST
/// phase, so this is unreachable, and it reports `Starting` rather than
/// assuming health if it ever is reached.
struct NotInitialised;

#[async_trait::async_trait]
impl Healthcheck for NotInitialised {
    fn name(&self) -> &'static str {
        "event-broker-readiness"
    }

    async fn check(&self) -> toolkit::HealthcheckResult {
        toolkit::HealthcheckResult::unhealthy("event-broker init has not run").with_code("starting")
    }
}

impl EventBrokerModule {
    /// Crate-internal introspection point for the per-mode gating tests
    /// (`docs/ADR/0007-service-decomposition.md` D6) - not public API.
    #[cfg(test)]
    pub(crate) fn mode(&self) -> DeploymentMode {
        *self.mode.get().expect("init must run before mode()")
    }

    /// Background lifecycle entry point (`lifecycle(entry = "serve")`).
    ///
    /// Every background task this gear runs - specification refresh, the ingest
    /// outbox pipeline, the delivery loader, retention, and directory presence -
    /// is one supervised worker in a single [`Supervisor`], keyed on a child of
    /// the lifecycle token. `start_workers` registers exactly the workers the
    /// [`DeploymentMode`] calls for; `supervise` then blocks until the first
    /// worker returns for any reason (a clean cancel, an error, or a panic),
    /// cancels the child token to bring the rest down, drains them, and returns
    /// the first non-success outcome. So cancellation or any worker's fatal exit
    /// tears the whole set down together, and no partially-started set is ever
    /// leaked: if a `start_*` fails midway, `shutdown` cancels and drains
    /// whatever already started.
    ///
    /// Directory presence is just another worker: in `cluster_ingest`/
    /// `cluster_delivery` it registers with `DirectoryService` (design.md D4,
    /// register-on-start awaited so a failure is fatal), heartbeats until
    /// cancelled, and deregisters on shutdown (`DirectoryClient` has no TTL-lease
    /// semantics, unlike the removed `ServiceDiscoveryV1`); in `standalone`/
    /// `cluster_dispatcher` it is a bare cancel-gate that keeps the set non-empty
    /// so `serve()` blocks until cancellation. The `reaper` worker lands with a
    /// future ticket - it slots in as one more `start_*` under `reaper_active()`.
    pub(crate) async fn serve(&self, cancel: CancellationToken) -> anyhow::Result<()> {
        let mode = *self
            .mode
            .get()
            .ok_or_else(|| anyhow::anyhow!("init must run before serve()"))?;

        // Child token: workers observe it, and the supervisor cancels it on the
        // first worker exit without touching the lifecycle's own token.
        let mut supervisor = Supervisor::new(cancel.child_token());
        if let Err(e) = self.start_workers(&mut supervisor, mode).await {
            supervisor.shutdown().await;
            return Err(e);
        }
        supervisor.supervise().await
    }

    /// Registers exactly the workers `mode` calls for, in the established order,
    /// each into `sup` on its child token.
    async fn start_workers(
        &self,
        sup: &mut Supervisor,
        mode: DeploymentMode,
    ) -> anyhow::Result<()> {
        // Must happen before anything else, and only where the ingest role runs.
        // `types-registry` commits `entities:`-seeded instances from its
        // configuration-mode storage to queryable storage during the platform's
        // `post_init`, which runs after every gear's `init()` but before
        // `serve()`, so a load in `init()` would always see zero of them -
        // discovered by seeding a topic that way and finding `GET /v1/topics`
        // still empty afterwards.
        //
        // Exactly one role writes this state and the others re-read it: a
        // delivery instance reads what ingest resolved out of the shared
        // database, which is why it needs none of the `topics` configuration
        // that shaped it, and a dispatcher holds none of it at all.
        if mode.ingest_active() {
            self.start_specification_refresh(sup).await?;
        }

        // Must happen before anything else that follows: `cluster`'s own
        // `start()` (which actually registers its backends into the
        // `ClientHub`) is guaranteed to have already run by the time
        // `serve()` is invoked (topo-sorted dependency order within the
        // platform's start phase - `EventBrokerModule`'s `deps = [cluster,
        // ...]`), but nothing before `serve()` is. See `Storage::set_cache`'s
        // doc comment. Returns the resolved cache so `start_outbox_pipeline`
        // can hand the same handle to `IngestOutboxHandler` (design.md D6's
        // delivery-wake-up notification) without re-resolving it.
        let cluster_cache = self.wire_cluster_cache().await?;

        if mode.ingest_active() {
            self.start_outbox_pipeline(sup, cluster_cache).await?;
        }

        // Only where delivery runs: an ingest-only instance serves no streams,
        // so a loader there would fetch for readers that cannot exist.
        if mode.delivery_active() {
            self.start_loader(sup)?;
            self.start_subscription_sweeper(sup)?;
        }

        // Wherever events are stored: an instance that holds rows is the one
        // that has to keep them bounded, whether or not anything reads them.
        if mode.ingest_active() || mode.delivery_active() {
            self.start_retention(sup)?;
        }

        // Registered last, so the instance advertises itself only once its
        // workers are up.
        self.start_directory_presence(sup, mode).await
    }

    /// Registers directory presence as one supervised worker.
    ///
    /// In `cluster_ingest`/`cluster_delivery` it registers with
    /// `DirectoryService` (register-on-start awaited, so a failure is fatal and
    /// tears down the workers that already started), heartbeats until the child
    /// token fires, and deregisters on shutdown. In `standalone`/
    /// `cluster_dispatcher` there is no directory, so the worker is a bare
    /// cancel-gate: it keeps the set non-empty - a dispatcher has no other
    /// workers - so `serve()` blocks until cancellation instead of returning
    /// against an empty set.
    async fn start_directory_presence(
        &self,
        sup: &mut Supervisor,
        mode: DeploymentMode,
    ) -> anyhow::Result<()> {
        let gear_name = match mode {
            DeploymentMode::ClusterIngest => "event-broker-ingest",
            DeploymentMode::ClusterDelivery => "event-broker-delivery",
            DeploymentMode::Standalone | DeploymentMode::ClusterDispatcher => {
                let token = sup.cancel().clone();
                sup.spawn("cancel-gate", async move {
                    token.cancelled().await;
                    Ok(())
                });
                return Ok(());
            }
        };

        let (directory, info) = self.register_self(gear_name).await?;
        let token = sup.cancel().clone();
        sup.spawn("directory-presence", async move {
            presence_loop(&directory, &info, &token).await;
            if let Err(e) = directory
                .deregister_instance(&info.gear, &info.instance_id)
                .await
            {
                tracing::warn!(
                    module = EventBrokerModule::MODULE_NAME,
                    gear = %info.gear,
                    error = %e,
                    "deregister on shutdown failed"
                );
            }
            Ok(())
        });
        Ok(())
    }

    /// Resolves `EventBrokerCluster` and wires its `cache` into `Storage`
    /// (`Storage::set_cache`'s own doc comment has the full ordering
    /// rationale). Must be called from `serve()` - `cluster`'s backends
    /// aren't registered into the `ClientHub` until the platform's start
    /// phase.
    async fn wire_cluster_cache(&self) -> anyhow::Result<cluster_sdk::ClusterCacheV1> {
        let hub = self
            .client_hub
            .get()
            .ok_or_else(|| anyhow::anyhow!("init must run before wire_cluster_cache()"))?;
        let storage = self
            .storage
            .get()
            .ok_or_else(|| anyhow::anyhow!("init must run before wire_cluster_cache()"))?;
        let cache = EventBrokerCluster::resolve(hub)
            .await
            .context("resolving the event-broker cluster profile failed")?
            .cache;
        storage.set_cache(cache.clone());
        Ok(cache)
    }

    /// Registers the specification refresh worker.
    ///
    /// Loads the specification cache once, awaited, then registers a loop that
    /// refreshes it on the configured cadence until the child token fires. The
    /// first pass is awaited because nothing may serve a request against an
    /// empty cache, and REST traffic starts arriving as soon as the platform's
    /// start phase completes.
    async fn start_specification_refresh(&self, sup: &mut Supervisor) -> anyhow::Result<()> {
        let db = self
            .db
            .get()
            .ok_or_else(|| anyhow::anyhow!("init must run before serve()"))?;
        let config = self
            .config
            .get()
            .ok_or_else(|| anyhow::anyhow!("init must run before serve()"))?;
        let types_registry = self
            .client_hub
            .get()
            .ok_or_else(|| anyhow::anyhow!("init must run before serve()"))?
            .get::<dyn types_registry_sdk::TypesRegistryClient>()?;

        let worker = crate::infra::workers::SpecificationRefreshWorker::new(
            types_registry,
            Arc::clone(db),
            config.clone(),
            Duration::from_secs(u64::from(
                config.workers.specification_refresh_interval_secs,
            )),
        );
        worker.run_once().await;
        let token = sup.cancel().clone();
        sup.spawn("specification-refresh", async move {
            worker.run(token).await;
            Ok(())
        });
        Ok(())
    }

    /// Registers the tick that drives each topic's backend through one retention
    /// pass, running until the child token fires.
    fn start_retention(&self, sup: &mut Supervisor) -> anyhow::Result<()> {
        let config = self
            .config
            .get()
            .ok_or_else(|| anyhow::anyhow!("init must run before serve()"))?;
        let spec_manager = self
            .spec_manager
            .get()
            .ok_or_else(|| anyhow::anyhow!("init must run before serve()"))?
            .clone();
        let backend_resolver = self
            .backend_resolver
            .get()
            .ok_or_else(|| anyhow::anyhow!("init must run before serve()"))?
            .clone();

        let worker = crate::infra::workers::RetentionWorker::new(
            spec_manager,
            backend_resolver,
            config.clone(),
            Duration::from_secs(u64::from(config.workers.retention_interval_secs)),
        );
        let token = sup.cancel().clone();
        sup.spawn("retention", async move {
            worker.run(token).await;
            Ok(())
        });
        Ok(())
    }

    /// Registers the sweeper that reaps subscriptions past their state's
    /// lifetime, running until the child token fires.
    fn start_subscription_sweeper(&self, sup: &mut Supervisor) -> anyhow::Result<()> {
        let groups = self
            .groups
            .get()
            .ok_or_else(|| anyhow::anyhow!("init must run before serve()"))?
            .clone();
        let markers: Arc<dyn crate::domain::repo::RoutingMarkers> = self
            .storage
            .get()
            .ok_or_else(|| anyhow::anyhow!("init must run before serve()"))?
            .clone();
        let token = sup.cancel().clone();
        sup.spawn("subscription-sweeper", async move {
            crate::domain::consumer_group_coordinator::sweeper::run(groups, markers, token).await;
            Ok(())
        });
        Ok(())
    }

    /// Registers the loader that fills partition caches ahead of readers and
    /// reclaims behind them, running until the child token fires.
    fn start_loader(&self, sup: &mut Supervisor) -> anyhow::Result<()> {
        let topics = self
            .topics
            .get()
            .ok_or_else(|| anyhow::anyhow!("init must run before serve()"))?;
        let loader_cfg = self
            .loader
            .get()
            .ok_or_else(|| anyhow::anyhow!("init must run before serve()"))?;
        let spec_manager = self
            .spec_manager
            .get()
            .ok_or_else(|| anyhow::anyhow!("init must run before serve()"))?
            .clone();
        let backend_resolver = self
            .backend_resolver
            .get()
            .ok_or_else(|| anyhow::anyhow!("init must run before serve()"))?
            .clone();

        let token = sup.cancel().clone();
        let loader = crate::infra::wiring::loader_future(
            loader_cfg,
            Arc::clone(topics),
            spec_manager,
            backend_resolver,
            token,
        );
        sup.spawn("loader", async move {
            loader.await;
            Ok(())
        });
        Ok(())
    }

    /// Builds and starts the ingest outbox pipeline (design.md D5):
    /// `Outbox::builder(db).queue(INGEST_QUEUE_NAME, ..).leased(handler).
    /// start()`, then wires the resulting `Arc<Outbox>` into `Storage` via
    /// [`Storage::set_outbox`] so `IdempotencyGuard::check_and_enqueue` has
    /// somewhere to insert - done before the handle is handed to the worker,
    /// since the enqueue path needs it immediately.
    ///
    /// Unlike the loop workers, the pipeline runs its own internal task set and
    /// is stopped by consuming its `OutboxHandle`. So the worker registered here
    /// is a bridge: it awaits the child token, then `handle.stop().await` drains
    /// the pipeline. (The pipeline's own internal task failures are not
    /// observable through `OutboxHandle`, so this worker reacts only to cancel.)
    async fn start_outbox_pipeline(
        &self,
        sup: &mut Supervisor,
        cluster_cache: cluster_sdk::ClusterCacheV1,
    ) -> anyhow::Result<()> {
        let db = self
            .db
            .get()
            .ok_or_else(|| anyhow::anyhow!("init must run before start_outbox_pipeline()"))?;
        let storage = self
            .storage
            .get()
            .ok_or_else(|| anyhow::anyhow!("init must run before start_outbox_pipeline()"))?;
        let spec_manager = self
            .spec_manager
            .get()
            .ok_or_else(|| anyhow::anyhow!("init must run before start_outbox_pipeline()"))?
            .clone();
        let backend_resolver = self
            .backend_resolver
            .get()
            .ok_or_else(|| anyhow::anyhow!("init must run before start_outbox_pipeline()"))?
            .clone();

        let handle = Outbox::builder(db.db())
            .queue(
                crate::domain::outbox::INGEST_QUEUE_NAME,
                Partitions::of(INGEST_OUTBOX_PARTITIONS),
            )
            .leased(IngestOutboxHandler::new(
                spec_manager,
                backend_resolver,
                cluster_cache,
            ))
            .start()
            .await
            .map_err(|e| anyhow::anyhow!("ingest outbox start: {e}"))?;
        storage.set_outbox(Arc::clone(handle.outbox()));

        let token = sup.cancel().clone();
        sup.spawn("ingest-outbox", async move {
            token.cancelled().await;
            tracing::info!(
                module = EventBrokerModule::MODULE_NAME,
                "stopping ingest outbox pipeline"
            );
            handle.stop().await;
            Ok(())
        });

        tracing::info!(module = Self::MODULE_NAME, "ingest outbox pipeline started");
        Ok(())
    }

    /// Resolves the advertise address (design.md D5) and registers this
    /// instance with `DirectoryService` under `gear_name`
    /// (`"event-broker-ingest"`/`"event-broker-delivery"` - prefixed so this
    /// intra-gear role registration can't collide with another gear's real
    /// name in `DirectoryService`'s flat namespace). Returns the resolved
    /// `DirectoryClient` and the `RegisterInstanceInfo` used, so the caller
    /// can heartbeat/deregister with the same identity.
    async fn register_self(
        &self,
        gear_name: &str,
    ) -> anyhow::Result<(Arc<dyn DirectoryClient>, RegisterInstanceInfo)> {
        let hub = self
            .client_hub
            .get()
            .ok_or_else(|| anyhow::anyhow!("init must run before register_self()"))?;
        let registration = self
            .registration
            .get()
            .ok_or_else(|| anyhow::anyhow!("init must run before register_self()"))?;

        let bound_addr: SocketAddr = registration.listen_addr.parse().with_context(|| {
            format!(
                "invalid registration.listen_addr '{}'",
                registration.listen_addr
            )
        })?;
        let address = ConfigAdvertiseAddress {
            config: registration,
        }
        .resolve(bound_addr)?;

        let directory: Arc<dyn DirectoryClient> = hub.get::<dyn DirectoryClient>()?;
        let info = RegisterInstanceInfo::new(gear_name.to_owned(), Uuid::new_v4().to_string())
            .with_rest_endpoint(ServiceEndpoint::new(address));
        directory.register_instance(info.clone()).await?;
        tracing::info!(
            module = Self::MODULE_NAME,
            gear = gear_name,
            instance_id = %info.instance_id,
            "registered with DirectoryService"
        );
        Ok((directory, info))
    }
}

/// Sends a heartbeat every [`DIRECTORY_HEARTBEAT_INTERVAL`] until `cancel`
/// fires. Unlike `toolkit::runtime`'s own (private) `presence_loop`, this
/// doesn't self-heal by re-registering on heartbeat failure - a simplification
/// acceptable for this gear's intra-role registration, revisit if
/// `DirectoryService` restarts turn out to be disruptive in practice.
///
/// A free function, not a method, because the directory-presence worker owns
/// `directory`/`info` and runs on the `'static` `JoinSet` - it borrows nothing
/// from the module.
async fn presence_loop(
    directory: &Arc<dyn DirectoryClient>,
    info: &RegisterInstanceInfo,
    cancel: &CancellationToken,
) {
    let mut heartbeat = tokio::time::interval(DIRECTORY_HEARTBEAT_INTERVAL);
    heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    heartbeat.tick().await; // consume the immediate first tick

    loop {
        tokio::select! {
            () = cancel.cancelled() => return,
            _ = heartbeat.tick() => {
                if let Err(e) = directory.send_heartbeat(&info.gear, &info.instance_id).await {
                    tracing::warn!(
                        module = EventBrokerModule::MODULE_NAME,
                        gear = %info.gear,
                        error = %e,
                        "heartbeat failed"
                    );
                }
            }
        }
    }
}

/// Owns every background worker for one `serve()` call and brings them down
/// together. A worker is any future that runs until the shared cancel token
/// fires and then returns; the supervisor cancels that token the instant the
/// first worker returns - cleanly, with an error, or by panic - so no worker
/// outlives the set. A `start_*` failing midway therefore tears down whatever
/// already started (see [`Supervisor::shutdown`]) instead of leaking it.
struct Supervisor {
    cancel: CancellationToken,
    tasks: tokio::task::JoinSet<anyhow::Result<()>>,
}

impl Supervisor {
    fn new(cancel: CancellationToken) -> Self {
        Self {
            cancel,
            tasks: tokio::task::JoinSet::new(),
        }
    }

    /// The token every worker observes. Handed to each `start_*` so a worker
    /// built there joins the same lifetime.
    fn cancel(&self) -> &CancellationToken {
        &self.cancel
    }

    /// Registers one worker. `name` rides in an error the worker returns, so a
    /// fatal exit names the worker; a panic is reported generically (the
    /// runtime keeps the payload, not the name).
    fn spawn<F>(&mut self, name: &'static str, worker: F)
    where
        F: std::future::Future<Output = anyhow::Result<()>> + Send + 'static,
    {
        self.tasks.spawn(async move {
            worker
                .await
                .with_context(|| format!("worker '{name}' exited with an error"))
        });
    }

    /// Blocks until the first worker returns for any reason, cancels the rest,
    /// drains them, and yields the first non-success outcome (an error return or
    /// a panic). A clean shutdown - the external token firing - makes every
    /// worker return `Ok(())`, so this returns `Ok(())`.
    async fn supervise(mut self) -> anyhow::Result<()> {
        let mut outcome = Ok(());
        while let Some(joined) = self.tasks.join_next().await {
            let result = match joined {
                Ok(worker_result) => worker_result,
                Err(join) if join.is_panic() => {
                    Err(anyhow::anyhow!("a background worker panicked"))
                }
                // Aborted while draining teardown - expected, not a failure.
                Err(_) => Ok(()),
            };
            if result.is_err() && outcome.is_ok() {
                outcome = result;
            }
            // The first return of any kind means shutdown: bring the rest down.
            self.cancel.cancel();
        }
        outcome
    }

    /// Startup-failure path: cancel and drain whatever already started, so a
    /// `start_*` failing partway through never leaks the earlier workers.
    async fn shutdown(mut self) {
        self.cancel.cancel();
        while self.tasks.join_next().await.is_some() {}
    }
}

#[cfg(test)]
#[path = "module_tests.rs"]
mod module_tests;
