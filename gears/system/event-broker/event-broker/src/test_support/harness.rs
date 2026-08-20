//! Top-level test harness that wires all components together.

use std::sync::Arc;

use authz_resolver_sdk::PolicyEnforcer;
use toolkit_db::outbox::{Outbox, Partitions};
use toolkit_security::SecurityContext;
use types_registry_sdk::testing::{MockTypesRegistryClient, make_test_instance};
use uuid::Uuid;

use crate::api::rest::routes::test_router;
use crate::api::rest::state::HandlerState;
use crate::domain::backend::{BackendResolver, SingleBackendResolver};
use crate::domain::delivery::DeliveryService;
use crate::domain::ingest::IngestService;
use crate::domain::outbox::INGEST_QUEUE_NAME;
use crate::domain::specification::SpecificationManager;
use crate::infra::specification::TypesRegistrySpecificationManager;
use crate::infra::storage::Storage;
use crate::infra::storage::builtin::SqliteEventBackend;
use crate::infra::workers::IngestOutboxHandler;

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
    path.push(format!(
        "cf-eb-harness-{}.db",
        Uuid::now_v7().simple()
    ));
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
    if let Some(registry) = registry {
        for topic in registry.topics {
            let payload = serde_json::to_value(&topic).expect("Topic must serialize");
            instances.push(make_test_instance(topic.id.as_ref(), payload));
        }
        for event_type in registry.event_types {
            let payload = serde_json::to_value(&event_type).expect("EventType must serialize");
            instances.push(make_test_instance(event_type.id.as_ref(), payload));
        }
    }
    let client: Arc<dyn types_registry_sdk::TypesRegistryClient> =
        Arc::new(MockTypesRegistryClient::new().with_instances(instances));
    crate::infra::specification::bulk_load(&client, &db)
        .await
        .expect("SpecificationManager bulk-load must not fail");
    Arc::new(TypesRegistrySpecificationManager::new(client, db))
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
) -> (Arc<dyn BackendResolver>, toolkit_db::outbox::OutboxHandle) {
    let backend = Arc::new(SqliteEventBackend::new(Arc::clone(&db)));
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

    (backend_resolver, handle)
}

use super::api_v1::ApiV1;
use super::authz_doubles::AllowAllAuthZ;
use super::type_registry::StaticTypesRegistry;

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
}

impl EventBrokerHarness {
    pub fn builder() -> EventBrokerHarnessBuilder {
        EventBrokerHarnessBuilder::default()
    }

    pub fn api_v1(&self) -> ApiV1<'_> {
        ApiV1::new(self)
    }

    pub fn ingest(&self) -> &dyn IngestService {
        &*self.ingest
    }

    pub fn delivery(&self) -> &dyn DeliveryService {
        &*self.delivery
    }

    /// Low-level access to the backing `Storage` - topic/event-type
    /// fixtures go through `EventBrokerHarnessBuilder::with_type_registry`
    /// instead; this stays for whatever else a test needs the real repo
    /// for (`ConsumerGroupRepo`/`CursorRepo`/`SubscriptionRepo`/
    /// `ActiveStreamMarker` - `Storage` implements all of them).
    pub fn repo(&self) -> &Storage {
        &self.storage
    }

    pub fn security_context(&self) -> &SecurityContext {
        &self.ctx
    }

    pub fn router(&self) -> &axum::Router {
        &self.router
    }
}

/// Builder for [`EventBrokerHarness`].
#[derive(Default)]
pub struct EventBrokerHarnessBuilder {
    type_registry: Option<StaticTypesRegistry>,
    policy_enforcer: Option<PolicyEnforcer>,
}

impl EventBrokerHarnessBuilder {
    /// Seeds `Topic`/`EventType` fixtures from a [`StaticTypesRegistry`].
    #[must_use]
    pub fn with_type_registry(mut self, registry: StaticTypesRegistry) -> Self {
        self.type_registry = Some(registry);
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

        let (backend_resolver, outbox_handle) = start_outbox(
            Arc::clone(&db),
            Arc::clone(&spec_manager),
            &storage,
            cluster.cache,
        )
        .await;

        let HandlerState { ingest, delivery } = crate::infra::wiring::build_handler_state(
            Arc::clone(&storage),
            policy_enforcer,
            spec_manager,
            backend_resolver,
            None,
        );

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

        EventBrokerHarness {
            ingest,
            delivery,
            storage,
            ctx,
            router,
            _outbox_handle: outbox_handle,
        }
    }
}
