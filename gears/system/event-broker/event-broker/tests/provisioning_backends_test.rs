//! Topic provisioning on every engine the gear database may be: an attempt
//! younger than `attempt_timeout` is continued under its log id and an older
//! one is replaced, which needs the attempt's `attempt_at` text compared with
//! the database's own clock, which each engine renders in its own SQL; and
//! two instances' retention record writes race through the engine's own
//! compare-and-set.
//!
//! The storage backend is scripted, so only the gear database varies by
//! engine. Needs a Docker daemon for `PostgreSQL` and `MySQL`:
//!
//! ```text
//! cargo test -p cf-gears-event-broker --features integration --test provisioning_backends_test
//! ```

#![cfg(feature = "integration")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use event_broker::EventBrokerConfig;
use event_broker::domain::specification::{ProvisioningLimits, SpecificationManager, TopicState};
use event_broker::infra::specification::{TypesRegistrySpecificationManager, bulk_load};
use event_broker::test_support::module_boot::migrated_gear_db;
use event_broker::test_support::{EventBrokerHarness, SingleBackendResolver, StaticTypesRegistry};
use event_broker_sdk::backend::{
    AppendRequest, Appended, Backend, BackendError, Dedup, LogClaim, Provisioning, Replacing,
    Retention, TopicPartition, TopicSpec, Wait, Watch,
};
use event_broker_sdk::{Event, PartitionRange, Position, Sequence, TopicSegment};
use serde_json::json;
use tokio::sync::Notify;
use tokio::time::timeout;
use toolkit_db::{DBProvider, DbError};
use toolkit_gts::GtsInstanceId;
use types_registry_sdk::TypesRegistryClient;
use types_registry_sdk::testing::{MockTypesRegistryClient, make_test_instance};

use support::matrix::{Engine, Matrix};

const TOPIC: &str = "gts.cf.core.events.topic.v1~x.eb.provisioning.orders.v1";
const MEMORY_BACKEND: &str = "gts.cf.core.events.backend.v1~cf.core.backend.memory.v1~";

const LIMITS: ProvisioningLimits = ProvisioningLimits {
    call_timeout: Duration::from_millis(100),
    attempt_timeout: Duration::from_secs(2),
};
/// Bounds each pass or load beyond the call timeout it may wait out, so a call
/// held on a gate that is never released fails the test instead of hanging it.
const BOUND: Duration = Duration::from_secs(10);

/// What the scripted backend answers one `provision` call with.
enum Answer {
    Ready(u32),
    /// Never answers within the call timeout.
    Hang,
    /// Recorded once the test releases the gate, then answers `Ready(4)`: the
    /// backend holds this call's spec over every call recorded while it waited.
    GatedApplyOnRelease(Arc<Gate>),
}

/// Holds a gated [`Answer`] call open: `entered` fires once the call arrives,
/// and the call answers only after `release`.
#[derive(Default)]
struct Gate {
    entered: Notify,
    release: Notify,
}

/// Answers each `provision` call with the next scripted answer and records
/// every spec and claim it was handed. Answers nothing else.
struct ScriptedBackend {
    answers: Mutex<VecDeque<Answer>>,
    calls: Mutex<Vec<(TopicSpec, LogClaim)>>,
}

impl ScriptedBackend {
    fn answering(answers: impl IntoIterator<Item = Answer>) -> Self {
        Self {
            answers: Mutex::new(answers.into_iter().collect()),
            calls: Mutex::default(),
        }
    }

    fn calls(&self) -> Vec<(TopicSpec, LogClaim)> {
        self.calls.lock().unwrap().clone()
    }

    fn claims(&self) -> Vec<LogClaim> {
        self.calls().into_iter().map(|(_, claim)| claim).collect()
    }
}

fn unused() -> BackendError {
    BackendError::Internal("provisioning calls only provision".to_owned())
}

#[async_trait::async_trait]
impl Backend for ScriptedBackend {
    async fn provision(
        &self,
        topic: &TopicSpec,
        claim: &LogClaim,
    ) -> Result<Provisioning, BackendError> {
        let answer = self
            .answers
            .lock()
            .unwrap()
            .pop_front()
            .expect("the test scripted an answer");
        if !matches!(answer, Answer::GatedApplyOnRelease(_)) {
            self.calls.lock().unwrap().push((topic.clone(), *claim));
        }
        match answer {
            Answer::Ready(partitions) => Ok(Provisioning::Ready { partitions }),
            Answer::Hang => {
                tokio::time::sleep(Duration::from_hours(1)).await;
                Err(unused())
            }
            Answer::GatedApplyOnRelease(gate) => {
                gate.entered.notify_one();
                gate.release.notified().await;
                self.calls.lock().unwrap().push((topic.clone(), *claim));
                Ok(Provisioning::Ready { partitions: 4 })
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

/// Feature 0006 §4.1: the first attempt hangs past its call timeout and is
/// left behind. A pass well inside `attempt_timeout` continues it under its
/// log id; once `attempt_timeout` has passed, the next pass replaces it under
/// a new log id, claiming the old one, and completes.
async fn a_live_attempt_is_continued_and_an_abandoned_one_replaced(engine: Engine) {
    let Some(matrix) = Matrix::start(engine, "abandoned-attempt").await else {
        return;
    };
    let backend = Arc::new(ScriptedBackend::answering([
        Answer::Hang,
        Answer::Hang,
        Answer::Ready(4),
    ]));
    let harness = EventBrokerHarness::builder()
        .with_type_registry(StaticTypesRegistry::of(json!([
            { "id": TOPIC, "partitions": 4 },
        ])))
        .with_gear_db(matrix.gear_db())
        .with_backend(Arc::clone(&backend) as Arc<dyn Backend>)
        .without_provisioning()
        .with_provisioning_limits(LIMITS)
        .build()
        .await;

    let started = Instant::now();
    timeout(LIMITS.call_timeout + BOUND, harness.provision_topics())
        .await
        .expect("the first pass ends once its call times out");
    let claims = backend.claims();
    let &[
        LogClaim {
            replacing: Replacing::Nothing,
            new: first,
        },
    ] = claims.as_slice()
    else {
        panic!("expected one fresh provision call: {claims:?}")
    };
    assert_eq!(
        harness.topic_state(TOPIC).await,
        Some(TopicState::Provisioning { log: first })
    );

    timeout(LIMITS.call_timeout + BOUND, harness.provision_topics())
        .await
        .expect("the continuing pass ends once its call times out");
    // `started` precedes the first `attempt_at`, so this bounds the attempt's
    // age at the continuing pass's compare from above.
    let elapsed = started.elapsed();
    assert!(
        elapsed < LIMITS.attempt_timeout,
        "the continuing pass must compare inside attempt_timeout: {elapsed:?}"
    );
    assert_eq!(
        backend.claims(),
        vec![
            LogClaim {
                replacing: Replacing::Nothing,
                new: first,
            },
            LogClaim {
                replacing: Replacing::Log(first),
                new: first,
            },
        ]
    );
    assert_eq!(
        harness.topic_state(TOPIC).await,
        Some(TopicState::Provisioning { log: first })
    );

    tokio::time::sleep(Duration::from_millis(2_500)).await;
    timeout(BOUND, harness.provision_topics())
        .await
        .expect("the replacing pass ends in time");

    let claims = backend.claims();
    let &[_, _, LogClaim { new: second, .. }] = claims.as_slice() else {
        panic!("expected three provision calls: {claims:?}")
    };
    assert_ne!(second, first);
    assert_eq!(
        claims,
        vec![
            LogClaim {
                replacing: Replacing::Nothing,
                new: first,
            },
            LogClaim {
                replacing: Replacing::Log(first),
                new: first,
            },
            LogClaim {
                replacing: Replacing::Log(first),
                new: second,
            },
        ]
    );
    assert_eq!(
        harness.topic_state(TOPIC).await,
        Some(TopicState::Provisioned {
            log: second,
            partitions: 4,
        })
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_live_attempt_is_continued_and_an_abandoned_one_replaced_on_sqlite() {
    a_live_attempt_is_continued_and_an_abandoned_one_replaced(Engine::Sqlite).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_live_attempt_is_continued_and_an_abandoned_one_replaced_on_postgres() {
    a_live_attempt_is_continued_and_an_abandoned_one_replaced(Engine::Postgres).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_live_attempt_is_continued_and_an_abandoned_one_replaced_on_mysql() {
    a_live_attempt_is_continued_and_an_abandoned_one_replaced(Engine::Mysql).await;
}

/// Runs the real load path over `db` with `settings` as the topic's configured
/// settings; run again, it is a load after a configuration change.
async fn load(db: &Arc<DBProvider<DbError>>, settings: serde_json::Value) {
    let config: EventBrokerConfig = serde_json::from_value(json!({
        "mode": "standalone",
        "backends": { "main": { "type": MEMORY_BACKEND } },
        "backend_routing": { "gts.cf.core.events.topic.v1~": "main" },
        "topics": { "gts.cf.core.events.topic.v1~": settings },
    }))
    .expect("the test configuration deserializes");
    let client: Arc<dyn TypesRegistryClient> = Arc::new(
        MockTypesRegistryClient::new().with_instances([make_test_instance(
            TOPIC,
            json!({ "id": TOPIC, "description": "a topic to provision" }),
        )]),
    );
    bulk_load(&client, db, &config).await.expect("bulk_load");
}

/// Feature 0006 §4.1: a re-provision that loses its compare-and-set marks the
/// record unconfirmed, because the backend may have applied its call after
/// the one recorded. Two instances, each on a pool of its own over one gear
/// database: one instance's 2d call is held while the other records 3d, and
/// the backend applies the 2d call last. Once the configurations agree on 3d,
/// it is handed again, and then no call. No SQLite lane: each open of a
/// SQLite `GearDb` is a fresh file, and the unit tests cover SQLite.
async fn a_lost_retention_record_is_provisioned_again_when_its_call_was_applied_last(
    engine: Engine,
) {
    let Some(matrix) = Matrix::start(engine, "lost-retention-record").await else {
        return;
    };
    let gear_db = matrix.gear_db();
    let (one_db, two_db) = (
        Arc::new(migrated_gear_db(&gear_db).await),
        Arc::new(migrated_gear_db(&gear_db).await),
    );
    load(
        &one_db,
        json!({ "partitions": 4, "retention": { "duration": "1d" } }),
    )
    .await;
    let gate = Arc::new(Gate::default());
    let backend = Arc::new(ScriptedBackend::answering([
        Answer::Ready(4),
        Answer::GatedApplyOnRelease(Arc::clone(&gate)),
        Answer::Ready(4),
        Answer::Ready(4),
    ]));
    let through = SingleBackendResolver::new(Arc::clone(&backend) as Arc<dyn Backend>);
    let held = ProvisioningLimits {
        call_timeout: Duration::from_mins(5),
        attempt_timeout: Duration::from_mins(5),
    };
    let one = TypesRegistrySpecificationManager::new(Arc::clone(&one_db));
    let two = TypesRegistrySpecificationManager::new(Arc::clone(&two_db));
    timeout(BOUND, one.provision_topics(&through, held))
        .await
        .expect("the first provisioning pass ends in time");

    timeout(
        BOUND,
        load(
            &one_db,
            json!({ "partitions": 4, "retention": { "duration": "2d" } }),
        ),
    )
    .await
    .expect("the 2d load ends in time");
    let held_pass = one.provision_topics(&through, held);
    let pass_while_held = async {
        gate.entered.notified().await;
        load(
            &two_db,
            json!({ "partitions": 4, "retention": { "duration": "3d" } }),
        )
        .await;
        two.provision_topics(&through, held).await;
        gate.release.notify_one();
    };
    timeout(BOUND, async { tokio::join!(held_pass, pass_while_held) })
        .await
        .expect("the held pass and the pass while it is held resolve");

    load(
        &one_db,
        json!({ "partitions": 4, "retention": { "duration": "3d" } }),
    )
    .await;
    timeout(BOUND, one.provision_topics(&through, held))
        .await
        .expect("the re-provisioning pass ends in time");
    timeout(BOUND, one.provision_topics(&through, held))
        .await
        .expect("the converged pass ends in time");

    let calls = backend.calls();
    let &[(_, LogClaim { new: log, .. }), ..] = calls.as_slice() else {
        panic!("expected provision calls: {calls:?}")
    };
    let call = |hours: u64, replacing: Replacing| {
        (
            TopicSpec {
                topic: GtsInstanceId::try_new(TOPIC).unwrap(),
                partitions: 4,
                retention: Retention {
                    max_age: Some(Duration::from_hours(hours)),
                    max_bytes: None,
                },
            },
            LogClaim {
                replacing,
                new: log,
            },
        )
    };
    assert_eq!(
        calls,
        vec![
            call(24, Replacing::Nothing),
            call(72, Replacing::Held),
            // Read first, applied last: its record loses.
            call(48, Replacing::Held),
            call(72, Replacing::Held),
        ]
    );
    assert_eq!(
        one.topic_state(&GtsInstanceId::try_new(TOPIC).unwrap())
            .await
            .expect("the topic binding reads"),
        Some(TopicState::Provisioned { log, partitions: 4 })
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_lost_retention_record_is_provisioned_again_when_its_call_was_applied_last_on_postgres() {
    a_lost_retention_record_is_provisioned_again_when_its_call_was_applied_last(Engine::Postgres)
        .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_lost_retention_record_is_provisioned_again_when_its_call_was_applied_last_on_mysql() {
    a_lost_retention_record_is_provisioned_again_when_its_call_was_applied_last(Engine::Mysql)
        .await;
}
