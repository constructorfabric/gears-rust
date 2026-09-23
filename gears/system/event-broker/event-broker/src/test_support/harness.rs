//! Top-level test harness that wires all components together.

use std::sync::Arc;

use authz_resolver_sdk::PolicyEnforcer;
use event_broker_sdk::EventBrokerApi;
use toolkit::client_hub::ClientHub;
use toolkit_db::outbox::{Outbox, Partitions};
use toolkit_security::SecurityContext;
use types_registry_sdk::testing::{MockTypesRegistryClient, make_test_instance};
use uuid::Uuid;

use crate::api::rest::routes::test_router;
use crate::api::rest::state::HandlerState;
use crate::domain::backend::{BackendResolver, SingleBackendResolver};
use crate::domain::delivery::DeliveryService;
use crate::domain::ingest::IngestService;
use crate::domain::local_broker::LocalBroker;
use crate::domain::outbox::INGEST_QUEUE_NAME;
use crate::domain::specification::SpecificationManager;
use crate::infra::specification::TypesRegistrySpecificationManager;
use crate::infra::storage::Storage;
use crate::infra::workers::IngestOutboxHandler;
use sqlite_event_broker_plugin::{EventLogPath, SqliteEventBackend};

/// A fresh temp-file `SQLite` DB (`SQLite` has no row-level locking, so this
/// stays single-process, matching `eb-single-process-implementation` D3),
/// migrated with every table this crate owns (`SpecificationManager`'s
/// cache, `Storage`'s durable namespaces, the `SQLite` `EventBrokerBackend`,
/// the ingest outbox) via the same `Migrator`
/// `EventBrokerModule::migrations()` runs in production. One shared
/// connection pool (`max_conns: 1`) for every table family - `Storage` and
/// the backend are two different types over the *same* underlying `SQLite`
/// file, matching production (`module.rs::init()` resolves one `db` and
/// hands it to both).
async fn test_db() -> Arc<toolkit_db::DBProvider<toolkit_db::DbError>> {
    let mut path = std::env::temp_dir();
    path.push(format!("cf-eb-harness-{}.db", Uuid::now_v7().simple()));
    let mut file = path.to_string_lossy().replace('\\', "/");
    if !file.starts_with('/') {
        file.insert(0, '/');
    }
    let dsn = format!("sqlite://{file}?mode=rwc");
    let opts = toolkit_db::ConnectOpts {
        max_conns: Some(1),
        min_conns: Some(1),
        ..Default::default()
    };
    let db = toolkit_db::connect_db(&dsn, opts)
        .await
        .expect("connect sqlite");
    toolkit_db::migration_runner::run_migrations_for_testing(
        &db,
        <crate::infra::storage::migrations::Migrator as sea_orm_migration::MigratorTrait>::migrations(),
    )
    .await
    .expect("migrations");
    Arc::new(toolkit_db::DBProvider::new(db))
}

/// Seeds a `TypesRegistrySpecificationManager` from `registry`'s
/// `Topic`/`EventType` fixtures via a `MockTypesRegistryClient`, run through
/// the same startup bulk-load path production uses (`module.rs::serve()`) -
/// not a direct in-memory insert, so harness-backed tests exercise the real
/// `types-registry`-to-cache pipeline (eb-single-process-implementation D2
/// risk mitigation).
async fn seeded_spec_manager(
    db: Arc<toolkit_db::DBProvider<toolkit_db::DbError>>,
    registry: Option<super::type_registry::StaticTypesRegistry>,
) -> Arc<dyn SpecificationManager> {
    let mut instances = Vec::new();
    let (schemas, config) = if let Some(registry) = registry {
        for document in registry.topics {
            let id = document["id"]
                .as_str()
                .expect("a topic document names its id");
            instances.push(make_test_instance(id, document.clone()));
        }
        (registry.event_types, registry.config)
    } else {
        (Vec::new(), StaticTypesRegistry::empty_config())
    };
    let client: Arc<dyn types_registry_sdk::TypesRegistryClient> = Arc::new(
        MockTypesRegistryClient::new()
            .with_instances(instances)
            .with_type_schemas(schemas),
    );
    crate::infra::specification::bulk_load(&client, &db, &config)
        .await
        .expect("SpecificationManager bulk-load must not fail");
    Arc::new(TypesRegistrySpecificationManager::new(db))
}

/// The real, permanent `SQLite` `EventBrokerBackend` (eb-single-process-
/// implementation D3) wrapped in a `SingleBackendResolver`, plus the ingest
/// outbox pipeline draining into it - the harness's `Storage` is wired to
/// the same outbox handle via `Storage::set_outbox`, matching
/// `EventBrokerModule::serve()`'s production sequencing. The returned
/// `OutboxHandle` MUST be kept alive for the harness's lifetime - unlike a
/// `ClusterHandle`, dropping an `OutboxHandle` without calling `.stop()`
/// does not leave it harmlessly running: its own doc comment states
/// `TaskSet::Drop` cancels the pipeline's cancellation token on drop,
/// stopping every background worker immediately (discovered the hard way -
/// a dropped-at-function-return handle here meant nothing ever drained).
async fn start_outbox(
    db: Arc<toolkit_db::DBProvider<toolkit_db::DbError>>,
    spec_manager: Arc<dyn SpecificationManager>,
    storage: &Storage,
    cluster_cache: cluster_sdk::ClusterCacheV1,
) -> (
    Arc<dyn event_broker_sdk::EventBrokerBackend>,
    Arc<dyn BackendResolver>,
    toolkit_db::outbox::OutboxHandle,
) {
    // The backend's own event log, not the harness's `db`: that database keeps
    // the metadata behind `Storage` and the ingest outbox, and events are not
    // metadata. In memory, because a harness outlives no process.
    let backend = Arc::new(
        SqliteEventBackend::open(&EventLogPath::InMemory)
            .await
            .expect("an in-memory event log must open"),
    );
    let backend_resolver: Arc<dyn BackendResolver> = Arc::new(SingleBackendResolver::new(
        Arc::clone(&backend) as Arc<dyn event_broker_sdk::EventBrokerBackend>,
    ));

    let handle = Outbox::builder(db.db())
        .queue(INGEST_QUEUE_NAME, Partitions::of(4))
        .leased(IngestOutboxHandler::new(
            Arc::clone(&spec_manager),
            Arc::clone(&backend_resolver),
            cluster_cache,
        ))
        .start()
        .await
        .expect("outbox start");
    storage.set_outbox(Arc::clone(handle.outbox()));

    (
        Arc::clone(&backend) as Arc<dyn event_broker_sdk::EventBrokerBackend>,
        backend_resolver,
        handle,
    )
}

use super::api_v1::ApiV1;
use super::authz_doubles::AllowAllAuthZ;
use super::type_registry::StaticTypesRegistry;

/// Arms/disarms the publish faults on a [`FaultInjectingIngest`]. Cloneable and
/// cheap - the harness keeps one and hands it out via
/// [`EventBrokerHarness::set_publish_rate_limited`].
#[derive(Clone)]
pub struct PublishFaultHandle {
    rate_limited: Arc<std::sync::atomic::AtomicBool>,
}

impl PublishFaultHandle {
    fn new() -> Self {
        Self {
            rate_limited: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        }
    }

    fn set_rate_limited(&self, on: bool) {
        self.rate_limited
            .store(on, std::sync::atomic::Ordering::SeqCst);
    }

    fn rate_limited(&self) -> bool {
        self.rate_limited.load(std::sync::atomic::Ordering::SeqCst)
    }
}

/// A transparent [`IngestService`] wrapper that injects a *real*
/// [`DomainError::RateLimited`] on the publish path when armed, so a test can
/// prove the producer outbox **retries** a transient publish failure instead of
/// rejecting the message. Rate limiting is not otherwise implemented in the
/// gear, so this is the only way to exercise the genuine
/// `DomainError -> CanonicalError(ResourceExhausted) -> EventBrokerError` round
/// trip: the fault is only the trigger, every other operation (and the
/// disarmed default) delegates unchanged to the real ingest, so this is not a
/// broker double.
struct FaultInjectingIngest {
    inner: Arc<dyn IngestService>,
    faults: PublishFaultHandle,
}

impl FaultInjectingIngest {
    fn rate_limit_error() -> crate::domain::error::DomainError {
        crate::domain::error::DomainError::RateLimited {
            code: crate::domain::error::ErrorCode::RateLimited,
            message: "publish rate limit exceeded".to_owned(),
            retry_after_secs: 0,
        }
    }
}

#[async_trait::async_trait]
impl IngestService for FaultInjectingIngest {
    async fn publish_event(
        &self,
        ctx: &SecurityContext,
        request: crate::domain::ingest::PublishRequest,
    ) -> Result<crate::domain::ingest::PublishAck, crate::domain::error::DomainError> {
        if self.faults.rate_limited() {
            return Err(Self::rate_limit_error());
        }
        self.inner.publish_event(ctx, request).await
    }

    async fn publish_batch(
        &self,
        ctx: &SecurityContext,
        requests: Vec<crate::domain::ingest::PublishRequest>,
    ) -> Result<crate::domain::ingest::BatchResult, crate::domain::error::DomainError> {
        if self.faults.rate_limited() {
            return Err(Self::rate_limit_error());
        }
        self.inner.publish_batch(ctx, requests).await
    }

    async fn register_producer(
        &self,
        ctx: &SecurityContext,
        input: crate::domain::ingest::ProducerRegistrationInput,
    ) -> Result<crate::domain::ingest::ProducerRegistration, crate::domain::error::DomainError>
    {
        self.inner.register_producer(ctx, input).await
    }

    async fn get_producer_cursors(
        &self,
        ctx: &SecurityContext,
        producer_id: Uuid,
    ) -> Result<crate::domain::ingest::ProducerCursors, crate::domain::error::DomainError> {
        self.inner.get_producer_cursors(ctx, producer_id).await
    }

    async fn reset_producer(
        &self,
        ctx: &SecurityContext,
        producer_id: Uuid,
        scope: crate::domain::ingest::ProducerResetScope,
    ) -> Result<(), crate::domain::error::DomainError> {
        self.inner.reset_producer(ctx, producer_id, scope).await
    }

    async fn list_topics(
        &self,
        ctx: &SecurityContext,
    ) -> Result<Vec<crate::domain::model::Topic>, crate::domain::error::DomainError> {
        self.inner.list_topics(ctx).await
    }

    async fn list_topic_segments(
        &self,
        ctx: &SecurityContext,
        topic: &toolkit_gts::GtsInstanceId,
        partition: i32,
    ) -> Result<crate::domain::model::TopicSegmentManifest, crate::domain::error::DomainError> {
        self.inner.list_topic_segments(ctx, topic, partition).await
    }

    async fn list_event_types(
        &self,
        ctx: &SecurityContext,
    ) -> Result<Vec<event_broker_sdk::models::EventType>, crate::domain::error::DomainError> {
        self.inner.list_event_types(ctx).await
    }
}

/// Fully-wired test environment for `event-broker` integration tests.
pub struct EventBrokerHarness {
    ingest: Arc<dyn IngestService>,
    delivery: Arc<dyn DeliveryService>,
    storage: Arc<Storage>,
    ctx: SecurityContext,
    router: axum::Router,
    /// Kept alive for the harness's lifetime, never `.stop()`'d - see
    /// `start_outbox`'s doc comment for why dropping it early breaks
    /// draining. Never read after construction, hence `_`-prefixed.
    _outbox_handle: toolkit_db::outbox::OutboxHandle,
    /// The caches the loader fills and a session reads. Exposed so a test can
    /// assert on residency without reaching through the delivery service.
    topics: Arc<crate::infra::loader::topics::TopicManager>,
    /// Stream exclusion, so a test can assert a denied open left no lease held.
    leases: Arc<crate::domain::streaming::lease::InProcessStreamLeases>,
    /// The storage the events actually land in. Exposed because retention is
    /// the backend's own business: a test that needs a prefix-erased or an
    /// emptied partition has to ask the backend to run a pass.
    backend: Arc<dyn event_broker_sdk::EventBrokerBackend>,
    /// Cancels the loader on drop, so a test's loader does not outlive it and
    /// keep fetching against a closing pool.
    loader_shutdown: tokio_util::sync::CancellationToken,
    _loader_handle: tokio::task::JoinHandle<()>,
    /// Holds this instance's in-process client (`LocalBroker`) under
    /// `dyn EventBrokerApi`, exactly as `module.rs::register_rest` publishes it
    /// in production. A consumer resolves `hub.get::<dyn EventBrokerApi>()` and
    /// reaches the harness's real ingest/delivery services with no socket.
    client_hub: Arc<ClientHub>,
    /// Arms the publish-path fault injection wrapped around `ingest`.
    publish_faults: PublishFaultHandle,
}

impl Drop for EventBrokerHarness {
    fn drop(&mut self) {
        self.loader_shutdown.cancel();
    }
}

impl EventBrokerHarness {
    /// The partition caches the loader fills.
    #[must_use]
    pub fn topics(&self) -> &Arc<crate::infra::loader::topics::TopicManager> {
        &self.topics
    }

    /// Stream exclusion for this instance.
    #[must_use]
    pub fn leases(&self) -> &Arc<crate::domain::streaming::lease::InProcessStreamLeases> {
        &self.leases
    }

    /// The backend behind every topic here, for a test that has to run a
    /// retention pass or read what storage holds.
    #[must_use]
    pub fn backend(&self) -> &Arc<dyn event_broker_sdk::EventBrokerBackend> {
        &self.backend
    }

    #[must_use]
    pub fn builder() -> EventBrokerHarnessBuilder {
        EventBrokerHarnessBuilder::default()
    }

    #[must_use]
    pub fn api_v1(&self) -> ApiV1<'_> {
        ApiV1::new(self)
    }

    #[must_use]
    pub fn ingest(&self) -> &dyn IngestService {
        &*self.ingest
    }

    #[must_use]
    pub fn delivery(&self) -> &dyn DeliveryService {
        &*self.delivery
    }

    /// Low-level access to the backing `Storage` - topic/event-type
    /// fixtures go through `EventBrokerHarnessBuilder::with_type_registry`
    /// instead; this stays for whatever else a test needs the real repo
    /// for (`ConsumerGroupRepo`/`CursorRepo`/`SubscriptionRepo`/
    /// `ActiveStreamMarker` - `Storage` implements all of them).
    #[must_use]
    pub fn repo(&self) -> &Storage {
        &self.storage
    }

    #[must_use]
    pub fn security_context(&self) -> &SecurityContext {
        &self.ctx
    }

    /// Test fault injection: forget a producer's server-side registration, the
    /// state the registration Reaper (a future ticket) or a `P30D` age-out
    /// leaves behind. Afterwards the `producer_id` resolves to nothing, so the
    /// next publish naming it is a `404 ProducerNotFound`; a managed producer
    /// with `on_unknown = RegisterNew` recovers by rotating to a fresh
    /// registration. There is no client-facing deregister to drive this through,
    /// which is why it is a harness hook rather than an API call.
    pub async fn forget_producer(&self, producer_id: uuid::Uuid) {
        self.storage
            .delete_producer_registration(producer_id)
            .await
            .expect("forget_producer: delete registration row");
    }

    /// Test fault injection: when `on`, every `publish`/`publish_batch` through
    /// this instance fails with a real `RateLimited` (retry-after 0) before
    /// reaching the real ingest, so a producer-outbox test can prove a transient
    /// publish failure is retried rather than rejected. Off by default and
    /// reversible.
    pub fn set_publish_rate_limited(&self, on: bool) {
        self.publish_faults.set_rate_limited(on);
    }

    pub fn router(&self) -> &axum::Router {
        &self.router
    }

    /// The `ClientHub` this harness published its in-process client into - a
    /// consumer resolves `dyn EventBrokerApi` from it just as it would against a
    /// running gear.
    #[must_use]
    pub fn client_hub(&self) -> &Arc<ClientHub> {
        &self.client_hub
    }

    /// This instance's in-process `EventBrokerApi`, resolved from the hub. The
    /// SDK's integration tests drive a consumer/producer against this in place
    /// of the deleted mock.
    #[must_use]
    pub fn broker(&self) -> Arc<dyn EventBrokerApi> {
        self.client_hub
            .get::<dyn EventBrokerApi>()
            .expect("the harness registers a LocalBroker in build()")
    }
}

/// Builder for [`EventBrokerHarness`].
#[derive(Default)]
pub struct EventBrokerHarnessBuilder {
    type_registry: Option<StaticTypesRegistry>,
    policy_enforcer: Option<PolicyEnforcer>,
    heartbeat: Option<std::time::Duration>,
}

impl EventBrokerHarnessBuilder {
    /// Seeds `Topic`/`EventType` fixtures from a [`StaticTypesRegistry`].
    #[must_use]
    pub fn with_type_registry(mut self, registry: StaticTypesRegistry) -> Self {
        self.type_registry = Some(registry);
        self
    }

    /// The delivery heartbeat cadence (default 1s, the production whole-second
    /// value). A test that asserts slow-consumer detection or idle heartbeats
    /// sets a sub-second value so it runs fast without waiting out a real tick;
    /// production `StreamingConfig` is unchanged.
    #[must_use]
    pub fn with_heartbeat(mut self, heartbeat: std::time::Duration) -> Self {
        self.heartbeat = Some(heartbeat);
        self
    }

    /// Overrides the default always-allow `PolicyEnforcer` (default:
    /// `AllowAllAuthZ`) - for tests asserting a specific authz denial.
    #[must_use]
    pub fn with_policy_enforcer(mut self, policy_enforcer: PolicyEnforcer) -> Self {
        self.policy_enforcer = Some(policy_enforcer);
        self
    }

    #[must_use]
    pub async fn build(self) -> EventBrokerHarness {
        let policy_enforcer = self
            .policy_enforcer
            .unwrap_or_else(|| PolicyEnforcer::new(Arc::new(AllowAllAuthZ)));

        let db = test_db().await;
        let spec_manager = seeded_spec_manager(Arc::clone(&db), self.type_registry).await;

        let (_hub, cluster) = crate::test_support::standalone_event_broker_cluster().await;
        let storage = Arc::new(Storage::new(Arc::clone(&db), Arc::clone(&spec_manager)));
        storage.set_cache(cluster.cache.clone());

        let (backend, backend_resolver, outbox_handle) = start_outbox(
            Arc::clone(&db),
            Arc::clone(&spec_manager),
            &storage,
            cluster.cache,
        )
        .await;

        // The harness runs a real loader over a real `TopicManager`, so a test
        // that opens a stream exercises the production fill path rather than a
        // hand-fed cache. The tick is short because a test should not wait out a
        // production cadence.
        let loader_cfg = crate::config::LoaderConfig {
            tick_ms: 2,
            ..crate::config::LoaderConfig::default()
        };
        let topics = crate::infra::wiring::build_topic_manager(&loader_cfg);
        let leases = Arc::new(crate::domain::streaming::lease::InProcessStreamLeases::new());
        let loader_shutdown = tokio_util::sync::CancellationToken::new();
        let loader_handle = tokio::spawn(crate::infra::wiring::loader_future(
            &loader_cfg,
            Arc::clone(&topics),
            Arc::clone(&spec_manager),
            Arc::clone(&backend_resolver),
            loader_shutdown.clone(),
        ));

        let attacher: Arc<dyn crate::domain::streaming::source::ReaderAttacher> = topics.clone();
        let HandlerState { ingest, delivery } = crate::infra::wiring::build_handler_state(
            Arc::clone(&storage),
            policy_enforcer,
            spec_manager,
            backend_resolver,
            attacher,
            Arc::clone(&leases),
            crate::config::BatchConfig::default(),
            // Short cadences so idle behaviour is testable without real waits.
            // Everything else is the production default.
            crate::config::StreamingConfig {
                heartbeat_interval_secs: 1,
                ..crate::config::StreamingConfig::default()
            },
            // Default matches the whole-second `StreamingConfig` above; a test
            // that needs a fast heartbeat overrides it via `with_heartbeat`.
            self.heartbeat.unwrap_or(std::time::Duration::from_secs(1)),
        );

        // Wrap ingest so a test can arm a transient publish fault; disarmed by
        // default, so every other test sees the real ingest unchanged.
        let publish_faults = PublishFaultHandle::new();
        let ingest: Arc<dyn IngestService> = Arc::new(FaultInjectingIngest {
            inner: ingest,
            faults: publish_faults.clone(),
        });

        let ctx = SecurityContext::builder()
            .subject_tenant_id(Uuid::new_v4())
            .subject_id(Uuid::new_v4())
            .build()
            .expect("test security context");

        let router = test_router(
            HandlerState {
                ingest: Arc::clone(&ingest),
                delivery: Arc::clone(&delivery),
            },
            ctx.clone(),
        );

        // Publish the in-process client into a hub the harness owns, mirroring
        // `module.rs::register_rest`: the SDK's tests resolve it back out rather
        // than construct a broker directly.
        let client_hub = Arc::new(ClientHub::new());
        let broker: Arc<dyn EventBrokerApi> =
            Arc::new(LocalBroker::new(Arc::clone(&ingest), Arc::clone(&delivery)));
        client_hub.register::<dyn EventBrokerApi>(broker);

        EventBrokerHarness {
            ingest,
            delivery,
            storage,

            ctx,
            router,
            _outbox_handle: outbox_handle,
            topics,
            leases,
            backend,
            loader_shutdown,
            _loader_handle: loader_handle,
            client_hub,
            publish_faults,
        }
    }
}
