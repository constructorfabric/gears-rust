//! Passes are forced, never slept for, so what these assert is what the worker
//! does rather than what a timer happened to do.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use event_broker_sdk::backend::{
    AppendRequest, Appended, Backend, BackendError, Dedup, LogClaim, Provisioning, TopicPartition,
    TopicSpec, Wait, Watch,
};
use event_broker_sdk::{Event, PartitionRange, Position, Sequence, TopicSegment};
use serde_json::json;
use tokio_util::sync::CancellationToken;
use toolkit_gts::GtsInstanceId;
use types_registry_sdk::testing::{MockTypesRegistryClient, make_test_instance};

use super::SpecificationRefreshWorker;
use crate::domain::specification::{ProvisioningLimits, SpecificationManager};
use crate::infra::specification::TypesRegistrySpecificationManager;
use crate::test_support::SingleBackendResolver;
use crate::test_support::StaticTypesRegistry;

const TOPIC_ID: &str = "gts.cf.core.events.topic.v1~example.eb.refresh.topic.v1";

fn topic_document() -> serde_json::Value {
    json!({ "id": TOPIC_ID, "description": "a topic registered after startup" })
}

async fn test_db() -> Arc<toolkit_db::DBProvider<toolkit_db::DbError>> {
    use sea_orm_migration::MigratorTrait;
    let mut path = std::env::temp_dir();
    path.push(format!(
        "cf-eb-refresh-test-{}.db",
        uuid::Uuid::now_v7().simple()
    ));
    let dsn = format!("sqlite://{}?mode=rwc", path.to_string_lossy());
    let db = toolkit_db::connect_db(
        &dsn,
        toolkit_db::ConnectOpts {
            max_conns: Some(1),
            min_conns: Some(1),
            ..Default::default()
        },
    )
    .await
    .expect("connect sqlite");
    toolkit_db::migration_runner::run_migrations_for_testing(
        &db,
        crate::infra::storage::migrations::Migrator::migrations(),
    )
    .await
    .expect("migrations");
    Arc::new(toolkit_db::DBProvider::new(db))
}

/// The point of the cadence: a topic that did not exist when the process
/// started is resolvable after a pass, with no restart in between.
#[tokio::test]
async fn a_topic_registered_after_the_first_pass_resolves_after_the_next_one() {
    let db = test_db().await;
    let empty: Arc<dyn types_registry_sdk::TypesRegistryClient> =
        Arc::new(MockTypesRegistryClient::new());
    let config = StaticTypesRegistry::empty_config();
    let manager = TypesRegistrySpecificationManager::new(Arc::clone(&db));
    let topic_id = GtsInstanceId::try_new(TOPIC_ID).expect("a valid GTS instance id");

    SpecificationRefreshWorker::new(
        Arc::clone(&empty),
        Arc::clone(&db),
        config.clone(),
        Duration::from_mins(1),
    )
    .run_once()
    .await;
    assert!(
        manager.get_topic(&topic_id).await.is_none(),
        "nothing was registered when the first pass ran"
    );

    let registered: Arc<dyn types_registry_sdk::TypesRegistryClient> = Arc::new(
        MockTypesRegistryClient::new()
            .with_instances(vec![make_test_instance(TOPIC_ID, topic_document())]),
    );
    SpecificationRefreshWorker::new(registered, Arc::clone(&db), config, Duration::from_mins(1))
        .run_once()
        .await;

    let topic = manager
        .get_topic(&topic_id)
        .await
        .expect("the next pass admits it");
    assert_eq!(topic.description, "a topic registered after startup");
}

/// Two forced passes over the same registry leave the entity's identity alone -
/// the durable tables key a topic by that integer, so renumbering it would
/// silently re-point every cursor.
#[tokio::test]
async fn a_second_forced_pass_does_not_renumber_what_it_still_finds() {
    let db = test_db().await;
    let client: Arc<dyn types_registry_sdk::TypesRegistryClient> = Arc::new(
        MockTypesRegistryClient::new()
            .with_instances(vec![make_test_instance(TOPIC_ID, topic_document())]),
    );
    let worker = || {
        SpecificationRefreshWorker::new(
            Arc::clone(&client),
            Arc::clone(&db),
            StaticTypesRegistry::empty_config(),
            Duration::from_mins(1),
        )
    };
    let manager = TypesRegistrySpecificationManager::new(Arc::clone(&db));
    let topic_id = GtsInstanceId::try_new(TOPIC_ID).expect("a valid GTS instance id");

    worker().run_once().await;
    let first = manager
        .resolve_topic_id(&topic_id)
        .await
        .expect("resolves after the first pass");
    worker().run_once().await;
    let second = manager
        .resolve_topic_id(&topic_id)
        .await
        .expect("resolves after the second pass");

    assert_eq!(first, second);
}

/// A pass against an unreachable registry is not a failure that empties the
/// cache: what the last pass found still serves.
#[tokio::test]
async fn a_failing_pass_leaves_the_previous_load_in_place() {
    let db = test_db().await;
    let client: Arc<dyn types_registry_sdk::TypesRegistryClient> = Arc::new(
        MockTypesRegistryClient::new()
            .with_instances(vec![make_test_instance(TOPIC_ID, topic_document())]),
    );
    let manager = TypesRegistrySpecificationManager::new(Arc::clone(&db));
    let topic_id = GtsInstanceId::try_new(TOPIC_ID).expect("a valid GTS instance id");

    SpecificationRefreshWorker::new(
        client,
        Arc::clone(&db),
        StaticTypesRegistry::empty_config(),
        Duration::from_mins(1),
    )
    .run_once()
    .await;

    // A client that answers nothing at all, standing in for a registry that
    // has become unreachable between passes.
    let unreachable: Arc<dyn types_registry_sdk::TypesRegistryClient> =
        Arc::new(MockTypesRegistryClient::new());
    SpecificationRefreshWorker::new(
        unreachable,
        Arc::clone(&db),
        StaticTypesRegistry::empty_config(),
        Duration::from_mins(1),
    )
    .run_once()
    .await;

    assert!(
        manager.get_topic(&topic_id).await.is_some(),
        "a pass that found nothing must not remove what the last one admitted"
    );
}

/// Answers every `provision` with `Pending`, standing in for a backend still
/// starting; provisioning calls nothing else.
struct PendingBackend;

fn unused() -> BackendError {
    BackendError::Internal("provisioning calls only provision".to_owned())
}

#[async_trait]
impl Backend for PendingBackend {
    async fn provision(&self, _: &TopicSpec, _: &LogClaim) -> Result<Provisioning, BackendError> {
        Ok(Provisioning::Pending)
    }
    async fn append(&self, _: &AppendRequest<'_>) -> Result<Appended, BackendError> {
        Err(unused())
    }
    fn dedup(&self) -> Dedup {
        Dedup::Fenced
    }
    fn max_events_per_append(&self) -> usize {
        1
    }
    async fn read(
        &self,
        _: &TopicPartition,
        _: Sequence,
        _: usize,
        _: Wait,
    ) -> Result<Vec<Event>, BackendError> {
        Err(unused())
    }
    async fn resolve(&self, _: &TopicPartition, _: Position) -> Result<Sequence, BackendError> {
        Err(unused())
    }
    fn watch(&self) -> Watch<'_> {
        Watch::Poll(Duration::from_secs(1))
    }
    async fn query(
        &self,
        _: &TopicPartition,
        _: PartitionRange,
    ) -> Result<Vec<TopicSegment>, BackendError> {
        Err(unused())
    }
}

/// A worker over one registered topic whose backend never provisions it.
async fn pending_worker() -> SpecificationRefreshWorker {
    let db = test_db().await;
    let client: Arc<dyn types_registry_sdk::TypesRegistryClient> = Arc::new(
        MockTypesRegistryClient::new()
            .with_instances(vec![make_test_instance(TOPIC_ID, topic_document())]),
    );
    let backend: Arc<dyn Backend> = Arc::new(PendingBackend);
    SpecificationRefreshWorker::new(
        client,
        Arc::clone(&db),
        StaticTypesRegistry::empty_config(),
        Duration::from_mins(1),
    )
    .provisioning(
        Arc::new(TypesRegistrySpecificationManager::new(db)),
        Arc::new(SingleBackendResolver::new(backend)),
        ProvisioningLimits {
            call_timeout: Duration::from_millis(100),
            attempt_timeout: Duration::from_mins(5),
        },
    )
}

/// A stop during the startup wait ends the wait at once instead of after it:
/// the process exits promptly on a stop right after launch.
#[tokio::test]
async fn a_stop_ends_the_startup_provisioning_wait_promptly() {
    let worker = pending_worker().await;
    assert_eq!(
        worker.run_once().await,
        1,
        "the backend leaves the topic pending"
    );

    let stop = CancellationToken::new();
    let stopper = stop.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(200)).await;
        stopper.cancel();
    });
    let started = std::time::Instant::now();
    let settled = worker
        .settle_provisioning(Duration::from_secs(10), &stop)
        .await;
    let waited = started.elapsed();

    assert_eq!(settled, None, "a stop ends the wait before it settles");
    assert!(
        waited < Duration::from_secs(2),
        "the wait ended {waited:?} after starting, the stop came after 200ms"
    );
}

/// Without a stop the wait runs to its bound and reports what is still pending.
#[tokio::test]
async fn the_startup_provisioning_wait_reports_what_is_left_at_its_bound() {
    let worker = pending_worker().await;
    worker.run_once().await;

    let settled = worker
        .settle_provisioning(Duration::from_millis(300), &CancellationToken::new())
        .await;

    assert_eq!(settled, Some(1));
}
