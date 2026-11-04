//! Passes are forced, never slept for, so what these assert is what the worker
//! does rather than what a timer happened to do.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use event_broker_sdk::backend::{
    AppendRequest, Appended, Backend, BackendError, Dedup, LogClaim, Provisioning, Replacing,
    Retention, TopicPartition, TopicSpec, Wait, Watch,
};
use event_broker_sdk::{Event, PartitionRange, Position, Sequence, TopicSegment};
use serde_json::json;
use tokio_util::sync::CancellationToken;
use toolkit_gts::GtsInstanceId;
use types_registry_sdk::testing::{MockTypesRegistryClient, make_test_instance};

use super::{ProvisioningWait, SpecificationRefreshWorker};
use crate::domain::specification::{ProvisioningLimits, SpecificationManager, TopicState};
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
        manager
            .get_topic(&topic_id)
            .await
            .expect("the cache reads")
            .is_none(),
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
        .expect("the cache reads")
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
        manager
            .get_topic(&topic_id)
            .await
            .expect("the cache reads")
            .is_some(),
        "a pass that found nothing must not remove what the last one admitted"
    );
}

/// What the scripted backend answers one `provision` call with.
#[derive(Clone, Copy)]
enum Answer {
    Ready(u32),
    Pending,
    /// Never answers within any test's call timeout, standing in for a
    /// backend that is unreachable.
    Hang,
}

/// Answers `provision` from a script, the last answer repeating, and records
/// every spec and claim it was handed. Provisioning calls nothing else.
struct ScriptedBackend {
    script: Mutex<VecDeque<Answer>>,
    calls: Mutex<Vec<(TopicSpec, LogClaim)>>,
}

impl ScriptedBackend {
    fn answering(answers: impl IntoIterator<Item = Answer>) -> Self {
        Self {
            script: Mutex::new(answers.into_iter().collect()),
            calls: Mutex::new(Vec::new()),
        }
    }

    fn calls(&self) -> Vec<(TopicSpec, LogClaim)> {
        self.calls.lock().unwrap().clone()
    }
}

fn unused() -> BackendError {
    BackendError::Internal("provisioning calls only provision".to_owned())
}

#[async_trait]
impl Backend for ScriptedBackend {
    async fn provision(
        &self,
        topic: &TopicSpec,
        claim: &LogClaim,
    ) -> Result<Provisioning, BackendError> {
        self.calls.lock().unwrap().push((topic.clone(), *claim));
        let answer = {
            let mut script = self.script.lock().unwrap();
            if script.len() > 1 {
                script.pop_front()
            } else {
                script.front().copied()
            }
            .expect("the test scripted an answer")
        };
        match answer {
            Answer::Ready(partitions) => Ok(Provisioning::Ready { partitions }),
            Answer::Pending => Ok(Provisioning::Pending),
            Answer::Hang => {
                tokio::time::sleep(Duration::from_hours(1)).await;
                Err(unused())
            }
        }
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

/// A worker over one registered topic provisioned on `backend`, and a manager
/// reading the same database.
async fn provisioning_worker(
    backend: &Arc<ScriptedBackend>,
    call_timeout: Duration,
    tick: Duration,
) -> (
    SpecificationRefreshWorker,
    TypesRegistrySpecificationManager,
) {
    let db = test_db().await;
    let client: Arc<dyn types_registry_sdk::TypesRegistryClient> = Arc::new(
        MockTypesRegistryClient::new()
            .with_instances(vec![make_test_instance(TOPIC_ID, topic_document())]),
    );
    let worker = SpecificationRefreshWorker::new(
        client,
        Arc::clone(&db),
        StaticTypesRegistry::empty_config(),
        tick,
    )
    .provisioning(
        Arc::new(TypesRegistrySpecificationManager::new(Arc::clone(&db))),
        Arc::new(SingleBackendResolver::new(
            Arc::clone(backend) as Arc<dyn Backend>
        )),
        ProvisioningLimits {
            call_timeout,
            attempt_timeout: Duration::from_mins(5),
        },
    );
    (worker, TypesRegistrySpecificationManager::new(db))
}

/// A worker over one registered topic whose backend never provisions it.
async fn pending_worker() -> SpecificationRefreshWorker {
    let backend = Arc::new(ScriptedBackend::answering([Answer::Pending]));
    provisioning_worker(&backend, Duration::from_millis(100), Duration::from_mins(1))
        .await
        .0
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

    assert_eq!(
        settled,
        ProvisioningWait::Stopped,
        "a stop ends the wait before it settles"
    );
    assert!(
        waited < Duration::from_secs(2),
        "the wait ended {waited:?} after starting, the stop came after 200ms"
    );
}

/// Without a stop the wait retries until its bound leaves no room for another retry, and
/// reports what is still pending.
#[tokio::test]
async fn the_startup_provisioning_wait_reports_what_is_left_at_its_bound() {
    let worker = pending_worker().await;
    worker.run_once().await;

    // The first pass, a 250ms backoff, then one retry; the next 500ms backoff
    // no longer fits, so the wait settles after the retry. The bound cuts it
    // only if the first two passes take 500ms or more together.
    let settled = worker
        .settle_provisioning(Duration::from_millis(750), &CancellationToken::new())
        .await;

    assert_eq!(settled, ProvisioningWait::Settled { unprovisioned: 1 });
}

/// The bound is hard: a pass still waiting on an unreachable backend when it
/// passes is cut, so startup is delayed by the bound and not by the pass.
#[tokio::test]
async fn the_startup_provisioning_wait_is_bounded_while_a_pass_is_in_flight() {
    let backend = Arc::new(ScriptedBackend::answering([Answer::Hang]));
    let (worker, _) =
        provisioning_worker(&backend, Duration::from_secs(5), Duration::from_mins(1)).await;
    worker.load_once().await;

    let started = std::time::Instant::now();
    let settled = worker
        .settle_provisioning(Duration::from_millis(300), &CancellationToken::new())
        .await;
    let waited = started.elapsed();

    assert_eq!(settled, ProvisioningWait::Cut);
    assert!(
        waited < Duration::from_secs(2),
        "the wait ended {waited:?} after starting, its bound is 300ms"
    );
}

/// A shutdown ends `run` while a pass is in flight instead of after it: a
/// stop never waits out an unreachable backend.
#[tokio::test]
async fn a_shutdown_ends_a_run_pass_in_flight() {
    let backend = Arc::new(ScriptedBackend::answering([Answer::Hang]));
    let (worker, _) =
        provisioning_worker(&backend, Duration::from_secs(5), Duration::from_mins(1)).await;
    worker.load_once().await;

    let shutdown = CancellationToken::new();
    let handle = worker.spawn(shutdown.clone());
    tokio::time::sleep(Duration::from_millis(200)).await;
    shutdown.cancel();

    let joined = tokio::time::timeout(Duration::from_secs(2), handle).await;
    assert!(
        matches!(joined, Ok(Ok(()))),
        "run did not end within 2s of the shutdown: {joined:?}"
    );
}

/// `run` retries a topic its backend answers `Pending` for on the short
/// backoff, under the same log id, long before the next refresh tick.
#[tokio::test]
async fn run_retries_a_pending_topic_on_the_short_backoff() {
    let backend = Arc::new(ScriptedBackend::answering([
        Answer::Pending,
        Answer::Pending,
        Answer::Ready(4),
    ]));
    let (worker, manager) =
        provisioning_worker(&backend, Duration::from_millis(100), Duration::from_mins(1)).await;
    worker.load_once().await;
    let topic = GtsInstanceId::try_new(TOPIC_ID).expect("a valid GTS instance id");

    let shutdown = CancellationToken::new();
    let handle = worker.spawn(shutdown.clone());
    let deadline = std::time::Instant::now() + Duration::from_secs(8);
    let mut state = manager.topic_state(&topic).await.expect("state reads");
    while !matches!(state, Some(TopicState::Provisioned { .. }))
        && std::time::Instant::now() < deadline
    {
        tokio::time::sleep(Duration::from_millis(100)).await;
        state = manager.topic_state(&topic).await.expect("state reads");
    }
    shutdown.cancel();
    handle.await.expect("run ends on the shutdown");

    let calls = backend.calls();
    let &[(_, LogClaim { new: log, .. }), _, _] = calls.as_slice() else {
        panic!("expected three provision calls: {calls:?}")
    };
    assert_eq!(state, Some(TopicState::Provisioned { log, partitions: 4 }));
    let spec = TopicSpec {
        topic,
        partitions: 8,
        retention: Retention {
            max_age: Some(Duration::from_hours(7 * 24)),
            max_bytes: None,
        },
    };
    assert_eq!(
        calls,
        vec![
            (
                spec.clone(),
                LogClaim {
                    replacing: Replacing::Nothing,
                    new: log
                },
            ),
            (
                spec.clone(),
                LogClaim {
                    replacing: Replacing::Log(log),
                    new: log
                },
            ),
            (
                spec,
                LogClaim {
                    replacing: Replacing::Log(log),
                    new: log
                },
            ),
        ]
    );
}
