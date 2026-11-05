//! Partition registration of the outbox producer: one broker id per slot in a
//! process, no slot waiting on another's registration, and no broker id lost
//! to a caller dropped mid-registration.
//!
//! `partition_producer` is `pub(crate)`, so these are in-crate unit tests over
//! the registration store's SQLite path. The broker is a `ScriptedBroker`: it
//! answers only `register_producer`, counts the calls, and holds a call on a
//! gate when the test arms one. Every other operation panics.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::Mutex as StdMutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use sea_orm::{ColumnTrait, Condition, EntityTrait};
use tokio::sync::{Notify, mpsc, oneshot};
use tokio::time::timeout;
use toolkit_db::secure::{AccessScope, SecureDeleteExt};
use toolkit_security::SecurityContext;
use uuid::Uuid;

use super::DbProducer;
use crate::api::ProducerMode;
use crate::broker_stub::BrokerStub;
use crate::error::EventBrokerError;
use crate::ids::ProducerId;
use crate::producer::registration::{
    self, NewRegistration, ProducerRegistration, ProducerRegistrationStore, RegistrationSlot,
};
use crate::producer::types::{DbDeduplication, ProducerIdentity};

const KEY: &str = "orders";
const QUEUE: &str = "event-broker-producer";
const CLIENT_AGENT: &str = "order-service/1.0";
/// Bounds each await after a gate or pause is armed, so a held registration
/// that never releases fails the test instead of hanging it.
const BOUND: Duration = Duration::from_secs(1);

/// The id the broker hands out on its `n`th registration (1-based).
fn registered(n: usize) -> ProducerId {
    ProducerId(Uuid::from_u128(
        u128::try_from(n).expect("a small call count"),
    ))
}

/// Answers `register_producer` with `registered(n)` on its `n`th call. Before
/// answering it reports `n` on `entered`, then waits on the gate armed for
/// `n`, if any.
struct ScriptedBroker {
    register_calls: AtomicUsize,
    gates: StdMutex<HashMap<usize, oneshot::Receiver<()>>>,
    entered: mpsc::UnboundedSender<usize>,
}

impl ScriptedBroker {
    fn new() -> (Arc<Self>, mpsc::UnboundedReceiver<usize>) {
        let (entered, entered_rx) = mpsc::unbounded_channel();
        let broker = Arc::new(Self {
            register_calls: AtomicUsize::new(0),
            gates: StdMutex::new(HashMap::new()),
            entered,
        });
        (broker, entered_rx)
    }

    /// Holds the `n`th registration until the returned sender fires or drops.
    fn gate(&self, n: usize) -> oneshot::Sender<()> {
        let (release, gate) = oneshot::channel();
        self.gates.lock().expect("gates lock").insert(n, gate);
        release
    }

    fn register_calls(&self) -> usize {
        self.register_calls.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl BrokerStub for ScriptedBroker {
    async fn register_producer(
        &self,
        _ctx: &SecurityContext,
        _mode: ProducerMode,
        _client_agent: &str,
    ) -> Result<ProducerId, EventBrokerError> {
        let n = self.register_calls.fetch_add(1, Ordering::SeqCst) + 1;
        let gate = self.gates.lock().expect("gates lock").remove(&n);
        self.entered.send(n).expect("the test listens");
        if let Some(gate) = gate {
            // A dropped sender releases the call too.
            let _ = gate.await;
        }
        Ok(registered(n))
    }
}

/// Holds the next first registration right after it releases its slot's
/// write lock, before the id it read is cached: it reports on `reached`, then
/// waits on `resume`. Unarmed, it holds nothing.
#[derive(Default)]
pub(super) struct UnlockPause {
    armed: AtomicBool,
    reached: Notify,
    resume: Notify,
}

impl UnlockPause {
    fn arm(&self) {
        self.armed.store(true, Ordering::SeqCst);
    }

    pub(super) async fn pause(&self) {
        if self.armed.swap(false, Ordering::SeqCst) {
            self.reached.notify_one();
            self.resume.notified().await;
        }
    }
}

/// A migrated in-memory SQLite database.
async fn sqlite_db() -> toolkit_db::Db {
    let db = toolkit_db::connect_db(
        "sqlite::memory:",
        toolkit_db::ConnectOpts {
            max_conns: Some(1),
            ..Default::default()
        },
    )
    .await
    .expect("sqlite connects");
    toolkit_db::migration_runner::run_migrations_for_testing(
        &db,
        crate::producer_registration_migrations(),
    )
    .await
    .expect("registration migrations run");
    db
}

/// A managed chained producer over `broker` and `db`, built without touching
/// the broker.
async fn managed_producer(broker: Arc<ScriptedBroker>, db: toolkit_db::Db) -> DbProducer {
    DbProducer::builder()
        .broker(broker)
        .db(db)
        .security_context(SecurityContext::anonymous())
        .identity(
            ProducerIdentity::new()
                .source("order-service")
                .client_agent(CLIENT_AGENT),
        )
        .deduplication(
            DbDeduplication::managed(ProducerMode::Chained)
                .key(KEY)
                .build()
                .expect("a valid managed setting"),
        )
        .topics(["gts.cf.core.events.topic.v1~example.sdk.db.orders.v1"])
        .event_type_patterns(["gts.cf.core.events.event.v1~example.sdk.db.*"])
        .lazy_validation()
        .build()
        .await
        .expect("the producer builds")
}

#[tokio::test]
async fn a_slow_registration_does_not_block_another_partition() {
    let (broker, mut entered) = ScriptedBroker::new();
    let producer = managed_producer(broker.clone(), sqlite_db().await).await;
    assert_eq!(
        producer
            .partition_producer(QUEUE, 0)
            .await
            .expect("partition 0 registers"),
        registered(1)
    );
    assert_eq!(entered.recv().await, Some(1));

    let release = broker.gate(2);
    let slow = tokio::spawn({
        let producer = producer.clone();
        async move { producer.partition_producer(QUEUE, 1).await }
    });
    assert_eq!(
        timeout(BOUND, entered.recv()).await,
        Ok(Some(2)),
        "partition 1 is registering"
    );

    let cached = timeout(BOUND, producer.partition_producer(QUEUE, 0))
        .await
        .expect("partition 0 answers while partition 1 is still registering")
        .expect("partition 0 has its id");
    assert_eq!(cached, registered(1));

    release.send(()).expect("partition 1 is still waiting");
    assert_eq!(
        timeout(BOUND, slow)
            .await
            .expect("the registration task completes in time")
            .expect("the registration task completes")
            .expect("partition 1 registers"),
        registered(2)
    );
    assert_eq!(broker.register_calls(), 2);
}

#[tokio::test]
async fn a_drop_after_register_does_not_orphan_the_broker_id() {
    let (broker, mut entered) = ScriptedBroker::new();
    let db = sqlite_db().await;
    let producer = managed_producer(broker.clone(), db.clone()).await;

    // The first caller is aborted while the broker holds the registration.
    let release = broker.gate(1);
    let first = tokio::spawn({
        let producer = producer.clone();
        async move { producer.partition_producer(QUEUE, 3).await }
    });
    assert_eq!(timeout(BOUND, entered.recv()).await, Ok(Some(1)));
    first.abort();
    assert!(
        timeout(BOUND, first)
            .await
            .expect("the aborted caller finishes in time")
            .expect_err("the first caller is dropped")
            .is_cancelled(),
        "the first caller is cancelled, not finished"
    );
    release
        .send(())
        .expect("the registration outlives its caller");

    let second = timeout(BOUND, producer.partition_producer(QUEUE, 3))
        .await
        .expect("the slot resolves in time")
        .expect("the slot resolves after the drop");

    assert_eq!(broker.register_calls(), 1, "the broker registered one id");
    assert_eq!(second, registered(1));
    assert_eq!(
        timeout(BOUND, ProducerRegistrationStore::new(db).load_all(KEY))
            .await
            .expect("the store reads in time")
            .expect("the store reads"),
        vec![ProducerRegistration {
            slot: RegistrationSlot {
                key: KEY.to_owned(),
                queue: QUEUE.to_owned(),
                partition: 3,
            },
            producer_id: registered(1),
            mode: ProducerMode::Chained,
            client_agent: CLIENT_AGENT.to_owned(),
            generation: 1,
        }]
    );
}

#[tokio::test]
async fn a_queue_name_over_255_bytes_is_refused() {
    let (broker, _entered) = ScriptedBroker::new();
    let producer = managed_producer(broker, sqlite_db().await).await;

    let Err(EventBrokerError::InvalidProducerOptions { detail }) =
        producer.outbox_queue("q".repeat(256), toolkit_db::outbox::Partitions::of(1))
    else {
        panic!("a 256-byte queue name is refused as invalid producer options");
    };
    assert_eq!(
        detail,
        "producer outbox queue name must be at most 255 bytes"
    );

    assert!(
        producer
            .outbox_queue("q".repeat(255), toolkit_db::outbox::Partitions::of(1))
            .is_ok(),
        "a 255-byte queue name fits the registration column"
    );
}

#[tokio::test]
async fn a_queue_name_with_edge_whitespace_is_refused() {
    let (broker, _entered) = ScriptedBroker::new();
    let producer = managed_producer(broker, sqlite_db().await).await;

    for queue in ["orders ", " orders", "orders\t"] {
        let Err(EventBrokerError::InvalidProducerOptions { detail }) =
            producer.outbox_queue(queue, toolkit_db::outbox::Partitions::of(1))
        else {
            panic!("queue name {queue:?} is refused as invalid producer options");
        };
        assert_eq!(
            detail, "producer outbox queue name must not have leading or trailing whitespace",
            "queue name {queue:?}"
        );
    }
}

#[tokio::test]
async fn an_overlong_queue_name_with_a_trailing_space_is_refused_for_whitespace() {
    let (broker, _entered) = ScriptedBroker::new();
    let producer = managed_producer(broker, sqlite_db().await).await;

    let Err(EventBrokerError::InvalidProducerOptions { detail }) = producer.outbox_queue(
        format!("{} ", "q".repeat(255)),
        toolkit_db::outbox::Partitions::of(1),
    ) else {
        panic!(
            "a 256-byte queue name with a trailing space is refused as invalid producer options"
        );
    };
    assert_eq!(
        detail,
        "producer outbox queue name must not have leading or trailing whitespace"
    );
}

#[tokio::test]
async fn an_all_spaces_queue_name_is_refused_as_blank() {
    let (broker, _entered) = ScriptedBroker::new();
    let producer = managed_producer(broker, sqlite_db().await).await;

    let Err(EventBrokerError::InvalidProducerOptions { detail }) =
        producer.outbox_queue(" ".repeat(256), toolkit_db::outbox::Partitions::of(1))
    else {
        panic!("a queue name of 256 spaces is refused as invalid producer options");
    };
    assert_eq!(detail, "producer outbox queue name must not be empty");
}

#[tokio::test]
async fn a_queue_name_with_inner_whitespace_is_accepted() {
    let (broker, _entered) = ScriptedBroker::new();
    let producer = managed_producer(broker, sqlite_db().await).await;

    assert!(
        producer
            .outbox_queue("orders created", toolkit_db::outbox::Partitions::of(1))
            .is_ok(),
        "inner whitespace is part of the queue name"
    );
}

#[tokio::test]
async fn a_test_slot_with_an_edge_whitespace_queue_is_refused() {
    let (broker, _entered) = ScriptedBroker::new();
    let db = sqlite_db().await;
    let producer = managed_producer(broker.clone(), db.clone()).await;

    let result = producer.partition_producer_for_test(" q", 0).await;

    let Err(EventBrokerError::InvalidProducerOptions { detail }) = result else {
        panic!("the slot is refused as invalid producer options: {result:?}");
    };
    assert_eq!(
        detail,
        "producer outbox queue name must not have leading or trailing whitespace"
    );
    assert_eq!(broker.register_calls(), 0, "the broker registered nothing");
    assert_eq!(
        ProducerRegistrationStore::new(db)
            .load_all(KEY)
            .await
            .expect("the store reads"),
        vec![]
    );
}

/// The id a slot holds before a test replaces it; the broker never hands it out.
const STORED: ProducerId = ProducerId(Uuid::from_u128(0xA));

/// Stores `producer_id` as the generation-1 id of `partition`, bypassing the broker.
async fn store_registration(db: &toolkit_db::Db, partition: i64, producer_id: ProducerId) {
    ProducerRegistrationStore::new(db.clone())
        .insert_new(NewRegistration {
            slot: &slot(partition),
            producer_id,
            mode: ProducerMode::Chained,
            client_agent: CLIENT_AGENT,
        })
        .await
        .expect("the registration stores");
}

fn slot(partition: i64) -> RegistrationSlot {
    RegistrationSlot {
        key: KEY.to_owned(),
        queue: QUEUE.to_owned(),
        partition,
    }
}

#[tokio::test]
async fn two_concurrent_replaces_of_one_slot_register_once() {
    let (broker, mut entered) = ScriptedBroker::new();
    let db = sqlite_db().await;
    store_registration(&db, 5, STORED).await;
    let producer = managed_producer(broker.clone(), db.clone()).await;

    // The first registration is held until a second caller reaches the
    // broker too, or until 300 ms pass when no second caller ever does.
    let release = broker.gate(1);
    let releaser = async move {
        assert_eq!(timeout(BOUND, entered.recv()).await, Ok(Some(1)));
        let _ = timeout(Duration::from_millis(300), entered.recv()).await;
        release
            .send(())
            .expect("the first registration is still held");
    };
    let (first, second, ()) = timeout(BOUND + Duration::from_millis(300), async {
        tokio::join!(
            producer.replace_unknown_producer(QUEUE, 5, STORED),
            producer.replace_unknown_producer(QUEUE, 5, STORED),
            releaser,
        )
    })
    .await
    .expect("both replaces finish in time");

    assert!(
        matches!((&first, &second), (Ok(()), Ok(()))),
        "both replaces succeed: {first:?}, {second:?}"
    );
    assert_eq!(broker.register_calls(), 1, "the broker registered one id");
    assert_eq!(
        ProducerRegistrationStore::new(db)
            .load_all(KEY)
            .await
            .expect("the store reads"),
        vec![ProducerRegistration {
            slot: slot(5),
            producer_id: registered(1),
            mode: ProducerMode::Chained,
            client_agent: CLIENT_AGENT.to_owned(),
            generation: 2,
        }]
    );
}

#[tokio::test]
async fn a_drop_after_register_on_replace_orphans_nothing() {
    let (broker, mut entered) = ScriptedBroker::new();
    let db = sqlite_db().await;
    store_registration(&db, 6, STORED).await;
    let producer = managed_producer(broker.clone(), db.clone()).await;

    // The caller is aborted while the broker holds the registration.
    let release = broker.gate(1);
    let caller = tokio::spawn({
        let producer = producer.clone();
        async move { producer.replace_unknown_producer(QUEUE, 6, STORED).await }
    });
    assert_eq!(timeout(BOUND, entered.recv()).await, Ok(Some(1)));
    caller.abort();
    assert!(
        timeout(BOUND, caller)
            .await
            .expect("the aborted caller finishes in time")
            .expect_err("the caller is dropped")
            .is_cancelled(),
        "the caller is cancelled, not finished"
    );
    release.send(()).expect("the replace outlives its caller");

    let resolved = timeout(BOUND, producer.partition_producer(QUEUE, 6))
        .await
        .expect("the slot resolves in time")
        .expect("the slot resolves after the drop");

    assert_eq!(broker.register_calls(), 1, "the broker registered one id");
    assert_eq!(resolved, registered(1));
    assert_eq!(
        timeout(BOUND, ProducerRegistrationStore::new(db).load_all(KEY))
            .await
            .expect("the store reads in time")
            .expect("the store reads"),
        vec![ProducerRegistration {
            slot: slot(6),
            producer_id: registered(1),
            mode: ProducerMode::Chained,
            client_agent: CLIENT_AGENT.to_owned(),
            generation: 2,
        }]
    );
}

#[tokio::test]
async fn rotate_registration_replaces_each_slot_once() {
    let (broker, _entered) = ScriptedBroker::new();
    let db = sqlite_db().await;
    for partition in [0, 1, 2] {
        store_registration(
            &db,
            partition,
            ProducerId(Uuid::from_u128(0xA0 + u128::from(partition.unsigned_abs()))),
        )
        .await;
    }
    let producer = managed_producer(broker.clone(), db.clone()).await;

    producer
        .rotate_registration()
        .await
        .expect("the registrations rotate");

    assert_eq!(broker.register_calls(), 3, "one new id per slot");
    let mut rows = ProducerRegistrationStore::new(db)
        .load_all(KEY)
        .await
        .expect("the store reads");
    rows.sort_by_key(|row| row.slot.partition);
    assert_eq!(
        rows,
        vec![
            ProducerRegistration {
                slot: slot(0),
                producer_id: registered(1),
                mode: ProducerMode::Chained,
                client_agent: CLIENT_AGENT.to_owned(),
                generation: 2,
            },
            ProducerRegistration {
                slot: slot(1),
                producer_id: registered(2),
                mode: ProducerMode::Chained,
                client_agent: CLIENT_AGENT.to_owned(),
                generation: 2,
            },
            ProducerRegistration {
                slot: slot(2),
                producer_id: registered(3),
                mode: ProducerMode::Chained,
                client_agent: CLIENT_AGENT.to_owned(),
                generation: 2,
            },
        ]
    );
}

#[tokio::test]
async fn a_first_drain_racing_a_rotation_returns_the_rotated_id() {
    let (broker, _entered) = ScriptedBroker::new();
    let db = sqlite_db().await;
    store_registration(&db, 7, STORED).await;
    let producer = managed_producer(broker.clone(), db.clone()).await;

    // The first drain reads the stored id and releases the slot's write lock;
    // it is held there, before caching what it read, while the rotation
    // replaces that id.
    producer.unlock_pause.arm();
    let drain = tokio::spawn({
        let producer = producer.clone();
        async move { producer.partition_producer(QUEUE, 7).await }
    });
    timeout(BOUND, producer.unlock_pause.reached.notified())
        .await
        .expect("the drain reaches the pause in time");
    timeout(BOUND, producer.rotate_registration())
        .await
        .expect("the registrations rotate in time")
        .expect("the registrations rotate");
    producer.unlock_pause.resume.notify_one();

    let drained = timeout(BOUND, drain)
        .await
        .expect("the drain task completes in time")
        .expect("the drain task completes")
        .expect("the slot resolves");
    let cached = timeout(BOUND, producer.partition_producer(QUEUE, 7))
        .await
        .expect("the slot resolves again in time")
        .expect("the slot resolves again");

    assert_eq!(
        (drained, cached),
        (registered(1), registered(1)),
        "the drain and the cache both hold the rotated id"
    );
    assert_eq!(broker.register_calls(), 1, "the rotation registered one id");
    assert_eq!(
        ProducerRegistrationStore::new(db)
            .load_all(KEY)
            .await
            .expect("the store reads"),
        vec![ProducerRegistration {
            slot: slot(7),
            producer_id: registered(1),
            mode: ProducerMode::Chained,
            client_agent: CLIENT_AGENT.to_owned(),
            generation: 2,
        }]
    );
}

/// Deletes the stored row of `partition`, bypassing the producer.
async fn delete_registration(db: &toolkit_db::Db, partition: i64) {
    let conn = db.conn().expect("the database connects");
    let deleted = registration::Entity::delete_many()
        .secure()
        .scope_with(&AccessScope::allow_all())
        .filter(
            Condition::all()
                .add(registration::Column::RegistrationKey.eq(KEY))
                .add(registration::Column::Queue.eq(QUEUE))
                .add(registration::Column::OutboxPartition.eq(partition)),
        )
        .exec(&conn)
        .await
        .expect("the row deletes");
    assert_eq!(deleted.rows_affected, 1, "one row is deleted");
}

#[tokio::test]
async fn a_vanished_registration_row_is_not_served_from_the_cache() {
    let (broker, mut entered) = ScriptedBroker::new();
    let db = sqlite_db().await;
    store_registration(&db, 8, STORED).await;
    let producer = managed_producer(broker.clone(), db.clone()).await;
    assert_eq!(
        producer
            .partition_producer(QUEUE, 8)
            .await
            .expect("the stored id resolves"),
        STORED,
        "the stored id is cached"
    );

    // The row vanishes while the replace registers its new id, so the replace
    // loses its generation update and its reload finds no row.
    let release = broker.gate(1);
    let replace = tokio::spawn({
        let producer = producer.clone();
        async move { producer.replace_unknown_producer(QUEUE, 8, STORED).await }
    });
    assert_eq!(timeout(BOUND, entered.recv()).await, Ok(Some(1)));
    timeout(BOUND, delete_registration(&db, 8))
        .await
        .expect("the row deletes in time");
    release.send(()).expect("the replace is still registering");
    timeout(BOUND, replace)
        .await
        .expect("the replace task completes in time")
        .expect("the replace task completes")
        .expect("the replace ends without error");

    let resolved = timeout(BOUND, producer.partition_producer(QUEUE, 8))
        .await
        .expect("the slot resolves in time")
        .expect("the slot resolves after the row vanished");

    assert_eq!(
        broker.register_calls(),
        2,
        "the lost replace and a fresh registration"
    );
    assert_eq!(resolved, registered(2));
    assert_eq!(
        ProducerRegistrationStore::new(db)
            .load_all(KEY)
            .await
            .expect("the store reads"),
        vec![ProducerRegistration {
            slot: slot(8),
            producer_id: registered(2),
            mode: ProducerMode::Chained,
            client_agent: CLIENT_AGENT.to_owned(),
            generation: 1,
        }]
    );
}

#[tokio::test]
async fn a_rejected_id_whose_row_vanished_is_not_served_again() {
    let (broker, _entered) = ScriptedBroker::new();
    let db = sqlite_db().await;
    store_registration(&db, 9, STORED).await;
    let producer = managed_producer(broker.clone(), db.clone()).await;
    assert_eq!(
        producer
            .partition_producer(QUEUE, 9)
            .await
            .expect("the stored id resolves"),
        STORED,
        "the stored id is cached"
    );

    // The row is gone before the replace starts, so the replace finds nothing
    // to move off the rejected id.
    delete_registration(&db, 9).await;
    producer
        .replace_unknown_producer(QUEUE, 9, STORED)
        .await
        .expect("the replace ends without error");

    let resolved = producer
        .partition_producer(QUEUE, 9)
        .await
        .expect("the slot resolves after the row vanished");

    assert_eq!(
        resolved,
        registered(1),
        "the rejected id is not served again"
    );
    assert_eq!(broker.register_calls(), 1, "one fresh registration");
    assert_eq!(
        ProducerRegistrationStore::new(db)
            .load_all(KEY)
            .await
            .expect("the store reads"),
        vec![ProducerRegistration {
            slot: slot(9),
            producer_id: registered(1),
            mode: ProducerMode::Chained,
            client_agent: CLIENT_AGENT.to_owned(),
            generation: 1,
        }]
    );
}

#[tokio::test]
async fn an_unchanged_registration_does_not_swap_the_cell() {
    let (broker, _entered) = ScriptedBroker::new();
    let producer = managed_producer(broker.clone(), sqlite_db().await).await;
    let registration = ProducerRegistration {
        slot: slot(4),
        producer_id: STORED,
        mode: ProducerMode::Chained,
        client_agent: CLIENT_AGENT.to_owned(),
        generation: 1,
    };

    producer.cache_registration(registration.clone());
    let first = producer.registration_cell(&slot(4));
    producer.cache_registration(registration.clone());
    let second = producer.registration_cell(&slot(4));

    assert!(
        Arc::ptr_eq(&first, &second),
        "caching the same id keeps the slot's cell"
    );
    assert_eq!(second.get(), Some(&registration));
    assert_eq!(broker.register_calls(), 0, "nothing registers");
}
