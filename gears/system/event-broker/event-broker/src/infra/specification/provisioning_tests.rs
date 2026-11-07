//! The provisioning state machine (feature 0006 §4.1), driven against a
//! scripted backend so every answer a backend can give is reachable.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use event_broker_sdk::backend::{
    AppendRequest, Appended, Backend, BackendError, Dedup, LogClaim, Provisioning, Replacing,
    Retention, TopicPartition, TopicSpec, Wait, Watch,
};
use event_broker_sdk::models::Event;
use event_broker_sdk::{PartitionRange, Position, Sequence, TopicSegment};
use serde_json::json;
use tokio::sync::Notify;
use toolkit_db::migration_runner::run_migrations_for_testing;
use toolkit_db::{ConnectOpts, DBProvider, connect_db};
use toolkit_gts::GtsInstanceId;
use uuid::Uuid;

use super::super::TypesRegistrySpecificationManager;
use crate::config::EventBrokerConfig;
use crate::domain::error::DomainError;
use crate::domain::specification::{ProvisioningLimits, SpecificationManager, TopicState};
use crate::infra::storage::entity::topic_binding;
use crate::infra::storage::migrations::Migrator;
use crate::test_support::SingleBackendResolver;
use crate::test_support::type_registry::registry_holding;
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
    /// Recorded, then answers `Ready(4)` once the test releases the gate.
    Gated(Arc<Gate>),
    /// Recorded once the test releases the gate, then answers `Ready(4)`: the
    /// backend holds this call's spec over every call recorded while it waited.
    GatedApplyOnRelease(Arc<Gate>),
    /// Recorded once the test releases the gate, then answers `Pending`: the
    /// backend may still apply this call's spec over every call recorded
    /// while it waited.
    GatedPendingOnRelease(Arc<Gate>),
    /// Recorded once the test releases the gate, then fails: the backend may
    /// have applied this call's spec over every call recorded while it waited.
    GatedFailOnRelease(Arc<Gate>),
}

/// Holds a gated [`Answer`] call open: `entered` fires once the call arrives,
/// and the call answers only after `release`.
#[derive(Default)]
struct Gate {
    entered: Notify,
    release: Notify,
}

/// Answers `provision` from a script per topic and records every spec and
/// claim it was handed. Nothing else is called by provisioning.
#[derive(Default)]
struct ScriptedBackend {
    script: Mutex<Vec<(String, VecDeque<Answer>)>>,
    calls: Mutex<Vec<(TopicSpec, LogClaim)>>,
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

    fn calls(&self, topic: &str) -> Vec<(TopicSpec, LogClaim)> {
        self.calls
            .lock()
            .unwrap()
            .iter()
            .filter(|(spec, _)| spec.topic.as_ref() == topic)
            .cloned()
            .collect()
    }

    fn claims(&self, topic: &str) -> Vec<LogClaim> {
        self.calls(topic)
            .into_iter()
            .map(|(_, claim)| claim)
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
        if !matches!(
            answer,
            Answer::GatedApplyOnRelease(_)
                | Answer::GatedPendingOnRelease(_)
                | Answer::GatedFailOnRelease(_)
        ) {
            self.calls.lock().unwrap().push((topic.clone(), *claim));
        }
        match answer {
            Answer::Ready(partitions) => Ok(Provisioning::Ready { partitions }),
            Answer::Pending => Ok(Provisioning::Pending),
            Answer::Conflict => Err(BackendError::log_conflict("provision")),
            Answer::Hang => {
                tokio::time::sleep(Duration::from_hours(1)).await;
                Err(unused())
            }
            Answer::Gated(gate) => {
                gate.entered.notify_one();
                gate.release.notified().await;
                Ok(Provisioning::Ready { partitions: 4 })
            }
            Answer::GatedApplyOnRelease(gate) => {
                gate.entered.notify_one();
                gate.release.notified().await;
                self.calls.lock().unwrap().push((topic.clone(), *claim));
                Ok(Provisioning::Ready { partitions: 4 })
            }
            Answer::GatedPendingOnRelease(gate) => {
                gate.entered.notify_one();
                gate.release.notified().await;
                self.calls.lock().unwrap().push((topic.clone(), *claim));
                Ok(Provisioning::Pending)
            }
            Answer::GatedFailOnRelease(gate) => {
                gate.entered.notify_one();
                gate.release.notified().await;
                self.calls.lock().unwrap().push((topic.clone(), *claim));
                Err(BackendError::Internal("the answer was lost".to_owned()))
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
    test_db_with_dsn().await.0
}

/// [`test_db`], with a DSN that reaches the same file.
async fn test_db_with_dsn() -> (Arc<DBProvider<toolkit_db::DbError>>, String) {
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
    (Arc::new(DBProvider::new(db)), dsn)
}

/// The topics, loaded through the real load path with 4 partitions each, into
/// a fresh database; a second manager over the same database is a second
/// instance.
async fn loaded(topics: &[&str]) -> Arc<DBProvider<toolkit_db::DbError>> {
    let db = test_db().await;
    load(&db, topics, json!({ "partitions": 4 })).await;
    db
}

/// Runs the real load path over `db` with `settings` as every topic's
/// configured settings; run again, it is a load after a configuration change.
async fn load(
    db: &Arc<DBProvider<toolkit_db::DbError>>,
    topics: &[&str],
    settings: serde_json::Value,
) {
    let cfg: EventBrokerConfig = serde_json::from_value(json!({
        "mode": "standalone",
        "backends": { "main": { "type": MEMORY_BACKEND } },
        "backend_routing": { "gts.cf.core.events.topic.v1~": "main" },
        "topics": { "gts.cf.core.events.topic.v1~": settings },
    }))
    .expect("test configuration deserializes");
    let documents: Vec<serde_json::Value> = topics
        .iter()
        .map(|id| json!({ "id": id, "description": "a topic to provision" }))
        .collect();
    let client: Arc<dyn types_registry_sdk::PlatformTypesRegistryApi> =
        registry_holding(&documents, &[]);
    super::super::bulk_load(&client, db, &cfg)
        .await
        .expect("bulk_load");
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
/// id, with the partition count the backend reports; the second instance
/// finds it provisioned with unchanged retention and makes no call (§3.1).
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
    let &[LogClaim { new: log, .. }] = claims.as_slice() else {
        panic!("expected one provision call: {claims:?}")
    };
    assert_eq!(
        claims,
        vec![LogClaim {
            replacing: Replacing::Nothing,
            new: log
        }]
    );
    assert_eq!(
        state(&two, TOPIC_A).await,
        Some(TopicState::Provisioned { log, partitions: 4 })
    );
}

/// S2: a provisioned topic whose retention is unchanged is not provisioned
/// again - not by a restart, not by a second instance, not on a later pass.
#[tokio::test]
async fn a_provisioned_topic_is_not_provisioned_again_while_its_retention_is_unchanged() {
    let db = loaded(&[TOPIC_A]).await;
    let backend = Arc::new(ScriptedBackend::answering(TOPIC_A, [Answer::Ready(4)]));
    let (one, two) = (instance(&db), instance(&db));

    one.provision_topics(&resolver(&backend), limits(LONG))
        .await;
    two.provision_topics(&resolver(&backend), limits(LONG))
        .await;
    two.provision_topics(&resolver(&backend), limits(LONG))
        .await;

    let calls = backend.calls(TOPIC_A);
    let &[(_, LogClaim { new: log, .. })] = calls.as_slice() else {
        panic!("expected one provision call: {calls:?}")
    };
    assert_eq!(
        backend.calls(TOPIC_A),
        vec![(
            TopicSpec {
                topic: GtsInstanceId::try_new(TOPIC_A).unwrap(),
                partitions: 4,
                retention: Retention {
                    max_age: Some(Duration::from_hours(24 * 7)),
                    max_bytes: None,
                },
            },
            LogClaim {
                replacing: Replacing::Nothing,
                new: log
            },
        )]
    );
    assert_eq!(
        state(&two, TOPIC_A).await,
        Some(TopicState::Provisioned { log, partitions: 4 })
    );
}

/// 0007 §2.6: a changed retention is handed to the backend once, under the
/// same log id; later passes find it recorded and make no call.
#[tokio::test]
async fn a_retention_change_re_provisions_once_under_the_same_log_id() {
    let db = test_db().await;
    load(
        &db,
        &[TOPIC_A],
        json!({ "partitions": 4, "retention": { "duration": "1d" } }),
    )
    .await;
    let backend = Arc::new(ScriptedBackend::answering(TOPIC_A, [Answer::Ready(4)]));
    let manager = instance(&db);
    manager
        .provision_topics(&resolver(&backend), limits(LONG))
        .await;

    load(
        &db,
        &[TOPIC_A],
        json!({ "partitions": 4, "retention": { "duration": "2d", "size_bytes": 4096 } }),
    )
    .await;
    manager
        .provision_topics(&resolver(&backend), limits(LONG))
        .await;
    manager
        .provision_topics(&resolver(&backend), limits(LONG))
        .await;

    let calls = backend.calls(TOPIC_A);
    let &[(_, LogClaim { new: log, .. }), _] = calls.as_slice() else {
        panic!("expected two provision calls: {calls:?}")
    };
    let topic = GtsInstanceId::try_new(TOPIC_A).unwrap();
    assert_eq!(
        backend.calls(TOPIC_A),
        vec![
            (
                TopicSpec {
                    topic: topic.clone(),
                    partitions: 4,
                    retention: Retention {
                        max_age: Some(Duration::from_hours(24)),
                        max_bytes: None,
                    },
                },
                LogClaim {
                    replacing: Replacing::Nothing,
                    new: log
                },
            ),
            (
                TopicSpec {
                    topic,
                    partitions: 4,
                    retention: Retention {
                        max_age: Some(Duration::from_hours(48)),
                        max_bytes: Some(4096),
                    },
                },
                LogClaim {
                    replacing: Replacing::Held,
                    new: log
                },
            ),
        ]
    );
    assert_eq!(
        state(&manager, TOPIC_A).await,
        Some(TopicState::Provisioned { log, partitions: 4 })
    );
}

/// A re-provision that gets no answer leaves the recorded retention as it
/// was, so the next pass calls again; once `Ready`, no further call.
#[tokio::test]
async fn a_failed_re_provision_is_retried_on_the_next_pass() {
    let db = test_db().await;
    load(
        &db,
        &[TOPIC_A],
        json!({ "partitions": 4, "retention": { "duration": "1d" } }),
    )
    .await;
    let backend = Arc::new(ScriptedBackend::answering(
        TOPIC_A,
        [Answer::Ready(4), Answer::Hang, Answer::Ready(4)],
    ));
    let manager = instance(&db);
    manager
        .provision_topics(&resolver(&backend), limits(LONG))
        .await;

    load(
        &db,
        &[TOPIC_A],
        json!({ "partitions": 4, "retention": { "duration": "2d" } }),
    )
    .await;
    for _ in 0..3 {
        manager
            .provision_topics(&resolver(&backend), limits(LONG))
            .await;
    }

    let calls = backend.calls(TOPIC_A);
    let &[(_, LogClaim { new: log, .. }), _, _] = calls.as_slice() else {
        panic!("expected three provision calls: {calls:?}")
    };
    let topic = GtsInstanceId::try_new(TOPIC_A).unwrap();
    assert_eq!(
        backend.calls(TOPIC_A),
        vec![
            (
                TopicSpec {
                    topic: topic.clone(),
                    partitions: 4,
                    retention: Retention {
                        max_age: Some(Duration::from_hours(24)),
                        max_bytes: None,
                    },
                },
                LogClaim {
                    replacing: Replacing::Nothing,
                    new: log
                },
            ),
            // Hangs past the call timeout.
            (
                TopicSpec {
                    topic: topic.clone(),
                    partitions: 4,
                    retention: Retention {
                        max_age: Some(Duration::from_hours(48)),
                        max_bytes: None,
                    },
                },
                LogClaim {
                    replacing: Replacing::Held,
                    new: log
                },
            ),
            (
                TopicSpec {
                    topic,
                    partitions: 4,
                    retention: Retention {
                        max_age: Some(Duration::from_hours(48)),
                        max_bytes: None,
                    },
                },
                LogClaim {
                    replacing: Replacing::Held,
                    new: log
                },
            ),
        ]
    );
    assert_eq!(
        state(&manager, TOPIC_A).await,
        Some(TopicState::Provisioned { log, partitions: 4 })
    );
}

/// Bounds a gated pass together with the pass run while it is held, so a
/// call that never reaches the gate, or a gate never released, fails the test
/// instead of hanging it.
const BOUND: Duration = Duration::from_secs(10);

/// 0006 §4.1: two instances resolving different retention during a rollout
/// record their re-provisions out of order: the instance that read the record
/// first records last. Once their configurations agree, the agreed retention
/// reaches the backend again, and then no call.
#[tokio::test]
async fn instances_that_record_retention_out_of_order_converge_once_their_configs_agree() {
    let db = test_db().await;
    load(
        &db,
        &[TOPIC_A],
        json!({ "partitions": 4, "retention": { "duration": "1d" } }),
    )
    .await;
    let gate = Arc::new(Gate::default());
    let backend = Arc::new(ScriptedBackend::answering(
        TOPIC_A,
        [
            Answer::Ready(4),
            Answer::Gated(Arc::clone(&gate)),
            Answer::Ready(4),
        ],
    ));
    let through = resolver(&backend);
    let held = ProvisioningLimits {
        call_timeout: LONG,
        attempt_timeout: LONG,
    };
    let (one, two) = (instance(&db), instance(&db));
    one.provision_topics(&through, held).await;

    load(
        &db,
        &[TOPIC_A],
        json!({ "partitions": 4, "retention": { "duration": "2d" } }),
    )
    .await;
    let held_pass = one.provision_topics(&through, held);
    let pass_while_held = async {
        gate.entered.notified().await;
        load(
            &db,
            &[TOPIC_A],
            json!({ "partitions": 4, "retention": { "duration": "3d" } }),
        )
        .await;
        two.provision_topics(&through, held).await;
        gate.release.notify_one();
    };
    tokio::time::timeout(BOUND, async { tokio::join!(held_pass, pass_while_held) })
        .await
        .expect("the held pass and the pass while it is held resolve");

    load(
        &db,
        &[TOPIC_A],
        json!({ "partitions": 4, "retention": { "duration": "2d" } }),
    )
    .await;
    one.provision_topics(&through, held).await;
    one.provision_topics(&through, held).await;

    let calls = backend.calls(TOPIC_A);
    let &[(_, LogClaim { new: log, .. }), ..] = calls.as_slice() else {
        panic!("expected provision calls: {calls:?}")
    };
    let call = |hours: u64, replacing: Replacing| {
        (
            TopicSpec {
                topic: GtsInstanceId::try_new(TOPIC_A).unwrap(),
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
            // Gated until the other instance has handed 3d and recorded it.
            call(48, Replacing::Held),
            call(72, Replacing::Held),
            call(48, Replacing::Held),
        ]
    );
    assert_eq!(
        state(&one, TOPIC_A).await,
        Some(TopicState::Provisioned { log, partitions: 4 })
    );
}

/// 0006 §4.1: a re-provision whose answer is not seen may still be applied by the
/// backend, so the record is not trusted to hold what the backend holds: when
/// the configuration returns to the recorded retention, it is handed again.
#[tokio::test]
async fn a_re_provision_whose_answer_is_not_seen_is_provisioned_again_once_the_config_returns() {
    let db = test_db().await;
    load(
        &db,
        &[TOPIC_A],
        json!({ "partitions": 4, "retention": { "duration": "1d" } }),
    )
    .await;
    let backend = Arc::new(ScriptedBackend::answering(
        TOPIC_A,
        [Answer::Ready(4), Answer::Hang, Answer::Ready(4)],
    ));
    let manager = instance(&db);
    manager
        .provision_topics(&resolver(&backend), limits(LONG))
        .await;

    load(
        &db,
        &[TOPIC_A],
        json!({ "partitions": 4, "retention": { "duration": "2d" } }),
    )
    .await;
    manager
        .provision_topics(&resolver(&backend), limits(LONG))
        .await;
    load(
        &db,
        &[TOPIC_A],
        json!({ "partitions": 4, "retention": { "duration": "1d" } }),
    )
    .await;
    manager
        .provision_topics(&resolver(&backend), limits(LONG))
        .await;
    manager
        .provision_topics(&resolver(&backend), limits(LONG))
        .await;

    let calls = backend.calls(TOPIC_A);
    let &[(_, LogClaim { new: log, .. }), ..] = calls.as_slice() else {
        panic!("expected provision calls: {calls:?}")
    };
    let call = |hours: u64, replacing: Replacing| {
        (
            TopicSpec {
                topic: GtsInstanceId::try_new(TOPIC_A).unwrap(),
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
            // Hangs past the call timeout.
            call(48, Replacing::Held),
            call(24, Replacing::Held),
        ]
    );
    assert_eq!(
        state(&manager, TOPIC_A).await,
        Some(TopicState::Provisioned { log, partitions: 4 })
    );
}

/// 0006 §4.1: a re-provision dropped mid-call (a shutdown, the start-up wait
/// cut, a crash) may still be applied by the backend, so the record is not
/// trusted to hold what the backend holds: when the configuration returns to
/// the recorded retention, it is handed again.
#[tokio::test]
async fn a_re_provision_dropped_mid_call_is_provisioned_again_once_the_config_returns() {
    let db = test_db().await;
    load(
        &db,
        &[TOPIC_A],
        json!({ "partitions": 4, "retention": { "duration": "1d" } }),
    )
    .await;
    let gate = Arc::new(Gate::default());
    let backend = Arc::new(ScriptedBackend::answering(
        TOPIC_A,
        [
            Answer::Ready(4),
            Answer::Gated(Arc::clone(&gate)),
            Answer::Ready(4),
        ],
    ));
    let through = resolver(&backend);
    let held = ProvisioningLimits {
        call_timeout: LONG,
        attempt_timeout: LONG,
    };
    let manager = instance(&db);
    manager.provision_topics(&through, held).await;

    load(
        &db,
        &[TOPIC_A],
        json!({ "partitions": 4, "retention": { "duration": "2d" } }),
    )
    .await;
    tokio::time::timeout(BOUND, async {
        tokio::select! {
            _ = manager.provision_topics(&through, held) => {
                panic!("the gated pass resolved without a release");
            }
            () = gate.entered.notified() => {}
        }
    })
    .await
    .expect("the pass reaches the gate");

    load(
        &db,
        &[TOPIC_A],
        json!({ "partitions": 4, "retention": { "duration": "1d" } }),
    )
    .await;
    manager.provision_topics(&through, held).await;
    manager.provision_topics(&through, held).await;

    let calls = backend.calls(TOPIC_A);
    let &[(_, LogClaim { new: log, .. }), ..] = calls.as_slice() else {
        panic!("expected provision calls: {calls:?}")
    };
    let call = |hours: u64, replacing: Replacing| {
        (
            TopicSpec {
                topic: GtsInstanceId::try_new(TOPIC_A).unwrap(),
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
            // Dropped while the backend holds it.
            call(48, Replacing::Held),
            call(24, Replacing::Held),
        ]
    );
    assert_eq!(
        state(&manager, TOPIC_A).await,
        Some(TopicState::Provisioned { log, partitions: 4 })
    );
}

/// 0006 §4.1: a re-provision that loses its compare-and-set marks the record
/// unconfirmed, because the backend may have applied its call after the one
/// recorded. Once the configurations agree on the recorded retention, it is
/// handed again, and then no call.
#[tokio::test]
async fn a_lost_retention_record_is_provisioned_again_when_its_call_was_applied_last() {
    let db = test_db().await;
    load(
        &db,
        &[TOPIC_A],
        json!({ "partitions": 4, "retention": { "duration": "1d" } }),
    )
    .await;
    let gate = Arc::new(Gate::default());
    let backend = Arc::new(ScriptedBackend::answering(
        TOPIC_A,
        [
            Answer::Ready(4),
            Answer::GatedApplyOnRelease(Arc::clone(&gate)),
            Answer::Ready(4),
        ],
    ));
    let through = resolver(&backend);
    let held = ProvisioningLimits {
        call_timeout: LONG,
        attempt_timeout: LONG,
    };
    let (one, two) = (instance(&db), instance(&db));
    one.provision_topics(&through, held).await;

    load(
        &db,
        &[TOPIC_A],
        json!({ "partitions": 4, "retention": { "duration": "2d" } }),
    )
    .await;
    let held_pass = one.provision_topics(&through, held);
    let pass_while_held = async {
        gate.entered.notified().await;
        load(
            &db,
            &[TOPIC_A],
            json!({ "partitions": 4, "retention": { "duration": "3d" } }),
        )
        .await;
        two.provision_topics(&through, held).await;
        gate.release.notify_one();
    };
    tokio::time::timeout(BOUND, async { tokio::join!(held_pass, pass_while_held) })
        .await
        .expect("the held pass and the pass while it is held resolve");

    load(
        &db,
        &[TOPIC_A],
        json!({ "partitions": 4, "retention": { "duration": "3d" } }),
    )
    .await;
    one.provision_topics(&through, held).await;
    one.provision_topics(&through, held).await;

    let calls = backend.calls(TOPIC_A);
    let &[(_, LogClaim { new: log, .. }), ..] = calls.as_slice() else {
        panic!("expected provision calls: {calls:?}")
    };
    let call = |hours: u64, replacing: Replacing| {
        (
            TopicSpec {
                topic: GtsInstanceId::try_new(TOPIC_A).unwrap(),
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
        state(&one, TOPIC_A).await,
        Some(TopicState::Provisioned { log, partitions: 4 })
    );
}

/// 0006 §4.1: the mark after a lost record bumps `retention_version`, so a
/// re-provision that read the record before that mark cannot confirm its own
/// retention over a call the backend applied after it. A reads first and is
/// applied last; C records in between; B reads C's record and is answered
/// after A's mark.
#[tokio::test]
async fn a_mark_after_a_lost_record_cannot_be_cleared_by_an_earlier_read() {
    let db = test_db().await;
    load(
        &db,
        &[TOPIC_A],
        json!({ "partitions": 4, "retention": { "duration": "1d" } }),
    )
    .await;
    let (gate_a, gate_b) = (Arc::new(Gate::default()), Arc::new(Gate::default()));
    let backend = Arc::new(ScriptedBackend::answering(
        TOPIC_A,
        [
            Answer::Ready(4),
            Answer::GatedApplyOnRelease(Arc::clone(&gate_a)),
            Answer::Ready(4),
            Answer::Gated(Arc::clone(&gate_b)),
            Answer::Ready(4),
        ],
    ));
    let through = resolver(&backend);
    let held = ProvisioningLimits {
        call_timeout: LONG,
        attempt_timeout: LONG,
    };
    let (a, b, c) = (instance(&db), instance(&db), instance(&db));
    a.provision_topics(&through, held).await;

    load(
        &db,
        &[TOPIC_A],
        json!({ "partitions": 4, "retention": { "duration": "2d" } }),
    )
    .await;
    let a_done = Notify::new();
    let a_pass = async {
        a.provision_topics(&through, held).await;
        a_done.notify_one();
    };
    let c_then_b = async {
        gate_a.entered.notified().await;
        load(
            &db,
            &[TOPIC_A],
            json!({ "partitions": 4, "retention": { "duration": "3d" } }),
        )
        .await;
        c.provision_topics(&through, held).await;
        load(
            &db,
            &[TOPIC_A],
            json!({ "partitions": 4, "retention": { "duration": "4d" } }),
        )
        .await;
        b.provision_topics(&through, held).await;
    };
    let releases = async {
        gate_b.entered.notified().await;
        gate_a.release.notify_one();
        a_done.notified().await;
        gate_b.release.notify_one();
    };
    tokio::time::timeout(BOUND, async { tokio::join!(a_pass, c_then_b, releases) })
        .await
        .expect("both held passes and the pass between them resolve");

    b.provision_topics(&through, held).await;
    b.provision_topics(&through, held).await;

    let calls = backend.calls(TOPIC_A);
    let &[(_, LogClaim { new: log, .. }), ..] = calls.as_slice() else {
        panic!("expected provision calls: {calls:?}")
    };
    let call = |hours: u64, replacing: Replacing| {
        (
            TopicSpec {
                topic: GtsInstanceId::try_new(TOPIC_A).unwrap(),
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
            // C, recorded between A's read and B's.
            call(72, Replacing::Held),
            // B, held until A's record has lost and marked.
            call(96, Replacing::Held),
            // A, read first and applied last.
            call(48, Replacing::Held),
            call(96, Replacing::Held),
        ]
    );
    assert_eq!(
        state(&b, TOPIC_A).await,
        Some(TopicState::Provisioned { log, partitions: 4 })
    );
}

/// 0006 §4.1: two instances driving one live first attempt with different
/// retention both get `Ready`; the one whose `provisioned` write loses marks
/// the record unconfirmed, because the backend may have applied its call last.
/// Once the configurations agree on the recorded retention, it is handed
/// again, and then no call.
#[tokio::test]
async fn a_first_provisioning_that_loses_its_record_is_provisioned_again_once_the_configs_agree() {
    let db = test_db().await;
    load(
        &db,
        &[TOPIC_A],
        json!({ "partitions": 4, "retention": { "duration": "2d" } }),
    )
    .await;
    let gate = Arc::new(Gate::default());
    let backend = Arc::new(ScriptedBackend::answering(
        TOPIC_A,
        [
            Answer::GatedApplyOnRelease(Arc::clone(&gate)),
            Answer::Ready(4),
        ],
    ));
    let through = resolver(&backend);
    let held = ProvisioningLimits {
        call_timeout: LONG,
        attempt_timeout: LONG,
    };
    let (one, two) = (instance(&db), instance(&db));

    let held_pass = one.provision_topics(&through, held);
    let pass_while_held = async {
        gate.entered.notified().await;
        load(
            &db,
            &[TOPIC_A],
            json!({ "partitions": 4, "retention": { "duration": "3d" } }),
        )
        .await;
        two.provision_topics(&through, held).await;
        gate.release.notify_one();
    };
    tokio::time::timeout(BOUND, async { tokio::join!(held_pass, pass_while_held) })
        .await
        .expect("the held pass and the pass while it is held resolve");

    load(
        &db,
        &[TOPIC_A],
        json!({ "partitions": 4, "retention": { "duration": "3d" } }),
    )
    .await;
    one.provision_topics(&through, held).await;
    one.provision_topics(&through, held).await;

    let calls = backend.calls(TOPIC_A);
    let &[(_, LogClaim { new: log, .. }), ..] = calls.as_slice() else {
        panic!("expected provision calls: {calls:?}")
    };
    let call = |hours: u64, replacing: Replacing| {
        (
            TopicSpec {
                topic: GtsInstanceId::try_new(TOPIC_A).unwrap(),
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
            // The live attempt, continued while the first call is held.
            call(72, Replacing::Log(log)),
            // Started the attempt, applied last.
            call(48, Replacing::Nothing),
            call(72, Replacing::Held),
        ]
    );
    assert_eq!(
        state(&one, TOPIC_A).await,
        Some(TopicState::Provisioned { log, partitions: 4 })
    );
}

/// 0006 §4.1: a first provisioning call answered `Pending` after another
/// instance driving the same attempt recorded the topic may still be applied
/// by the backend, so it marks the record unconfirmed. Once the
/// configurations agree on the recorded retention, it is handed again, and
/// then no call.
#[tokio::test]
async fn a_first_provisioning_left_pending_after_another_instance_recorded_is_provisioned_again() {
    let db = test_db().await;
    load(
        &db,
        &[TOPIC_A],
        json!({ "partitions": 4, "retention": { "duration": "2d" } }),
    )
    .await;
    let gate = Arc::new(Gate::default());
    let backend = Arc::new(ScriptedBackend::answering(
        TOPIC_A,
        [
            Answer::GatedPendingOnRelease(Arc::clone(&gate)),
            Answer::Ready(4),
        ],
    ));
    let through = resolver(&backend);
    let held = ProvisioningLimits {
        call_timeout: LONG,
        attempt_timeout: LONG,
    };
    let (one, two) = (instance(&db), instance(&db));

    let held_pass = one.provision_topics(&through, held);
    let pass_while_held = async {
        gate.entered.notified().await;
        load(
            &db,
            &[TOPIC_A],
            json!({ "partitions": 4, "retention": { "duration": "3d" } }),
        )
        .await;
        two.provision_topics(&through, held).await;
        gate.release.notify_one();
    };
    tokio::time::timeout(BOUND, async { tokio::join!(held_pass, pass_while_held) })
        .await
        .expect("the held pass and the pass while it is held resolve");

    load(
        &db,
        &[TOPIC_A],
        json!({ "partitions": 4, "retention": { "duration": "3d" } }),
    )
    .await;
    one.provision_topics(&through, held).await;
    one.provision_topics(&through, held).await;

    let calls = backend.calls(TOPIC_A);
    let &[(_, LogClaim { new: log, .. }), ..] = calls.as_slice() else {
        panic!("expected provision calls: {calls:?}")
    };
    let call = |hours: u64, replacing: Replacing| {
        (
            TopicSpec {
                topic: GtsInstanceId::try_new(TOPIC_A).unwrap(),
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
            // The live attempt, continued and recorded while the first call
            // is held.
            call(72, Replacing::Log(log)),
            // Started the attempt, answered `Pending`.
            call(48, Replacing::Nothing),
            call(72, Replacing::Held),
        ]
    );
    assert_eq!(
        state(&one, TOPIC_A).await,
        Some(TopicState::Provisioned { log, partitions: 4 })
    );
}

/// 0006 §4.1: a first provisioning call that fails after another instance
/// driving the same attempt recorded the topic may have been applied by the
/// backend, so it marks the record unconfirmed. Once the configurations agree
/// on the recorded retention, it is handed again, and then no call.
#[tokio::test]
async fn a_first_provisioning_failed_after_another_instance_recorded_is_provisioned_again() {
    let db = test_db().await;
    load(
        &db,
        &[TOPIC_A],
        json!({ "partitions": 4, "retention": { "duration": "2d" } }),
    )
    .await;
    let gate = Arc::new(Gate::default());
    let backend = Arc::new(ScriptedBackend::answering(
        TOPIC_A,
        [
            Answer::GatedFailOnRelease(Arc::clone(&gate)),
            Answer::Ready(4),
        ],
    ));
    let through = resolver(&backend);
    let held = ProvisioningLimits {
        call_timeout: LONG,
        attempt_timeout: LONG,
    };
    let (one, two) = (instance(&db), instance(&db));

    let held_pass = one.provision_topics(&through, held);
    let pass_while_held = async {
        gate.entered.notified().await;
        load(
            &db,
            &[TOPIC_A],
            json!({ "partitions": 4, "retention": { "duration": "3d" } }),
        )
        .await;
        two.provision_topics(&through, held).await;
        gate.release.notify_one();
    };
    tokio::time::timeout(BOUND, async { tokio::join!(held_pass, pass_while_held) })
        .await
        .expect("the held pass and the pass while it is held resolve");

    load(
        &db,
        &[TOPIC_A],
        json!({ "partitions": 4, "retention": { "duration": "3d" } }),
    )
    .await;
    one.provision_topics(&through, held).await;
    one.provision_topics(&through, held).await;

    let calls = backend.calls(TOPIC_A);
    let &[(_, LogClaim { new: log, .. }), ..] = calls.as_slice() else {
        panic!("expected provision calls: {calls:?}")
    };
    let call = |hours: u64, replacing: Replacing| {
        (
            TopicSpec {
                topic: GtsInstanceId::try_new(TOPIC_A).unwrap(),
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
            // The live attempt, continued and recorded while the first call
            // is held.
            call(72, Replacing::Log(log)),
            // Started the attempt, failed.
            call(48, Replacing::Nothing),
            call(72, Replacing::Held),
        ]
    );
    assert_eq!(
        state(&one, TOPIC_A).await,
        Some(TopicState::Provisioned { log, partitions: 4 })
    );
}

/// 0007 §2.6: a configured partition count that differs from the recorded one
/// is logged at every provisioning check, and is no reason to call again.
#[tracing_test::traced_test]
#[tokio::test]
async fn a_partition_mismatch_is_logged_on_every_pass() {
    let db = loaded(&[TOPIC_A]).await;
    let backend = Arc::new(ScriptedBackend::answering(TOPIC_A, [Answer::Ready(2)]));
    let manager = instance(&db);

    for _ in 0..3 {
        manager
            .provision_topics(&resolver(&backend), limits(LONG))
            .await;
    }

    let calls = backend.calls(TOPIC_A);
    let &[(_, LogClaim { new: log, .. })] = calls.as_slice() else {
        panic!("expected one provision call: {calls:?}")
    };
    assert_eq!(
        backend.calls(TOPIC_A),
        vec![(
            TopicSpec {
                topic: GtsInstanceId::try_new(TOPIC_A).unwrap(),
                partitions: 4,
                retention: Retention {
                    max_age: Some(Duration::from_hours(24 * 7)),
                    max_bytes: None,
                },
            },
            LogClaim {
                replacing: Replacing::Nothing,
                new: log
            },
        )]
    );
    logs_assert(|lines: &[&str]| {
        let logged = lines
            .iter()
            .filter(|line| line.contains("topic partition count differs"))
            .count();
        if logged == 3 {
            Ok(())
        } else {
            Err(format!("logged on {logged} of 3 passes"))
        }
    });
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
    let claims = backend.claims(TOPIC_A);
    let &[LogClaim { new: first, .. }] = claims.as_slice() else {
        panic!("expected one provision call: {claims:?}")
    };
    assert_eq!(
        state(&manager, TOPIC_A).await,
        Some(TopicState::Provisioning { log: first })
    );

    tokio::time::sleep(Duration::from_millis(20)).await;
    manager
        .provision_topics(&resolver(&backend), limits(Duration::ZERO))
        .await;

    let claims = backend.claims(TOPIC_A);
    let &[
        _,
        LogClaim {
            new: replacement, ..
        },
    ] = claims.as_slice()
    else {
        panic!("expected two provision calls: {claims:?}")
    };
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
    let claims = backend.claims(TOPIC_A);
    let &[LogClaim { new: log, .. }] = claims.as_slice() else {
        panic!("expected one provision call: {claims:?}")
    };
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

    let claims = backend.claims(TOPIC_A);
    let &[LogClaim { new: log, .. }, _] = claims.as_slice() else {
        panic!("expected two provision calls: {claims:?}")
    };
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

    let claims_a = backend.claims(TOPIC_A);
    let &[LogClaim { new: a, .. }] = claims_a.as_slice() else {
        panic!("expected one provision call: {claims_a:?}")
    };
    let claims_b = backend.claims(TOPIC_B);
    let &[LogClaim { new: b, .. }] = claims_b.as_slice() else {
        panic!("expected one provision call: {claims_b:?}")
    };
    assert_eq!(
        (claims_a, claims_b),
        (
            vec![LogClaim {
                replacing: Replacing::Nothing,
                new: a
            }],
            vec![LogClaim {
                replacing: Replacing::Nothing,
                new: b
            }],
        )
    );
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
        .expect("the records read")
        .expect("registered");
    assert_eq!(*topic.settings.partitions().value(), 2);
    assert_eq!(
        topic.settings.partitions().source(),
        crate::domain::resolution::Source::Backend
    );
}

/// A binding that cannot be read says nothing about the count in force: the
/// configured count would be wrong for a topic its backend holds with another.
#[tokio::test]
async fn a_binding_read_failure_is_an_error_not_the_configured_count() {
    let (db, dsn) = test_db_with_dsn().await;
    load(&db, &[TOPIC_A], json!({ "partitions": 4 })).await;
    let backend = Arc::new(ScriptedBackend::answering(TOPIC_A, [Answer::Ready(2)]));
    let manager = instance(&db);
    manager
        .provision_topics(&resolver(&backend), limits(LONG))
        .await;
    crate::test_support::tables::drop_table(&dsn, "event_broker__topic_binding").await;

    let topic = manager
        .get_topic(&GtsInstanceId::try_new(TOPIC_A).unwrap())
        .await;
    let Err(err) = topic else {
        panic!("an unreadable binding must not answer: {topic:?}");
    };
    assert!(
        matches!(&err, DomainError::StorageUnavailable { reason, source: Some(_) } if reason == "database scope resolution failed"),
        "{err:?}"
    );
    let listed = manager.list_topics().await;
    assert!(
        matches!(&listed, Err(DomainError::StorageUnavailable { .. })),
        "{listed:?}"
    );
}

/// A topic with no record has no state: registered, nothing attempted.
#[tokio::test]
async fn a_topic_never_attempted_has_no_state() {
    let db = loaded(&[TOPIC_A]).await;
    assert_eq!(state(&instance(&db), TOPIC_A).await, None);
}

/// 0007 §2.6: a re-provision hands the backend the partition count the record
/// holds, not the configured one - the recorded count stays in force.
#[tokio::test]
async fn a_re_provision_hands_the_recorded_partition_count() {
    let db = test_db().await;
    load(
        &db,
        &[TOPIC_A],
        json!({ "partitions": 4, "retention": { "duration": "1d" } }),
    )
    .await;
    let backend = Arc::new(ScriptedBackend::answering(TOPIC_A, [Answer::Ready(2)]));
    let manager = instance(&db);
    manager
        .provision_topics(&resolver(&backend), limits(LONG))
        .await;

    load(
        &db,
        &[TOPIC_A],
        json!({ "partitions": 4, "retention": { "duration": "2d" } }),
    )
    .await;
    manager
        .provision_topics(&resolver(&backend), limits(LONG))
        .await;

    let calls = backend.calls(TOPIC_A);
    let &[(_, LogClaim { new: log, .. }), _] = calls.as_slice() else {
        panic!("expected two provision calls: {calls:?}")
    };
    let topic = GtsInstanceId::try_new(TOPIC_A).unwrap();
    assert_eq!(
        backend.calls(TOPIC_A),
        vec![
            (
                TopicSpec {
                    topic: topic.clone(),
                    partitions: 4,
                    retention: Retention {
                        max_age: Some(Duration::from_hours(24)),
                        max_bytes: None,
                    },
                },
                LogClaim {
                    replacing: Replacing::Nothing,
                    new: log
                },
            ),
            (
                TopicSpec {
                    topic,
                    partitions: 2,
                    retention: Retention {
                        max_age: Some(Duration::from_hours(48)),
                        max_bytes: None,
                    },
                },
                LogClaim {
                    replacing: Replacing::Held,
                    new: log
                },
            ),
        ]
    );
    assert_eq!(
        state(&manager, TOPIC_A).await,
        Some(TopicState::Provisioned { log, partitions: 2 })
    );
}

/// A configured count beyond any backend's limit never reaches the backend
/// for a topic it already holds: the re-provision carries the recorded count.
#[tokio::test]
async fn a_re_provision_hands_the_recorded_partition_count_over_an_out_of_range_one() {
    let db = test_db().await;
    load(
        &db,
        &[TOPIC_A],
        json!({ "partitions": 4, "retention": { "duration": "1d" } }),
    )
    .await;
    let backend = Arc::new(ScriptedBackend::answering(TOPIC_A, [Answer::Ready(2)]));
    let manager = instance(&db);
    manager
        .provision_topics(&resolver(&backend), limits(LONG))
        .await;

    load(
        &db,
        &[TOPIC_A],
        json!({ "partitions": 300, "retention": { "duration": "2d" } }),
    )
    .await;
    manager
        .provision_topics(&resolver(&backend), limits(LONG))
        .await;

    let calls = backend.calls(TOPIC_A);
    let &[(_, LogClaim { new: log, .. }), _] = calls.as_slice() else {
        panic!("expected two provision calls: {calls:?}")
    };
    let topic = GtsInstanceId::try_new(TOPIC_A).unwrap();
    assert_eq!(
        backend.calls(TOPIC_A),
        vec![
            (
                TopicSpec {
                    topic: topic.clone(),
                    partitions: 4,
                    retention: Retention {
                        max_age: Some(Duration::from_hours(24)),
                        max_bytes: None,
                    },
                },
                LogClaim {
                    replacing: Replacing::Nothing,
                    new: log
                },
            ),
            (
                TopicSpec {
                    topic,
                    partitions: 2,
                    retention: Retention {
                        max_age: Some(Duration::from_hours(48)),
                        max_bytes: None,
                    },
                },
                LogClaim {
                    replacing: Replacing::Held,
                    new: log
                },
            ),
        ]
    );
    assert_eq!(
        state(&manager, TOPIC_A).await,
        Some(TopicState::Provisioned { log, partitions: 2 })
    );
}

/// A retention the record's columns cannot hold is recorded saturated to
/// `i64::MAX` and never blocks provisioning; the next pass finds it recorded
/// and makes no call.
#[tokio::test]
async fn an_unrepresentable_retention_is_recorded_saturated() {
    let db = test_db().await;
    load(
        &db,
        &[TOPIC_A],
        json!({
            "partitions": 4,
            "retention": { "duration": "300000000years", "size_bytes": 18_446_744_073_709_551_615_u64 },
        }),
    )
    .await;
    let backend = Arc::new(ScriptedBackend::answering(TOPIC_A, [Answer::Ready(4)]));
    let manager = instance(&db);

    manager
        .provision_topics(&resolver(&backend), limits(LONG))
        .await;
    manager
        .provision_topics(&resolver(&backend), limits(LONG))
        .await;

    let calls = backend.calls(TOPIC_A);
    let &[(_, LogClaim { new: log, .. })] = calls.as_slice() else {
        panic!("expected one provision call: {calls:?}")
    };
    assert_eq!(
        calls,
        vec![(
            TopicSpec {
                topic: GtsInstanceId::try_new(TOPIC_A).unwrap(),
                partitions: 4,
                retention: Retention {
                    max_age: Some(Duration::from_hours(300_000_000 * 8_766)),
                    max_bytes: Some(u64::MAX),
                },
            },
            LogClaim {
                replacing: Replacing::Nothing,
                new: log
            },
        )]
    );
    assert_eq!(
        state(&manager, TOPIC_A).await,
        Some(TopicState::Provisioned { log, partitions: 4 })
    );
    let row = manager
        .binding(super::binding_key(
            &GtsInstanceId::try_new(TOPIC_A).unwrap(),
        ))
        .await
        .expect("record reads")
        .expect("record exists");
    assert_eq!(
        (row.retention_max_age_ms, row.retention_max_bytes),
        (Some(i64::MAX), Some(i64::MAX))
    );
}

/// 0006 §4.1, ADR-0008: a backend refusing a re-provision because it holds
/// another log id for the topic breaks the single-holder invariant, so it is
/// logged at error level; its recorded retention is unchanged and the next
/// pass retries.
#[tracing_test::traced_test]
#[tokio::test]
async fn a_log_conflict_on_re_provision_is_logged_as_an_error() {
    let db = test_db().await;
    load(
        &db,
        &[TOPIC_A],
        json!({ "partitions": 4, "retention": { "duration": "1d" } }),
    )
    .await;
    let backend = Arc::new(ScriptedBackend::answering(
        TOPIC_A,
        [Answer::Ready(4), Answer::Conflict],
    ));
    let manager = instance(&db);
    manager
        .provision_topics(&resolver(&backend), limits(LONG))
        .await;

    load(
        &db,
        &[TOPIC_A],
        json!({ "partitions": 4, "retention": { "duration": "2d" } }),
    )
    .await;
    manager
        .provision_topics(&resolver(&backend), limits(LONG))
        .await;

    let calls = backend.calls(TOPIC_A);
    let [first, second] = calls.as_slice() else {
        panic!("expected two provision calls: {calls:?}")
    };
    let log = first.1.new;
    let topic = GtsInstanceId::try_new(TOPIC_A).unwrap();
    assert_eq!(
        [first.clone(), second.clone()],
        [
            (
                TopicSpec {
                    topic: topic.clone(),
                    partitions: 4,
                    retention: Retention {
                        max_age: Some(Duration::from_hours(24)),
                        max_bytes: None,
                    },
                },
                LogClaim {
                    replacing: Replacing::Nothing,
                    new: log
                },
            ),
            (
                TopicSpec {
                    topic: topic.clone(),
                    partitions: 4,
                    retention: Retention {
                        max_age: Some(Duration::from_hours(48)),
                        max_bytes: None,
                    },
                },
                LogClaim {
                    replacing: Replacing::Held,
                    new: log
                },
            ),
        ]
    );
    assert_eq!(
        state(&manager, TOPIC_A).await,
        Some(TopicState::Provisioned { log, partitions: 4 })
    );
    assert_eq!(
        manager
            .binding(super::binding_key(&topic))
            .await
            .expect("record reads")
            .map(|row| (row.retention_max_age_ms, row.retention_max_bytes)),
        Some((Some(86_400_000), None))
    );
    let log_id = log.to_string();
    logs_assert(|lines: &[&str]| {
        let logged = lines
            .iter()
            .filter(|line| {
                line.contains("ERROR")
                    && line.contains("holds another log id for it")
                    && line.contains(&log_id)
            })
            .count();
        let errors = lines.iter().filter(|line| line.contains("ERROR")).count();
        let failed = lines
            .iter()
            .filter(|line| {
                line.contains("WARN") && line.contains("re-provisioning a provisioned topic failed")
            })
            .count();
        if (logged, errors, failed) == (1, 1, 0) {
            Ok(())
        } else {
            Err(format!(
                "logged the refusal {logged} times, {errors} error lines and {failed} \
                 failure warnings; expected the refusal once, one error line and no warning"
            ))
        }
    });
}

/// 0006 §4.1, ADR-0008 O3: a re-provision never creates the log. A topic
/// re-routed to a backend that holds nothing for it, with its retention
/// changed, is refused there with nothing created, and its recorded retention
/// is unchanged; routed back to the backend holding the log, the retention is
/// recorded.
#[tracing_test::traced_test]
#[tokio::test]
async fn a_re_provision_never_creates_the_log_on_a_newly_routed_backend() {
    let db = test_db().await;
    load(
        &db,
        &[TOPIC_A],
        json!({ "partitions": 4, "retention": { "duration": "1d" } }),
    )
    .await;
    let a = Arc::new(memory_event_broker_plugin::MemoryBackend::new());
    let b = Arc::new(memory_event_broker_plugin::MemoryBackend::new());
    let through_a = SingleBackendResolver::new(Arc::clone(&a) as Arc<dyn Backend>);
    let through_b = SingleBackendResolver::new(Arc::clone(&b) as Arc<dyn Backend>);
    let manager = instance(&db);
    manager.provision_topics(&through_a, limits(LONG)).await;
    let Some(TopicState::Provisioned { log, .. }) = state(&manager, TOPIC_A).await else {
        panic!("expected the topic provisioned through A");
    };

    load(
        &db,
        &[TOPIC_A],
        json!({ "partitions": 4, "retention": { "duration": "2d" } }),
    )
    .await;
    manager.provision_topics(&through_b, limits(LONG)).await;

    let topic = GtsInstanceId::try_new(TOPIC_A).unwrap();
    let on_b = b
        .resolve(
            &TopicPartition {
                topic: topic.clone(),
                partition: event_broker_sdk::backend::Partition::new(0),
            },
            Position::Latest,
        )
        .await;
    assert!(
        matches!(&on_b, Err(BackendError::NotProvisioned { detail }) if detail == "resolve: the topic has no log on this backend"),
        "B must hold nothing for the topic: {on_b:?}"
    );
    assert_eq!(
        state(&manager, TOPIC_A).await,
        Some(TopicState::Provisioned { log, partitions: 4 })
    );
    let recorded = |row: topic_binding::Model| (row.retention_max_age_ms, row.retention_max_bytes);
    let key = super::binding_key(&topic);
    assert_eq!(
        manager
            .binding(key)
            .await
            .expect("record reads")
            .map(recorded),
        Some((Some(86_400_000), None))
    );

    manager.provision_topics(&through_a, limits(LONG)).await;

    assert_eq!(
        state(&manager, TOPIC_A).await,
        Some(TopicState::Provisioned { log, partitions: 4 })
    );
    assert_eq!(
        manager
            .binding(key)
            .await
            .expect("record reads")
            .map(recorded),
        Some((Some(172_800_000), None))
    );
    let log_id = log.to_string();
    logs_assert(|lines: &[&str]| {
        let logged = lines
            .iter()
            .filter(|line| {
                line.contains("ERROR")
                    && line.contains("holds no log for it")
                    && line.contains(&log_id)
            })
            .count();
        let errors = lines.iter().filter(|line| line.contains("ERROR")).count();
        if (logged, errors) == (1, 1) {
            Ok(())
        } else {
            Err(format!(
                "logged the refusal {logged} times and {errors} error lines; \
                 expected the refusal once and one error line"
            ))
        }
    });
}
