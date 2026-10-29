//! The provisioning state machine (feature 0006 §4.1), driven against a
//! scripted backend so every answer a backend can give is reachable.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use event_broker_sdk::backend::{
    AppendRequest, Appended, Backend, BackendError, Dedup, LogClaim, Provisioning, Replacing,
    TopicPartition, TopicSpec, Wait, Watch,
};
use event_broker_sdk::{Event, PartitionRange, Position, Sequence, TopicSegment};
use serde_json::json;
use toolkit_db::migration_runner::run_migrations_for_testing;
use toolkit_db::{ConnectOpts, DBProvider, connect_db};
use toolkit_gts::GtsInstanceId;
use types_registry_sdk::testing::{MockTypesRegistryClient, make_test_instance};
use uuid::Uuid;

use super::super::TypesRegistrySpecificationManager;
use crate::config::EventBrokerConfig;
use crate::domain::specification::{ProvisioningLimits, SpecificationManager, TopicState};
use crate::infra::storage::migrations::Migrator;
use crate::test_support::SingleBackendResolver;
use sea_orm_migration::MigratorTrait;

const TOPIC_A: &str = "gts.cf.core.events.topic.v1~example.eb.prov.a.v1";
const TOPIC_B: &str = "gts.cf.core.events.topic.v1~example.eb.prov.b.v1";
const MEMORY_BACKEND: &str = "gts.cf.core.events.backend.v1~cf.core.backend.memory.v1~";

/// What the scripted backend answers one `provision` call with.
#[derive(Clone)]
enum Answer {
    Ready(u32),
    Pending,
    Conflict,
    /// Never answers within any test's call timeout.
    Hang,
}

/// Answers `provision` from a script per topic and records every claim it was
/// handed. Nothing else is called by provisioning.
#[derive(Default)]
struct ScriptedBackend {
    script: Mutex<Vec<(String, VecDeque<Answer>)>>,
    claims: Mutex<Vec<(String, LogClaim)>>,
}

impl ScriptedBackend {
    fn answering(topic: &str, answers: impl IntoIterator<Item = Answer>) -> Self {
        let backend = Self::default();
        backend.also(topic, answers);
        backend
    }

    fn also(&self, topic: &str, answers: impl IntoIterator<Item = Answer>) {
        self.script
            .lock()
            .unwrap()
            .push((topic.to_owned(), answers.into_iter().collect()));
    }

    fn claims(&self, topic: &str) -> Vec<LogClaim> {
        self.claims
            .lock()
            .unwrap()
            .iter()
            .filter(|(t, _)| t == topic)
            .map(|(_, claim)| *claim)
            .collect()
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
        let name = topic.topic.to_string();
        self.claims.lock().unwrap().push((name.clone(), *claim));
        let answer = {
            let mut script = self.script.lock().unwrap();
            let (_, answers) = script
                .iter_mut()
                .find(|(t, _)| *t == name)
                .expect("the test scripted this topic");
            if answers.len() > 1 {
                answers.pop_front()
            } else {
                answers.front().cloned()
            }
            .expect("the test scripted an answer")
        };
        match answer {
            Answer::Ready(partitions) => Ok(Provisioning::Ready { partitions }),
            Answer::Pending => Ok(Provisioning::Pending),
            Answer::Conflict => Err(BackendError::log_conflict("provision")),
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

fn limits(attempt_timeout: Duration) -> ProvisioningLimits {
    ProvisioningLimits {
        call_timeout: Duration::from_millis(100),
        attempt_timeout,
    }
}

const LONG: Duration = Duration::from_mins(5);

async fn test_db() -> Arc<DBProvider<toolkit_db::DbError>> {
    let mut path = std::env::temp_dir();
    path.push(format!(
        "cf-eb-provisioning-test-{}.db",
        Uuid::now_v7().simple()
    ));
    let dsn = format!(
        "sqlite://{}?mode=rwc",
        path.to_string_lossy().replace('\\', "/")
    );
    let opts = ConnectOpts {
        max_conns: Some(1),
        min_conns: Some(1),
        ..Default::default()
    };
    let db = connect_db(&dsn, opts).await.expect("connect sqlite");
    run_migrations_for_testing(&db, Migrator::migrations())
        .await
        .expect("migrations");
    Arc::new(DBProvider::new(db))
}

/// The topics, loaded through the real load path with 4 partitions each, into
/// a fresh database; a second manager over the same database is a second
/// instance.
async fn loaded(topics: &[&str]) -> Arc<DBProvider<toolkit_db::DbError>> {
    let cfg: EventBrokerConfig = serde_json::from_value(json!({
        "mode": "standalone",
        "backends": { "main": { "type": MEMORY_BACKEND } },
        "backend_routing": { "gts.cf.core.events.topic.v1~": "main" },
        "topics": { "gts.cf.core.events.topic.v1~": { "partitions": 4 } },
    }))
    .expect("test configuration deserializes");
    let client: Arc<dyn types_registry_sdk::TypesRegistryClient> = Arc::new(
        MockTypesRegistryClient::new().with_instances(topics.iter().map(|id| {
            make_test_instance(
                id,
                json!({ "id": id, "description": "a topic to provision" }),
            )
        })),
    );
    let db = test_db().await;
    super::super::bulk_load(&client, &db, &cfg)
        .await
        .expect("bulk_load");
    db
}

fn instance(db: &Arc<DBProvider<toolkit_db::DbError>>) -> TypesRegistrySpecificationManager {
    TypesRegistrySpecificationManager::new(Arc::clone(db))
}

fn resolver(backend: &Arc<ScriptedBackend>) -> SingleBackendResolver {
    SingleBackendResolver::new(Arc::clone(backend) as Arc<dyn Backend>)
}

async fn state(manager: &TypesRegistrySpecificationManager, topic: &str) -> Option<TopicState> {
    manager
        .topic_state(&GtsInstanceId::try_new(topic).unwrap())
        .await
        .expect("state reads")
}

/// AC-1: two instances seeing a new topic provision it once, under one log
/// id, with the partition count the backend reports.
#[tokio::test]
async fn a_new_topic_is_provisioned_once_under_one_log_id() {
    let db = loaded(&[TOPIC_A]).await;
    let backend = Arc::new(ScriptedBackend::answering(TOPIC_A, [Answer::Ready(4)]));
    let (one, two) = (instance(&db), instance(&db));

    one.provision_topics(&resolver(&backend), limits(LONG))
        .await;
    two.provision_topics(&resolver(&backend), limits(LONG))
        .await;

    let claims = backend.claims(TOPIC_A);
    let log = claims[0].new;
    assert_eq!(
        claims,
        vec![
            LogClaim {
                replacing: Replacing::Nothing,
                new: log
            },
            // The second instance finds it provisioned and re-hands its settings.
            LogClaim {
                replacing: Replacing::Log(log),
                new: log
            },
        ]
    );
    assert_eq!(
        state(&two, TOPIC_A).await,
        Some(TopicState::Provisioned { log, partitions: 4 })
    );
}

/// AC-2: an attempt nobody acted on for `attempt_timeout` is replaced under a
/// new log id, claiming the old one, and completes.
#[tokio::test]
async fn an_abandoned_attempt_is_replaced_under_a_new_log_id() {
    let db = loaded(&[TOPIC_A]).await;
    let backend = Arc::new(ScriptedBackend::answering(
        TOPIC_A,
        [Answer::Hang, Answer::Ready(4)],
    ));
    let manager = instance(&db);

    manager
        .provision_topics(&resolver(&backend), limits(LONG))
        .await;
    let first = backend.claims(TOPIC_A)[0].new;
    assert_eq!(
        state(&manager, TOPIC_A).await,
        Some(TopicState::Provisioning { log: first })
    );

    tokio::time::sleep(Duration::from_millis(20)).await;
    manager
        .provision_topics(&resolver(&backend), limits(Duration::ZERO))
        .await;

    let claims = backend.claims(TOPIC_A);
    let replacement = claims[1].new;
    assert_ne!(replacement, first);
    assert_eq!(
        claims,
        vec![
            LogClaim {
                replacing: Replacing::Nothing,
                new: first
            },
            LogClaim {
                replacing: Replacing::Log(first),
                new: replacement
            },
        ]
    );
    assert_eq!(
        state(&manager, TOPIC_A).await,
        Some(TopicState::Provisioned {
            log: replacement,
            partitions: 4
        })
    );
}

/// AC-3: a backend answering `Pending` is called again with the same log id
/// until it answers `Ready`.
#[tokio::test]
async fn pending_is_retried_under_the_same_log_id() {
    let db = loaded(&[TOPIC_A]).await;
    let backend = Arc::new(ScriptedBackend::answering(
        TOPIC_A,
        [Answer::Pending, Answer::Ready(4)],
    ));
    let manager = instance(&db);

    manager
        .provision_topics(&resolver(&backend), limits(LONG))
        .await;
    let log = backend.claims(TOPIC_A)[0].new;
    assert_eq!(
        state(&manager, TOPIC_A).await,
        Some(TopicState::Provisioning { log })
    );

    manager
        .provision_topics(&resolver(&backend), limits(LONG))
        .await;

    assert_eq!(
        backend.claims(TOPIC_A),
        vec![
            LogClaim {
                replacing: Replacing::Nothing,
                new: log
            },
            LogClaim {
                replacing: Replacing::Log(log),
                new: log
            },
        ]
    );
    assert_eq!(
        state(&manager, TOPIC_A).await,
        Some(TopicState::Provisioned { log, partitions: 4 })
    );
}

/// AC-5: a backend holding a log id no attempt expects refuses the attempt;
/// the topic stays provisioning under the same log id, pass after pass.
#[tokio::test]
async fn a_log_conflict_keeps_the_attempt_and_its_log_id() {
    let db = loaded(&[TOPIC_A]).await;
    let backend = Arc::new(ScriptedBackend::answering(TOPIC_A, [Answer::Conflict]));
    let manager = instance(&db);

    manager
        .provision_topics(&resolver(&backend), limits(LONG))
        .await;
    manager
        .provision_topics(&resolver(&backend), limits(LONG))
        .await;

    let log = backend.claims(TOPIC_A)[0].new;
    assert_eq!(
        backend.claims(TOPIC_A),
        vec![
            LogClaim {
                replacing: Replacing::Nothing,
                new: log
            },
            LogClaim {
                replacing: Replacing::Log(log),
                new: log
            },
        ]
    );
    assert_eq!(
        state(&manager, TOPIC_A).await,
        Some(TopicState::Provisioning { log })
    );
}

/// AC-6: a backend that never answers for one topic does not keep another
/// from being provisioned in the same pass.
#[tokio::test]
async fn a_hanging_topic_does_not_hold_up_another() {
    let db = loaded(&[TOPIC_A, TOPIC_B]).await;
    let backend = Arc::new(ScriptedBackend::answering(TOPIC_A, [Answer::Hang]));
    backend.also(TOPIC_B, [Answer::Ready(4)]);
    let manager = instance(&db);

    manager
        .provision_topics(&resolver(&backend), limits(LONG))
        .await;

    let a = backend.claims(TOPIC_A)[0].new;
    let b = backend.claims(TOPIC_B)[0].new;
    assert_eq!(
        state(&manager, TOPIC_A).await,
        Some(TopicState::Provisioning { log: a })
    );
    assert_eq!(
        state(&manager, TOPIC_B).await,
        Some(TopicState::Provisioned {
            log: b,
            partitions: 4
        })
    );
}

/// The backend's partition count is the one in force, whatever configuration
/// says: a topic configured with 4 but held with 2 resolves with 2.
#[tokio::test]
async fn the_backends_partition_count_is_in_force() {
    let db = loaded(&[TOPIC_A]).await;
    let backend = Arc::new(ScriptedBackend::answering(TOPIC_A, [Answer::Ready(2)]));
    let manager = instance(&db);

    manager
        .provision_topics(&resolver(&backend), limits(LONG))
        .await;

    let topic = manager
        .get_topic(&GtsInstanceId::try_new(TOPIC_A).unwrap())
        .await
        .expect("registered");
    assert_eq!(*topic.settings.partitions().value(), 2);
    assert_eq!(
        topic.settings.partitions().source(),
        crate::domain::resolution::Source::Backend
    );
}

/// A topic with no record has no state: registered, nothing attempted.
#[tokio::test]
async fn a_topic_never_attempted_has_no_state() {
    let db = loaded(&[TOPIC_A]).await;
    assert_eq!(state(&instance(&db), TOPIC_A).await, None);
}
